//! Pure terminal keyboard & mouse input encoders:
//! - Standard xterm + DECCKM application cursor keys
//! - Kitty Keyboard Protocol (CSI u) & xterm `modifyOtherKeys`
//! - `Shift+Enter` newline handling across shells and modern TUI/AI CLIs
//! - SGR / X10 / UTF-8 mouse protocol encoding

use egui::{Key, Modifiers};

/// Active keyboard protocol state reported by the running terminal application.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyboardProtocol {
    /// Active Kitty Keyboard Protocol flags (`bit 0` = disambiguate escape codes).
    pub kitty_flags: u8,
    /// xterm `modifyOtherKeys` level (`0`, `1`, or `2`).
    pub modify_other_keys: u8,
    /// True when DECCKM (`ESC[?1h`) is active.
    pub app_cursor: bool,
    /// True when the underlying shell is `cmd.exe` (avoids sending ESC+CR on Shift+Enter).
    pub is_cmd: bool,
}

/// Compute xterm modifier parameter: `1 + shift + 2*alt + 4*ctrl`.
fn xterm_mod_code(modifiers: &Modifiers) -> u8 {
    let ctrl = modifiers.ctrl || modifiers.command;
    1 + (modifiers.shift as u8) + ((modifiers.alt as u8) * 2) + ((ctrl as u8) * 4)
}

/// Encode a pressed key into PTY bytes according to active terminal modes.
pub fn encode_key(
    key: Key,
    modifiers: &Modifiers,
    proto: KeyboardProtocol,
    has_text_event: bool,
) -> Option<Vec<u8>> {
    let ctrl = modifiers.ctrl || modifiers.command;
    let alt = modifiers.alt;
    let shift = modifiers.shift;
    let mod_code = xterm_mod_code(modifiers);
    let kitty_disambiguate = (proto.kitty_flags & 1) != 0;

    match key {
        Key::Enter => {
            if mod_code > 1 {
                if kitty_disambiguate {
                    return Some(format!("\x1b[13;{mod_code}u").into_bytes());
                }
                if proto.modify_other_keys >= 2 {
                    return Some(format!("\x1b[27;{mod_code};13~").into_bytes());
                }
                if alt && !ctrl {
                    return Some(b"\x1b\r".to_vec());
                }
                if shift && !ctrl && !alt && !proto.is_cmd {
                    // Widely accepted multi-line insert sequence in bash/zsh/PSReadLine/AI CLIs.
                    return Some(b"\x1b\r".to_vec());
                }
            }
            return Some(b"\r".to_vec());
        }
        Key::Tab => {
            if kitty_disambiguate && mod_code > 1 {
                return Some(format!("\x1b[9;{mod_code}u").into_bytes());
            }
            return Some(if shift && !ctrl && !alt {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            });
        }
        Key::Backspace => {
            if kitty_disambiguate && mod_code > 1 {
                return Some(format!("\x1b[127;{mod_code}u").into_bytes());
            }
            return Some(match (ctrl, alt) {
                (true, false) => vec![0x08],
                (false, true) => vec![0x1b, 0x7f],
                (true, true) => vec![0x1b, 0x08],
                (false, false) => vec![0x7f],
            });
        }
        Key::Escape => {
            if kitty_disambiguate {
                return Some(if mod_code > 1 {
                    format!("\x1b[27;{mod_code}u").into_bytes()
                } else {
                    b"\x1b[27u".to_vec()
                });
            }
            return Some(if alt { vec![0x1b, 0x1b] } else { vec![0x1b] });
        }
        Key::ArrowUp | Key::ArrowDown | Key::ArrowRight | Key::ArrowLeft | Key::Home | Key::End => {
            let final_ch = match key {
                Key::ArrowUp => 'A',
                Key::ArrowDown => 'B',
                Key::ArrowRight => 'C',
                Key::ArrowLeft => 'D',
                Key::Home => 'H',
                Key::End => 'F',
                _ => unreachable!(),
            };
            if mod_code > 1 {
                return Some(format!("\x1b[1;{mod_code}{final_ch}").into_bytes());
            }
            let prefix = if proto.app_cursor { "\x1bO" } else { "\x1b[" };
            return Some(format!("{prefix}{final_ch}").into_bytes());
        }
        Key::Insert | Key::Delete | Key::PageUp | Key::PageDown => {
            let num = match key {
                Key::Insert => 2,
                Key::Delete => 3,
                Key::PageUp => 5,
                Key::PageDown => 6,
                _ => unreachable!(),
            };
            if mod_code > 1 {
                return Some(format!("\x1b[{num};{mod_code}~").into_bytes());
            }
            return Some(format!("\x1b[{num}~").into_bytes());
        }
        Key::F1 | Key::F2 | Key::F3 | Key::F4 => {
            let final_ch = match key {
                Key::F1 => 'P',
                Key::F2 => 'Q',
                Key::F3 => 'R',
                Key::F4 => 'S',
                _ => unreachable!(),
            };
            if mod_code > 1 {
                return Some(format!("\x1b[1;{mod_code}{final_ch}").into_bytes());
            }
            return Some(format!("\x1bO{final_ch}").into_bytes());
        }
        Key::F5
        | Key::F6
        | Key::F7
        | Key::F8
        | Key::F9
        | Key::F10
        | Key::F11
        | Key::F12 => {
            let num = match key {
                Key::F5 => 15,
                Key::F6 => 17,
                Key::F7 => 18,
                Key::F8 => 19,
                Key::F9 => 20,
                Key::F10 => 21,
                Key::F11 => 23,
                Key::F12 => 24,
                _ => unreachable!(),
            };
            if mod_code > 1 {
                return Some(format!("\x1b[{num};{mod_code}~").into_bytes());
            }
            return Some(format!("\x1b[{num}~").into_bytes());
        }
        _ => {}
    }

    // Kitty CSI u encoding for modified printable keys (disambiguates Ctrl+I vs Tab, etc.).
    if kitty_disambiguate
        && (ctrl || alt)
        && !(ctrl && alt && has_text_event)
        && let Some(codepoint) = key_base_codepoint(key)
    {
        return Some(format!("\x1b[{codepoint};{mod_code}u").into_bytes());
    }

    // Legacy Ctrl control bytes (skip AltGr where Windows sets ctrl+alt alongside Event::Text).
    if ctrl && !(alt && has_text_event)
        && let Some(byte) = ctrl_byte(key, shift)
    {
        return Some(if alt { vec![0x1b, byte] } else { vec![byte] });
    }

    // Meta / Alt+<char> prefix sequences.
    if alt && !ctrl && !has_text_event
        && let Some(ch) = alt_ascii_char(key, shift)
    {
        return Some(vec![0x1b, ch]);
    }

    None
}

