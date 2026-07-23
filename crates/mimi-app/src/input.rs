//! Keyboard -> terminal byte encoding (xterm-compatible).

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Modifier code for `CSI 1;<code> X` style sequences.
fn mod_code(mods: ModifiersState) -> u8 {
    let mut code = 1;
    if mods.shift_key() {
        code += 1;
    }
    if mods.alt_key() {
        code += 2;
    }
    if mods.control_key() {
        code += 4;
    }
    code
}

/// Encode a key press into the bytes to write to the PTY.
/// `app_cursor` is DECCKM. Returns None for keys the app layer handles
/// (cmd shortcuts) or that produce nothing.
pub fn encode_key(event: &KeyEvent, mods: ModifiersState, app_cursor: bool) -> Option<Vec<u8>> {
    // cmd-anything is an app-level shortcut, never sent to the shell.
    if mods.super_key() {
        return None;
    }
    let mc = mod_code(mods);
    let plain = mc == 1;

    let seq = |plain_seq: &str, named: char| -> Vec<u8> {
        if plain {
            plain_seq.as_bytes().to_vec()
        } else {
            format!("\x1b[1;{mc}{named}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if plain {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{mc}~").into_bytes()
        }
    };

    match &event.logical_key {
        Key::Named(named) => {
            let arrows = |ch: char| {
                if plain && app_cursor {
                    format!("\x1bO{ch}").into_bytes()
                } else {
                    seq(&format!("\x1b[{ch}"), ch)
                }
            };
            let bytes = match named {
                NamedKey::Enter => {
                    if mods.alt_key() {
                        b"\x1b\r".to_vec()
                    } else {
                        b"\r".to_vec()
                    }
                }
                NamedKey::Backspace => {
                    if mods.alt_key() {
                        b"\x1b\x7f".to_vec()
                    } else {
                        b"\x7f".to_vec()
                    }
                }
                NamedKey::Tab => {
                    if mods.shift_key() {
                        b"\x1b[Z".to_vec()
                    } else {
                        b"\t".to_vec()
                    }
                }
                NamedKey::Escape => b"\x1b".to_vec(),
                NamedKey::Space => {
                    if mods.control_key() {
                        vec![0]
                    } else {
                        b" ".to_vec()
                    }
                }
                NamedKey::ArrowUp => arrows('A'),
                NamedKey::ArrowDown => arrows('B'),
                NamedKey::ArrowRight => arrows('C'),
                NamedKey::ArrowLeft => arrows('D'),
                NamedKey::Home => arrows('H'),
                NamedKey::End => arrows('F'),
                NamedKey::PageUp => tilde(5),
                NamedKey::PageDown => tilde(6),
                NamedKey::Insert => tilde(2),
                NamedKey::Delete => tilde(3),
                NamedKey::F1 => seq("\x1bOP", 'P'),
                NamedKey::F2 => seq("\x1bOQ", 'Q'),
                NamedKey::F3 => seq("\x1bOR", 'R'),
                NamedKey::F4 => seq("\x1bOS", 'S'),
                NamedKey::F5 => tilde(15),
                NamedKey::F6 => tilde(17),
                NamedKey::F7 => tilde(18),
                NamedKey::F8 => tilde(19),
                NamedKey::F9 => tilde(20),
                NamedKey::F10 => tilde(21),
                NamedKey::F11 => tilde(23),
                NamedKey::F12 => tilde(24),
                _ => return None,
            };
            Some(bytes)
        }
        Key::Character(s) => {
            let mut out = Vec::new();
            if mods.control_key() {
                // Ctrl+letter -> C0 byte.
                for c in s.chars() {
                    let b = match c.to_ascii_lowercase() {
                        'a'..='z' => c.to_ascii_lowercase() as u8 - b'a' + 1,
                        '@' | ' ' => 0,
                        '[' => 0x1b,
                        '\\' => 0x1c,
                        ']' => 0x1d,
                        '^' => 0x1e,
                        '_' | '/' => 0x1f,
                        _ => return Some(s.as_bytes().to_vec()),
                    };
                    out.push(b);
                }
            } else {
                if mods.alt_key() {
                    out.push(0x1b);
                }
                out.extend_from_slice(s.as_bytes());
            }
            Some(out)
        }
        _ => None,
    }
}

/// Wrap pasted text for the shell (bracketed paste when enabled).
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    // Normalize newlines to CR as terminals expect.
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let mut v = b"\x1b[200~".to_vec();
        v.extend_from_slice(text.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_encoding() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb".to_vec());
        let b = encode_paste("x", true);
        assert!(b.starts_with(b"\x1b[200~") && b.ends_with(b"\x1b[201~"));
    }
}
