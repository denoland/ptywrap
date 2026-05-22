/// Interpret C/shell-style escape sequences in a string.
///
/// Supports:
///   - Control characters: \n \r \t \a \b \f \v \e \E \\
///   - Null byte:          \0
///   - Hex byte:           \xHH (1 or 2 hex digits, single raw byte)
///   - 4-digit Unicode:    \uHHHH (encoded as UTF-8)
///   - 8-digit Unicode:    \UHHHHHHHH (encoded as UTF-8)
///
/// An unrecognized backslash sequence is emitted literally (the backslash
/// is kept, and the next character is processed normally next iteration).
pub fn interpret_escapes(s: &str) -> Vec<u8> {
    let mut result = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => {
                    result.push(b'\n');
                    i += 2;
                }
                b'r' => {
                    result.push(b'\r');
                    i += 2;
                }
                b't' => {
                    result.push(b'\t');
                    i += 2;
                }
                b'\\' => {
                    result.push(b'\\');
                    i += 2;
                }
                b'e' | b'E' => {
                    result.push(0x1b);
                    i += 2;
                }
                b'0' => {
                    result.push(0);
                    i += 2;
                }
                b'a' => {
                    result.push(0x07);
                    i += 2;
                }
                b'b' => {
                    result.push(0x08);
                    i += 2;
                }
                b'f' => {
                    result.push(0x0c);
                    i += 2;
                }
                b'v' => {
                    result.push(0x0b);
                    i += 2;
                }
                b'x' => {
                    i += 2;
                    let mut hex = String::new();
                    while hex.len() < 2
                        && i < bytes.len()
                        && (bytes[i] as char).is_ascii_hexdigit()
                    {
                        hex.push(bytes[i] as char);
                        i += 1;
                    }
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        result.push(byte);
                    }
                }
                b'u' => {
                    i += 2;
                    consume_unicode(bytes, &mut i, 4, &mut result);
                }
                b'U' => {
                    i += 2;
                    consume_unicode(bytes, &mut i, 8, &mut result);
                }
                _ => {
                    result.push(b'\\');
                    i += 1;
                }
            }
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    result
}

/// Read up to `max` hex digits from `bytes` starting at `*i` and, if any
/// were read, decode them as a Unicode codepoint and append its UTF-8
/// bytes to `out`. Invalid codepoints (e.g. surrogates) are silently
/// skipped.
fn consume_unicode(bytes: &[u8], i: &mut usize, max: usize, out: &mut Vec<u8>) {
    let mut hex = String::new();
    while hex.len() < max && *i < bytes.len() && (bytes[*i] as char).is_ascii_hexdigit() {
        hex.push(bytes[*i] as char);
        *i += 1;
    }
    if hex.is_empty() {
        return;
    }
    if let Ok(cp) = u32::from_str_radix(&hex, 16)
        && let Some(c) = char::from_u32(cp)
    {
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        out.extend_from_slice(s.as_bytes());
    }
}

/// Convert a named key to its terminal escape sequence bytes.
pub fn key_to_bytes(name: &str) -> Option<Vec<u8>> {
    let lower = name.to_lowercase();
    match lower.as_str() {
        "enter" | "return" | "cr" => Some(vec![b'\r']),
        "tab" => Some(vec![b'\t']),
        "escape" | "esc" => Some(vec![0x1b]),
        "space" => Some(vec![b' ']),
        "backspace" | "bs" => Some(vec![0x7f]),
        "delete" | "del" => Some(b"\x1b[3~".to_vec()),
        "up" => Some(b"\x1b[A".to_vec()),
        "down" => Some(b"\x1b[B".to_vec()),
        "right" => Some(b"\x1b[C".to_vec()),
        "left" => Some(b"\x1b[D".to_vec()),
        "home" => Some(b"\x1b[H".to_vec()),
        "end" => Some(b"\x1b[F".to_vec()),
        "pageup" | "pgup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" | "pgdn" => Some(b"\x1b[6~".to_vec()),
        "insert" | "ins" => Some(b"\x1b[2~".to_vec()),
        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => Some(b"\x1b[15~".to_vec()),
        "f6" => Some(b"\x1b[17~".to_vec()),
        "f7" => Some(b"\x1b[18~".to_vec()),
        "f8" => Some(b"\x1b[19~".to_vec()),
        "f9" => Some(b"\x1b[20~".to_vec()),
        "f10" => Some(b"\x1b[21~".to_vec()),
        "f11" => Some(b"\x1b[23~".to_vec()),
        "f12" => Some(b"\x1b[24~".to_vec()),
        _ => {
            // ctrl-X keys
            let stripped = lower
                .strip_prefix("ctrl-")
                .or_else(|| lower.strip_prefix("c-"));
            if let Some(ch) = stripped
                && ch.len() == 1
            {
                let c = ch.as_bytes()[0];
                if c.is_ascii_lowercase() {
                    return Some(vec![c - b'a' + 1]);
                }
            }
            None
        }
    }
}
