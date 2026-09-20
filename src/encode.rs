//! Byte encodings for keys and mouse events delivered to a PTY.
use vt100::{MouseProtocolEncoding, MouseProtocolMode as MouseMode};

/// xterm mouse bytes for a pane-relative one-based cell, or `None` when the
/// application's protocol mode does not want this event.
#[expect(
    clippy::too_many_arguments,
    reason = "each mouse event field is an independent input"
)]
pub(crate) fn mouse_bytes(
    screen: &vt100::Screen,
    action: &str,
    button: u8,
    col: u16,
    row: u16,
    ctrl: bool,
    alt: bool,
    shift: bool,
) -> Option<Vec<u8>> {
    let mode = screen.mouse_protocol_mode();
    let release = action == "release";
    let motion = action == "move";
    if release && mode == MouseMode::Press
        || motion
            && (matches!(mode, MouseMode::Press | MouseMode::PressRelease)
                || mode == MouseMode::ButtonMotion && button == 3)
    {
        return None;
    }
    let mut code = if action == "scrollup" {
        64
    } else if action == "scrolldown" {
        65
    } else {
        u16::from(button.min(3))
    };
    if motion {
        code += 32;
    }
    if shift {
        code += 4;
    }
    if alt {
        code += 8;
    }
    if ctrl {
        code += 16;
    }
    let (rows, cols) = screen.size();
    if row > rows || col > cols {
        return None;
    }
    if screen.mouse_protocol_encoding() == MouseProtocolEncoding::Sgr {
        Some(
            format!(
                "\x1b[<{code};{col};{row}{}",
                if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        )
    } else if col <= 223 && row <= 223 {
        Some(vec![
            27,
            b'[',
            b'M',
            (if release { 3 } else { code }) as u8 + 32,
            col as u8 + 32,
            row as u8 + 32,
        ])
    } else {
        None
    }
}

pub(crate) fn key_bytes(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    application: bool,
) -> Result<Vec<u8>, String> {
    let modifier = 1 + usize::from(shift) + 2 * usize::from(alt) + 4 * usize::from(ctrl);
    let csi = |code, final_byte| {
        if modifier > 1 {
            format!("\x1b[{code};{modifier}{final_byte}")
        } else {
            format!("\x1b[{code}{final_byte}")
        }
        .into_bytes()
    };
    let function = key.strip_prefix('f').and_then(|n| n.parse::<usize>().ok());
    let cursor = match key {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => function
            .filter(|n| (1..=4).contains(n))
            .map(|n| char::from(b'P' + n as u8 - 1)),
    };
    if let Some(final_byte) = cursor {
        return Ok(if modifier > 1 {
            csi(1, final_byte)
        } else {
            let prefix = if application || function.is_some() {
                'O'
            } else {
                '['
            };
            format!("\x1b{prefix}{final_byte}").into_bytes()
        });
    }
    let mut bytes = match key {
        "enter" => vec![13],
        "tab" if shift => b"\x1b[Z".to_vec(),
        "tab" => vec![9],
        "escape" => vec![27],
        "backspace" => vec![127],
        "insert" | "delete" | "pageup" | "pagedown" => {
            let code = match key {
                "insert" => 2,
                "delete" => 3,
                "pageup" => 5,
                _ => 6,
            };
            csi(code, '~')
        }
        _ if let Some(n) = function => {
            let codes = [15, 17, 18, 19, 20, 21, 23, 24];
            let code = codes
                .get(n.wrapping_sub(5))
                .ok_or("unsupported function key")?;
            csi(*code, '~')
        }
        _ if key.chars().count() == 1 => {
            let c = key.chars().next().ok_or("empty key")?;
            if ctrl && c.is_ascii() {
                vec![(c.to_ascii_uppercase() as u8) & 0x1f]
            } else {
                key.as_bytes().to_vec()
            }
        }
        _ => return Err(format!("unsupported key {key}")),
    };
    if alt && !bytes.starts_with(&[27]) {
        bytes.insert(0, 27);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    #[test]
    fn modified_keys_preserve_xterm_protocol_semantics() -> crate::testing::Outcome {
        for (key, plain, modified) in [
            ("f1", "\x1bOP", "\x1b[1;8P"),
            ("f4", "\x1bOS", "\x1b[1;8S"),
            ("f5", "\x1b[15~", "\x1b[15;8~"),
            ("insert", "\x1b[2~", "\x1b[2;8~"),
            ("home", "\x1b[H", "\x1b[1;8H"),
        ] {
            assert_eq!(
                key_bytes(key, false, false, false, false).need()?,
                plain.as_bytes()
            );
            assert_eq!(
                key_bytes(key, true, true, true, false).need()?,
                modified.as_bytes()
            );
        }
        for key in ["f0", "f13", "f999", "fno", ""] {
            assert!(key_bytes(key, false, false, false, false).is_err());
        }
        assert_eq!(
            key_bytes("left", false, false, false, true).need()?,
            b"\x1bOD"
        );
        assert_eq!(
            key_bytes("left", true, false, false, true).need()?,
            b"\x1b[1;5D"
        );
        assert_eq!(
            key_bytes("f1", true, false, true, false).need()?,
            b"\x1b[1;6P"
        );
        assert_eq!(
            key_bytes("f12", false, true, false, false).need()?,
            b"\x1b[24;3~"
        );
        Ok(())
    }
}
