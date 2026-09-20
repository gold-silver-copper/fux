use super::*;
use base64::Engine;

fn mouse(s: &Server, viewer: u64, action: &str, x: u16, y: u16, shift: bool) -> Result<(), String> {
    s.input(viewer, json!({"kind":"mouse","action":action,"button":0,"x":x,"y":y,"ctrl":false,"alt":false,"shift":shift}))
}

#[test]
fn shift_drag_and_keyboard_copy_mode_do_not_leak_application_mouse_bytes() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    let input = s.directory.join("mouse-selection.bin");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HREADY\033[?1003h\033[?1006h'; cat > '{}'",
            input.display()
        ),
    )?;
    eventually(|| Ok(s.screen(v)?.starts_with("READY")))?;
    mouse(&s, v, "press", 0, 0, true)?;
    mouse(&s, v, "move", 2, 0, true)?;
    mouse(&s, v, "release", 2, 0, true)?;
    s.key(v, "y", false)?;
    assert_eq!(copied(&s, v)?, "REA");
    s.control(v, "copy_mode", "")?;
    mouse(&s, v, "move", 3, 0, false)?;
    mouse(&s, v, "scrollup", 0, 0, false)?;
    s.key(v, "q", false)?;
    mouse(&s, v, "press", 4, 0, false)?;
    eventually(|| Ok(fs::read(&input).is_ok_and(|bytes| bytes == b"\x1b[<0;5;1M")))?;
    Ok(())
}

#[test]
fn evicted_history_selection_is_cleared_without_moving_another_viewer() -> Outcome {
    let s = Server::start()?;
    let a = s.attach()?;
    let b = s.attach()?;
    s.screen(a)?;
    s.screen(b)?;
    let signal = s.directory.join("evict");
    s.run(a, &format!("exec /bin/sh -c 'i=0; while test $i -lt 120; do printf \"OLD-%s\\n\" $i; i=$((i+1)); done; while test ! -f {}; do sleep 0.02; done; i=0; while test $i -lt 200; do printf \"NEW-%s\\n\" $i; i=$((i+1)); done; exec sleep 60'", signal.display()))?;
    eventually(|| Ok(s.screen(a)?.contains("OLD-119")))?;
    s.control(a, "copy_mode", "")?;
    for _ in 0..8 {
        s.key(a, "pageup", false)?;
    }
    s.key(a, " ", false)?;
    s.key(a, "right", false)?;
    let old = s.viewer(a)?.at("scrollback").as_u64().need()?;
    assert!(old > 0);
    fs::write(signal, "go").need()?;
    eventually(|| {
        if s.viewer(a)?
            .at("notice")
            .as_str()
            .is_some_and(|n| n.contains("selection cleared"))
        {
            return Ok(true);
        }
        s.screen(a)?;
        Ok(false)
    })?;
    assert_eq!(s.viewer(b)?.at("scrollback"), 0);
    s.key(a, "y", false)?;
    assert!(
        !s.rpc("fux.frame", json!({"viewer":a}))?
            .at("paint")
            .as_str()
            .need()?
            .contains("\x1b]52;")
    );
    s.key(a, "g", false)?;
    assert_eq!(s.viewer(a)?.at("scrollback"), 0);
    Ok(())
}

fn copied(s: &Server, viewer: u64) -> Result<String, Fail> {
    let frame = s.rpc("fux.frame", json!({"viewer":viewer}))?;
    let paint = frame.at("paint");
    let paint = paint.as_str().need()?;
    let encoded = paint
        .split("\x1b]52;c;")
        .nth(1)
        .need()?
        .split('\x07')
        .next()
        .need()?;
    Ok(String::from_utf8(
        base64::engine::general_purpose::STANDARD.decode(encoded)?,
    )?)
}