/// Encode a mouse press/release/motion/wheel event for the active mouse protocol encoding.
pub fn encode_mouse(
    encoding: vt100::MouseProtocolEncoding,
    button_code: u8,
    col: u16,
    row: u16,
    pressed: bool,
    modifiers: &Modifiers,
) -> Option<Vec<u8>> {
    let mut cb = button_code;
    if modifiers.shift {
        cb |= 4;
    }
    if modifiers.alt {
        cb |= 8;
    }
    if modifiers.ctrl || modifiers.command {
        cb |= 16;
    }
    let x = col + 1;
    let y = row + 1;
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let suffix = if pressed { 'M' } else { 'm' };
            Some(format!("\x1b[<{cb};{x};{y}{suffix}").into_bytes())
        }
        vt100::MouseProtocolEncoding::Default | vt100::MouseProtocolEncoding::Utf8 => {
            if x <= 223 && y <= 223 {
                let code = if pressed { cb } else { 3 | (cb & 0b11100) };
                Some(vec![
                    b'\x1b',
                    b'[',
                    b'M',
                    code + 32,
                    (x as u8) + 32,
                    (y as u8) + 32,
                ])
            } else {
                None
            }
        }
    }
}

fn key_base_codepoint(key: Key) -> Option<u32> {
    Some(match key {
        Key::Space => 32,
        Key::A => b'a' as u32,
        Key::B => b'b' as u32,
        Key::C => b'c' as u32,
        Key::D => b'd' as u32,
        Key::E => b'e' as u32,
        Key::F => b'f' as u32,
        Key::G => b'g' as u32,
        Key::H => b'h' as u32,
        Key::I => b'i' as u32,
        Key::J => b'j' as u32,
        Key::K => b'k' as u32,
        Key::L => b'l' as u32,
        Key::M => b'm' as u32,
        Key::N => b'n' as u32,
        Key::O => b'o' as u32,
        Key::P => b'p' as u32,
        Key::Q => b'q' as u32,
        Key::R => b'r' as u32,
        Key::S => b's' as u32,
        Key::T => b't' as u32,
        Key::U => b'u' as u32,
        Key::V => b'v' as u32,
        Key::W => b'w' as u32,
        Key::X => b'x' as u32,
        Key::Y => b'y' as u32,
        Key::Z => b'z' as u32,
        Key::Num0 => b'0' as u32,
        Key::Num1 => b'1' as u32,
        Key::Num2 => b'2' as u32,
        Key::Num3 => b'3' as u32,
        Key::Num4 => b'4' as u32,
        Key::Num5 => b'5' as u32,
        Key::Num6 => b'6' as u32,
        Key::Num7 => b'7' as u32,
        Key::Num8 => b'8' as u32,
        Key::Num9 => b'9' as u32,
        Key::OpenBracket => b'[' as u32,
        Key::CloseBracket => b']' as u32,
        Key::Backslash => b'\\' as u32,
        Key::Slash => b'/' as u32,
        Key::Minus => b'-' as u32,
        Key::Equals => b'=' as u32,
        Key::Period => b'.' as u32,
        Key::Comma => b',' as u32,
        Key::Semicolon => b';' as u32,
        Key::Quote => b'\'' as u32,
        Key::Backtick => b'`' as u32,
        _ => return None,
    })
}

