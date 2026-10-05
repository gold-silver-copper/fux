use super::*;
use crate::keys::decode::{Decoder, Input};

const NONE: Modifiers = Modifiers::NONE;

fn at(action: MouseAction, button: Option<MouseButton>, row: u16, col: u16) -> MouseEvent {
    MouseEvent {
        action,
        button,
        mods: NONE,
        row,
        col,
    }
}

fn press(button: MouseButton, row: u16, col: u16) -> MouseEvent {
    at(MouseAction::Press, Some(button), row, col)
}

fn release(button: MouseButton, row: u16, col: u16) -> MouseEvent {
    at(MouseAction::Release, Some(button), row, col)
}

fn motion(button: Option<MouseButton>, row: u16, col: u16) -> MouseEvent {
    at(MouseAction::Motion, button, row, col)
}

/// What `event` reports in `mode` and `encoding`; `None` for nothing.
fn report(
    event: MouseEvent,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let sent = mouse_bytes(event, mode, encoding, &mut out);
    assert_eq!(sent, !out.is_empty(), "{event:?} {mode:?} {encoding:?}");
    sent.then_some(out)
}

fn default(event: MouseEvent, mode: MouseProtocolMode) -> Option<Vec<u8>> {
    report(event, mode, MouseProtocolEncoding::Default)
}

fn sgr(event: MouseEvent, mode: MouseProtocolMode) -> Option<Vec<u8>> {
    report(event, mode, MouseProtocolEncoding::Sgr)
}

fn bytes(s: &[u8]) -> Option<Vec<u8>> {
    Some(s.to_vec())
}

const MODES: [MouseProtocolMode; 5] = [
    MouseProtocolMode::None,
    MouseProtocolMode::Press,
    MouseProtocolMode::PressRelease,
    MouseProtocolMode::ButtonMotion,
    MouseProtocolMode::AnyMotion,
];

const ENCODINGS: [MouseProtocolEncoding; 3] = [
    MouseProtocolEncoding::Default,
    MouseProtocolEncoding::Utf8,
    MouseProtocolEncoding::Sgr,
];

const BUTTONS: [MouseButton; 9] = [
    MouseButton::Left,
    MouseButton::Middle,
    MouseButton::Right,
    MouseButton::WheelUp,
    MouseButton::WheelDown,
    MouseButton::WheelLeft,
    MouseButton::WheelRight,
    MouseButton::Back,
    MouseButton::Forward,
];

#[test]
fn with_no_mode_nothing_is_reported() {
    for encoding in ENCODINGS {
        for button in BUTTONS {
            for event in [press(button, 0, 0), release(button, 0, 0)] {
                assert_eq!(report(event, MouseProtocolMode::None, encoding), None);
            }
        }
        assert_eq!(
            report(motion(None, 0, 0), MouseProtocolMode::None, encoding),
            None
        );
    }
}

#[test]
fn x10_reports_presses_alone_without_modifiers() {
    // ctlseqs, X10 compatibility mode: "Cb is button−1, where button is 1,
    // 2 or 3", no modifiers, no release.
    let mode = MouseProtocolMode::Press;
    assert_eq!(
        default(press(MouseButton::Left, 0, 0), mode),
        bytes(b"\x1b[M !!")
    );
    assert_eq!(
        default(press(MouseButton::Right, 2, 4), mode),
        bytes(b"\x1b[M\"%#")
    );
    let held = MouseEvent {
        mods: Modifiers {
            ctrl: true,
            alt: true,
            shift: true,
        },
        ..press(MouseButton::Middle, 0, 0)
    };
    assert_eq!(default(held, mode), bytes(b"\x1b[M!!!"));
    assert_eq!(default(release(MouseButton::Left, 0, 0), mode), None);
    assert_eq!(default(motion(Some(MouseButton::Left), 0, 0), mode), None);
    // Buttons 1 to 3 alone.
    for button in BUTTONS.into_iter().skip(3) {
        assert_eq!(default(press(button, 0, 0), mode), None, "{button:?}");
    }
    assert_eq!(
        sgr(press(MouseButton::Left, 0, 0), mode),
        bytes(b"\x1b[<0;1;1M")
    );
}

