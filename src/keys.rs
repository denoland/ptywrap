/// Interpret C-style escape sequences in a string.
/// Supports: \n \r \t \\ \e (ESC) \0 \a (BEL) \xNN (hex byte)
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
                b'e' => {
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
                b'x' => {
                    i += 2;
                    let mut hex = String::new();
                    if i < bytes.len() && (bytes[i] as char).is_ascii_hexdigit() {
                        hex.push(bytes[i] as char);
                        i += 1;
                    }
                    if i < bytes.len() && (bytes[i] as char).is_ascii_hexdigit() {
                        hex.push(bytes[i] as char);
                        i += 1;
                    }
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        result.push(byte);
                    }
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
            if let Some(ch) = stripped {
                if ch.len() == 1 {
                    let c = ch.as_bytes()[0];
                    if c.is_ascii_lowercase() {
                        return Some(vec![c - b'a' + 1]);
                    }
                }
            }
            None
        }
    }
}