#[test]
fn keyboard_and_drag_selection_copy_unicode_without_touching_other_viewers() -> Outcome {
    let s = Server::start()?;
    let a = s.attach()?;
    let b = s.attach()?;
    s.screen(a)?;
    s.screen(b)?;
    s.run(
        a,
        "exec /bin/sh -c 'printf \"\\033[2J\\033[HA界éZ\"; exec sleep 60'",
    )?;
    eventually(|| Ok(s.screen(a)?.starts_with("A界éZ")))?;
    s.control(a, "copy_mode", "")?;
    s.key(a, "right", false)?;
    s.key(a, " ", false)?;
    s.key(a, "right", false)?;
    let screen = s.painted(a, 24, 80)?;
    assert!(screen.cell(0, 1).need()?.inverse());
    assert!(screen.cell(0, 3).need()?.inverse());
    assert!(screen.hide_cursor());
    s.capture(a, 24, 80, "interaction-selection")?;
    assert!(!s.painted(b, 24, 80)?.cell(0, 1).need()?.inverse());
    s.control(b, "copy_mode", "")?;
    s.key(b, " ", false)?;
    s.key(a, "y", false)?;
    assert_eq!(copied(&s, a)?, "界é");
    assert!(s.painted(b, 24, 80)?.cell(0, 0).need()?.inverse());
    s.key(b, "y", false)?;
    assert_eq!(copied(&s, b)?, "A");
    assert!(!s.painted(a, 24, 80)?.cell(0, 1).need()?.inverse());
    s.mouse(a, "press", 0, 0)?;
    s.mouse(a, "move", 3, 0)?;
    s.mouse(a, "release", 3, 0)?;
    s.key(a, "enter", false)?;
    assert_eq!(copied(&s, a)?, "A界é");
    assert_eq!(s.viewer(b)?.at("scrollback"), 0);
    Ok(())
}

#[test]
fn selection_invalidates_visibly_on_output_and_resize_and_paste_is_modal() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    let signal = s.directory.join("change");
    s.run(v, &format!("exec /bin/sh -c 'printf \"\\033[2J\\033[HBEFORE\"; while test ! -f {}; do sleep 0.02; done; printf \"\\033[HAFTER!\"; exec sleep 60'", signal.display()))?;
    eventually(|| Ok(s.screen(v)?.starts_with("BEFORE")))?;
    s.control(v, "copy_mode", "")?;
    s.key(v, " ", false)?;
    s.key(v, "right", false)?;
    s.input(v, json!({"kind":"paste","text":"NOT-PTY-INPUT"}))?;
    fs::write(signal, "change").need()?;
    eventually(|| {
        Ok(s.painted(v, 24, 80)?
            .contents()
            .contains("selection cleared"))
    })?;
    assert!(!s.screen(v)?.contains("NOT-PTY-INPUT"));
    s.key(v, "y", false)?;
    let frame = s.rpc("fux.frame", json!({"viewer":v}))?;
    assert!(!frame.at("paint").as_str().need()?.contains("\x1b]52;"));
    s.key(v, " ", false)?;
    s.key(v, "right", false)?;
    s.resize(v, 16, 50)?;
    assert!(
        s.painted(v, 16, 50)?
            .contents()
            .contains("selection cleared")
    );
    s.key(v, "escape", false)?;
    s.capture(v, 16, 50, "interaction-selection-invalidated")?;
    Ok(())
}

#[test]
fn clipboard_disabled_reports_failure_without_emitting_an_effect() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    fs::write(s.directory.join("fux.json"), r#"{"clipboard":"disabled"}"#).need()?;
    eventually(|| {
        s.control(v, "copy", "")?;
        Ok(s.viewer(v)?
            .at("notice")
            .as_str()
            .need()?
            .contains("clipboard disabled"))
    })?;
    let frame = s.rpc("fux.frame", json!({"viewer":v}))?;
    // Any effects requested before hot reload settled are drained first.
    assert!(frame.at("paint").as_str().need()?.contains("clipboard"));
    s.control(v, "copy", "")?;
    assert!(
        !s.rpc("fux.frame", json!({"viewer":v}))?
            .at("paint")
            .as_str()
            .need()?
            .contains("\x1b]52;")
    );
    Ok(())
}