#[test]
fn normal_tracking_reports_presses_releases_and_the_wheel_with_modifiers() {
    let mode = MouseProtocolMode::PressRelease;
    assert_eq!(
        default(press(MouseButton::Left, 0, 0), mode),
        bytes(b"\x1b[M !!")
    );
    // A release is 3 in the default encoding, whichever button.
    assert_eq!(
        default(release(MouseButton::Right, 0, 0), mode),
        bytes(b"\x1b[M#!!")
    );
    assert_eq!(
        default(at(MouseAction::Release, None, 0, 0), mode),
        bytes(b"\x1b[M#!!")
    );
    // The wheel: 64 and 65 (plus 32: '`' and 'a'), never released.
    assert_eq!(
        default(press(MouseButton::WheelUp, 0, 0), mode),
        bytes(b"\x1b[M`!!")
    );
    assert_eq!(
        default(press(MouseButton::WheelDown, 0, 0), mode),
        bytes(b"\x1b[Ma!!")
    );
    assert_eq!(default(release(MouseButton::WheelUp, 0, 0), mode), None);
    // Shift 4, Meta 8, Control 16.
    for (mods, code) in [
        (
            Modifiers {
                shift: true,
                ..NONE
            },
            4u8,
        ),
        (Modifiers { alt: true, ..NONE }, 8),
        (Modifiers { ctrl: true, ..NONE }, 16),
        (
            Modifiers {
                ctrl: true,
                alt: true,
                shift: true,
            },
            28,
        ),
    ] {
        let event = MouseEvent {
            mods,
            ..press(MouseButton::Middle, 0, 0)
        };
        assert_eq!(
            default(event, mode),
            Some(vec![0x1b, b'[', b'M', 33 + code, 33, 33])
        );
    }
    assert_eq!(default(motion(Some(MouseButton::Left), 0, 0), mode), None);
    assert_eq!(default(motion(None, 0, 0), mode), None);
}

#[test]
fn button_event_tracking_reports_motion_while_a_button_is_held() {
    let mode = MouseProtocolMode::ButtonMotion;
    // Motion adds 32: left held is 32, plus 32 is '@'.
    assert_eq!(
        default(motion(Some(MouseButton::Left), 0, 1), mode),
        bytes(b"\x1b[M@\"!")
    );
    assert_eq!(
        sgr(motion(Some(MouseButton::Right), 0, 1), mode),
        bytes(b"\x1b[<34;2;1M")
    );
    assert_eq!(default(motion(None, 0, 0), mode), None);
    assert_eq!(
        default(press(MouseButton::Left, 0, 0), mode),
        bytes(b"\x1b[M !!")
    );
    assert_eq!(
        default(release(MouseButton::Left, 0, 0), mode),
        bytes(b"\x1b[M#!!")
    );
}

#[test]
fn any_event_tracking_reports_motion_with_no_button_held() {
    let mode = MouseProtocolMode::AnyMotion;
    // No button is 3, plus 32 for motion: 35, 'C' in the default encoding.
    assert_eq!(default(motion(None, 1, 0), mode), bytes(b"\x1b[MC!\""));
    assert_eq!(sgr(motion(None, 1, 0), mode), bytes(b"\x1b[<35;1;2M"));
    assert_eq!(
        default(motion(Some(MouseButton::Middle), 0, 0), mode),
        bytes(b"\x1b[MA!!")
    );
}

