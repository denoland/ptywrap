/// Answer terminal queries on behalf of the (virtual) terminal.
///
/// Full-screen programs probe their terminal at startup -- cursor
/// position (DSR), device attributes (DA1), foreground/background
/// colors (OSC 10/11) -- and change behavior based on the replies.
/// A real terminal answers these on its input stream; ptywrap's
/// in-process emulator must do the same or programs assume a dumb
/// terminal (e.g. codex skips truecolor UI backgrounds when it cannot
/// learn the terminal's background color).
///
/// Deliberately NOT answered:
///   - kitty keyboard protocol probe (`ESC[?u`): replying would invite
///     the program to expect kitty-encoded key input, but `write` and
///     `send-key` synthesize legacy encodings. No reply = the standard
///     "not supported" signal (the program sees its DA1 answer arrive
///     without a `?u` answer).
///   - DA2/XTVERSION and other terminal-identity probes: pretending to
///     be a specific terminal program invites quirk-emulation paths.
pub struct QueryAnswerer {
    /// Unprocessed trailing bytes of the previous chunk, kept so a query
    /// split across two PTY reads is still recognized.
    tail: Vec<u8>,
    fg: String,
    bg: String,
}

/// Longest pattern we match; the rolling tail keeps this many minus one
/// bytes between scans.
const MAX_PAT: usize = 6;

/// Convert "#rrggbb" to the xterm OSC color spec "rrrr/gggg/bbbb"
/// (each 8-bit component widened to 16 bits by repetition).
fn hex_to_xterm_rgb(hex: &str) -> Option<String> {
    let h = hex.strip_prefix('#').unwrap_or(hex);
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let (r, g, b) = (&h[0..2], &h[2..4], &h[4..6]);
    Some(format!("{r}{r}/{g}{g}/{b}{b}").to_lowercase())
}

impl QueryAnswerer {
    /// `fg`/`bg` are "#rrggbb" colors reported for OSC 10/11 queries.
    pub fn new(fg: &str, bg: &str) -> anyhow::Result<Self> {
        let fg = hex_to_xterm_rgb(fg)
            .ok_or_else(|| anyhow::anyhow!("invalid color {:?}; expected #rrggbb", fg))?;
        let bg = hex_to_xterm_rgb(bg)
            .ok_or_else(|| anyhow::anyhow!("invalid color {:?}; expected #rrggbb", bg))?;
        Ok(Self {
            tail: Vec::new(),
            fg,
            bg,
        })
    }

    /// Scan a chunk of PTY output for queries and return the replies to
    /// write back to the PTY. `cursor` is the current (row, col),
    /// 0-based, of the virtual terminal.
    pub fn scan(&mut self, chunk: &[u8], cursor: (u16, u16)) -> Vec<u8> {
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(chunk);
        let old = buf.len() - chunk.len(); // bytes already scanned last time

        let mut replies = Vec::new();
        for (pat, reply) in [
            // DSR 6: report cursor position (1-based)
            (
                &b"\x1b[6n"[..],
                format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1),
            ),
            // DSR 5: report status (OK)
            (&b"\x1b[5n"[..], "\x1b[0n".to_string()),
            // DA1: VT220-class with ANSI color
            (&b"\x1b[c"[..], "\x1b[?62;22c".to_string()),
            (&b"\x1b[0c"[..], "\x1b[?62;22c".to_string()),
            // OSC 10/11: foreground/background color
            (&b"\x1b]10;?"[..], format!("\x1b]10;rgb:{}\x1b\\", self.fg)),
            (&b"\x1b]11;?"[..], format!("\x1b]11;rgb:{}\x1b\\", self.bg)),
        ] {
            let mut from = 0;
            while let Some(pos) = find(&buf[from..], pat) {
                let start = from + pos;
                // Only answer matches that include at least one new byte;
                // fully-old matches were answered in a previous scan.
                if start + pat.len() > old {
                    replies.extend_from_slice(reply.as_bytes());
                }
                from = start + pat.len();
            }
        }

        let keep = buf.len().min(MAX_PAT - 1);
        self.tail = buf[buf.len() - keep..].to_vec();
        replies
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answerer() -> QueryAnswerer {
        QueryAnswerer::new("#e0e0e0", "#191d27").unwrap()
    }

    #[test]
    fn answers_cursor_and_colors() {
        let mut q = answerer();
        let r = q.scan(b"\x1b[6n\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c", (4, 9));
        let s = String::from_utf8(r).unwrap();
        assert!(s.contains("\x1b[5;10R"));
        assert!(s.contains("\x1b]10;rgb:e0e0/e0e0/e0e0\x1b\\"));
        assert!(s.contains("\x1b]11;rgb:1919/1d1d/2727\x1b\\"));
        assert!(s.contains("\x1b[?62;22c"));
    }

    #[test]
    fn answers_query_split_across_chunks() {
        let mut q = answerer();
        assert!(q.scan(b"\x1b]1", (0, 0)).is_empty());
        let r = q.scan(b"1;?\x07", (0, 0));
        assert!(String::from_utf8(r).unwrap().contains("\x1b]11;rgb:"));
    }

    #[test]
    fn does_not_answer_twice() {
        let mut q = answerer();
        let first = q.scan(b"\x1b[6n", (0, 0));
        assert!(!first.is_empty());
        // The same bytes are now in the tail; an empty follow-up chunk
        // (or unrelated output) must not re-trigger a reply.
        assert!(q.scan(b"hello", (0, 0)).is_empty());
    }

    #[test]
    fn ignores_kitty_probe_and_da2() {
        let mut q = answerer();
        assert!(q.scan(b"\x1b[?u\x1b[>c\x1b[>0c", (0, 0)).is_empty());
    }

    #[test]
    fn rejects_bad_colors() {
        assert!(QueryAnswerer::new("red", "#191d27").is_err());
        assert!(QueryAnswerer::new("#e0e0e0", "#19").is_err());
    }
}
