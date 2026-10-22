//! xterm's mouse modes 1001 (highlight tracking), 1007 (alternate scroll),
//! 1015 (urxvt) and 1016 (SGR with pixels), as ctlseqs ("Mouse Tracking")
//! names them and xterm 411 sends them (button.c `EditorButton` and
//! `EmitMousePosition`, scrollbar.c `AlternateScroll`, charproc.c's
//! default translations): each kept, reported by DECRQM, and encoded.
//! The expected bytes are those functions'.

use fux_vt::keys::Modifiers;
use fux_vt::keys::mouse::{MouseAction, MouseButton, MouseEvent};
use fux_vt::{
    Feature, Graphics, Mode, MouseProtocolEncoding, MouseProtocolMode, Options, Parser, Size,
};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn parser(input: &[u8]) -> Result<Parser> {
    let mut p = Parser::with_options(
        Size::new(24, 80)?,
        0,
        Options::new().with(Feature::ModeReports),
    )?;
    p.process(input)?;
    Ok(p)
}

fn event(action: MouseAction, row: u16, col: u16) -> MouseEvent {
    MouseEvent {
        action,
        mods: Modifiers::NONE,
        row,
        col,
    }
}

/// What the screen reports for `event`, `None` for nothing.
fn sent(p: &Parser, event: MouseEvent) -> Option<String> {
    let mut out = Vec::new();
    p.screen()
        .encode_mouse(event, &mut out)
        .then(|| String::from_utf8_lossy(&out).replace('\x1b', "^["))
}

/// DECRQM answers each set while it is, and the latest encoding wins
/// (xterm 411's `extend_coords`): 1006 then 1015 is urxvt, and resetting
/// 1006 then leaves it so.
#[test]
fn each_mode_is_kept_and_reported() -> Result {
    for n in [1001, 1007, 1015, 1016] {
        let mut p = parser(format!("\x1b[?{n}h").as_bytes())?;
        let mut out = Vec::new();
        p.process_with_replies(format!("\x1b[?{n}$p").as_bytes(), |b| {
            out.extend_from_slice(b)
        })?;
        assert_eq!(out, format!("\x1b[?{n};1$y").into_bytes(), "{n}");
        let mut out = Vec::new();
        p.process_with_replies(format!("\x1b[?{n}l\x1b[?{n}$p").as_bytes(), |b| {
            out.extend_from_slice(b)
        })?;
        assert_eq!(out, format!("\x1b[?{n};2$y").into_bytes(), "{n}");
    }
    let p = parser(b"\x1b[?1006h\x1b[?1015h\x1b[?1006l")?;
    assert_eq!(
        p.screen().mouse_protocol_encoding(),
        MouseProtocolEncoding::Urxvt
    );
    assert!(p.screen().mode(Mode::MouseUrxvt));
    let p = parser(b"\x1b[?1000h\x1b[?1001h")?;
    assert_eq!(
        p.screen().mouse_protocol_mode(),
        MouseProtocolMode::Highlight
    );
    assert!(!p.screen().mode(Mode::MousePressRelease));
    Ok(())
}

/// urxvt (1015): `CSI code ; column ; row M`, the code plus 32 as the
/// default encoding has it, a release unnamed (3), no limit on the
/// position.
#[test]
fn urxvt_sends_the_code_plus_32_and_decimal_positions() -> Result {
    let p = parser(b"\x1b[?1002h\x1b[?1015h")?;
    let left = MouseButton::Left;
    assert_eq!(
        sent(&p, event(MouseAction::Press(left), 0, 0)).as_deref(),
        Some("^[[32;1;1M")
    );
    assert_eq!(
        sent(&p, event(MouseAction::Release(Some(left)), 4, 2999)).as_deref(),
        Some("^[[35;3000;5M")
    );
    assert_eq!(
        sent(&p, event(MouseAction::Motion(Some(left)), 1, 1)).as_deref(),
        Some("^[[64;2;2M")
    );
    assert_eq!(
        sent(&p, event(MouseAction::Press(MouseButton::WheelUp), 9, 9)).as_deref(),
        Some("^[[96;10;10M")
    );
    Ok(())
}

/// SGR with pixels (1016): SGR's form, the pointer's pixel from one: the
/// middle of the cell, in the host's cell size, or 10 by 20.
#[test]
fn sgr_pixels_reports_the_pointers_pixel() -> Result {
    let mut p = parser(b"\x1b[?1000h\x1b[?1016h")?;
    let press = event(MouseAction::Press(MouseButton::Left), 1, 2);
    let release = event(MouseAction::Release(Some(MouseButton::Left)), 1, 2);
    assert_eq!(sent(&p, press).as_deref(), Some("^[[<0;26;31M"));
    assert_eq!(sent(&p, release).as_deref(), Some("^[[<0;26;31m"));
    p.set_host_graphics(Graphics {
        cell: Some((8, 16)),
        sixel: false,
    });
    assert_eq!(sent(&p, press).as_deref(), Some("^[[<0;21;25M"));
    Ok(())
}

/// Highlight tracking (1001) reports as normal tracking (1000): presses
/// and releases, no motion. Its highlight phase is the host's.
#[test]
fn highlight_tracking_reports_as_normal_tracking() -> Result {
    let p = parser(b"\x1b[?1001h\x1b[?1006h")?;
    let left = MouseButton::Left;
    assert_eq!(
        sent(&p, event(MouseAction::Press(left), 0, 0)).as_deref(),
        Some("^[[<0;1;1M")
    );
    assert_eq!(
        sent(&p, event(MouseAction::Release(Some(left)), 0, 0)).as_deref(),
        Some("^[[<0;1;1m")
    );
    assert_eq!(sent(&p, event(MouseAction::Motion(Some(left)), 0, 1)), None);
    Ok(())
}

/// Alternate scroll (1007): with no mouse reporting, on the alternate
/// screen, a wheel turn sends cursor up or down, in the cursor key mode,
/// 5 times (xterm 411's `scroll-back(5,line,m)`), half the screen's rows
/// with Ctrl (`scroll-back(1,halfpage,m)`); on the main screen, or with
/// mouse reporting asked for, it does not.
#[test]
fn alternate_scroll_sends_the_cursor_keys_on_the_alternate_screen() -> Result {
    let up = event(MouseAction::Press(MouseButton::WheelUp), 3, 3);
    let down = event(MouseAction::Press(MouseButton::WheelDown), 3, 3);
    let p = parser(b"\x1b[?1007h")?;
    assert_eq!(sent(&p, up), None);
    let p = parser(b"\x1b[?1007h\x1b[?1049h")?;
    assert_eq!(sent(&p, up).as_deref(), Some("^[[A^[[A^[[A^[[A^[[A"));
    assert_eq!(sent(&p, down).as_deref(), Some("^[[B^[[B^[[B^[[B^[[B"));
    let ctrl = MouseEvent {
        mods: Modifiers::CTRL,
        ..down
    };
    assert_eq!(
        sent(&p, ctrl),
        Some(std::iter::repeat_n("^[[B", 12).collect::<String>())
    );
    let p = parser(b"\x1b[?1007h\x1b[?1049h\x1b[?1h")?;
    assert_eq!(sent(&p, up).as_deref(), Some("^[OA^[OA^[OA^[OA^[OA"));
    let p = parser(b"\x1b[?1049h")?;
    assert_eq!(sent(&p, up), None);
    let p = parser(b"\x1b[?1007h\x1b[?1049h\x1b[?1000h")?;
    assert_eq!(sent(&p, up).as_deref(), Some("^[[M`$$"));
    Ok(())
}