#[test]
fn sgr_names_the_button_released_and_ends_a_release_with_m() {
    let mode = MouseProtocolMode::PressRelease;
    assert_eq!(
        sgr(press(MouseButton::Left, 4, 9), mode),
        bytes(b"\x1b[<0;10;5M")
    );
    assert_eq!(
        sgr(release(MouseButton::Left, 4, 9), mode),
        bytes(b"\x1b[<0;10;5m")
    );
    assert_eq!(
        sgr(release(MouseButton::Right, 0, 0), mode),
        bytes(b"\x1b[<2;1;1m")
    );
    assert_eq!(
        sgr(at(MouseAction::Release, None, 0, 0), mode),
        bytes(b"\x1b[<3;1;1m")
    );
    assert_eq!(
        sgr(press(MouseButton::WheelDown, 0, 0), mode),
        bytes(b"\x1b[<65;1;1M")
    );
    assert_eq!(
        sgr(press(MouseButton::WheelRight, 0, 0), mode),
        bytes(b"\x1b[<67;1;1M")
    );
    assert_eq!(
        sgr(press(MouseButton::Back, 0, 0), mode),
        bytes(b"\x1b[<128;1;1M")
    );
    assert_eq!(
        sgr(release(MouseButton::Forward, 0, 0), mode),
        bytes(b"\x1b[<129;1;1m")
    );
    let held = MouseEvent {
        mods: Modifiers {
            ctrl: true,
            shift: true,
            ..NONE
        },
        ..press(MouseButton::Right, 0, 0)
    };
    assert_eq!(sgr(held, mode), bytes(b"\x1b[<22;1;1M"));
    // Any position.
    assert_eq!(
        sgr(press(MouseButton::Left, u16::MAX, u16::MAX), mode),
        bytes(b"\x1b[<0;65536;65536M")
    );
}

#[test]
fn a_position_the_encoding_cannot_carry_sends_nothing() {
    let mode = MouseProtocolMode::PressRelease;
    // Default: 223 is the last (byte 255).
    assert_eq!(
        default(press(MouseButton::Left, 222, 222), mode),
        Some(vec![0x1b, b'[', b'M', 32, 255, 255])
    );
    let mut out = b"kept".to_vec();
    let event = press(MouseButton::Left, 0, 223);
    assert!(!mouse_bytes(
        event,
        mode,
        MouseProtocolEncoding::Default,
        &mut out
    ));
    assert!(!mouse_bytes(
        press(MouseButton::Left, 223, 0),
        mode,
        MouseProtocolEncoding::Default,
        &mut out
    ));
    assert_eq!(out, b"kept");
    // UTF-8: one byte below 96 (position 95 is 127), two after, 2015 last.
    let utf8 = |event| report(event, mode, MouseProtocolEncoding::Utf8);
    assert_eq!(
        utf8(press(MouseButton::Left, 0, 94)),
        bytes(b"\x1b[M \x7f!")
    );
    assert_eq!(
        utf8(press(MouseButton::Left, 0, 95)),
        bytes(b"\x1b[M \xc2\x80!")
    );
    assert_eq!(
        utf8(press(MouseButton::Left, 2014, 2014)),
        bytes(b"\x1b[M \xdf\xbf\xdf\xbf")
    );
    assert_eq!(utf8(press(MouseButton::Left, 0, 2015)), None);
    // A button code past 95 is two bytes too.
    assert_eq!(
        utf8(press(MouseButton::Back, 0, 0)),
        bytes(b"\x1b[M\xc2\xa0!!")
    );
}

#[test]
fn every_report_decodes_to_its_event() {
    for mode in MODES {
        for encoding in [MouseProtocolEncoding::Default, MouseProtocolEncoding::Sgr] {
            for button in BUTTONS.iter().copied().map(Some).chain([None]) {
                for action in [
                    MouseAction::Press,
                    MouseAction::Release,
                    MouseAction::Motion,
                ] {
                    for (shift, alt, ctrl) in [(false, false, false), (true, true, true)] {
                        let event = MouseEvent {
                            action,
                            button,
                            mods: Modifiers { ctrl, alt, shift },
                            row: 7,
                            col: 200,
                        };
                        let Some(sent) = report(event, mode, encoding) else {
                            continue;
                        };
                        let mut inputs = Vec::new();
                        Decoder::default().bytes(&sent, &mut inputs);
                        // What the report carries: X10 leaves out the
                        // modifiers; the default encoding, which button
                        // was released.
                        let mut expected = event;
                        if mode == MouseProtocolMode::Press {
                            expected.mods = NONE;
                        }
                        if encoding == MouseProtocolEncoding::Default
                            && action == MouseAction::Release
                        {
                            expected.button = None;
                        }
                        assert_eq!(
                            inputs,
                            vec![Input::Mouse(expected)],
                            "{event:?} {mode:?} {encoding:?} {sent:?}"
                        );
                    }
                }
            }
        }
    }
}