fn ctrl_byte(key: Key, shift: bool) -> Option<u8> {
    Some(match key {
        Key::Space => 0x00,
        Key::A => 0x01,
        Key::B => 0x02,
        Key::C => 0x03,
        Key::D => 0x04,
        Key::E => 0x05,
        Key::F => 0x06,
        Key::G => 0x07,
        Key::H => 0x08,
        Key::I => 0x09,
        Key::J => 0x0A,
        Key::K => 0x0B,
        Key::L => 0x0C,
        Key::M => 0x0D,
        Key::N => 0x0E,
        Key::O => 0x0F,
        Key::P => 0x10,
        Key::Q => 0x11,
        Key::R => 0x12,
        Key::S => 0x13,
        Key::T => 0x14,
        Key::U => 0x15,
        Key::V => 0x16,
        Key::W => 0x17,
        Key::X => 0x18,
        Key::Y => 0x19,
        Key::Z => 0x1A,
        Key::OpenBracket => 0x1B,
        Key::Backslash => 0x1C,
        Key::CloseBracket => 0x1D,
        Key::Num6 if shift => 0x1E,
        Key::Minus | Key::Slash => 0x1F,
        _ => return None,
    })
}

fn alt_ascii_char(key: Key, shift: bool) -> Option<u8> {
    let base = match key {
        Key::A => b'a',
        Key::B => b'b',
        Key::C => b'c',
        Key::D => b'd',
        Key::E => b'e',
        Key::F => b'f',
        Key::G => b'g',
        Key::H => b'h',
        Key::I => b'i',
        Key::J => b'j',
        Key::K => b'k',
        Key::L => b'l',
        Key::M => b'm',
        Key::N => b'n',
        Key::O => b'o',
        Key::P => b'p',
        Key::Q => b'q',
        Key::R => b'r',
        Key::S => b's',
        Key::T => b't',
        Key::U => b'u',
        Key::V => b'v',
        Key::W => b'w',
        Key::X => b'x',
        Key::Y => b'y',
        Key::Z => b'z',
        Key::Num0 => return Some(if shift { b')' } else { b'0' }),
        Key::Num1 => return Some(if shift { b'!' } else { b'1' }),
        Key::Num2 => return Some(if shift { b'@' } else { b'2' }),
        Key::Num3 => return Some(if shift { b'#' } else { b'3' }),
        Key::Num4 => return Some(if shift { b'$' } else { b'4' }),
        Key::Num5 => return Some(if shift { b'%' } else { b'5' }),
        Key::Num6 => return Some(if shift { b'^' } else { b'6' }),
        Key::Num7 => return Some(if shift { b'&' } else { b'7' }),
        Key::Num8 => return Some(if shift { b'*' } else { b'8' }),
        Key::Num9 => return Some(if shift { b'(' } else { b'9' }),
        Key::Period => return Some(if shift { b'>' } else { b'.' }),
        Key::Comma => return Some(if shift { b'<' } else { b',' }),
        Key::Slash => return Some(if shift { b'?' } else { b'/' }),
        Key::Minus => return Some(if shift { b'_' } else { b'-' }),
        Key::Equals => return Some(if shift { b'+' } else { b'=' }),
        Key::Backtick => return Some(if shift { b'~' } else { b'`' }),
        Key::Space => return Some(b' '),
        _ => return None,
    };
    Some(if shift {
        base.to_ascii_uppercase()
    } else {
        base
    })
}
