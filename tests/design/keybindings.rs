use super::*;

#[test]
fn reserved_menu_keys_override_custom_bindings_and_enter_executes_selected_action() -> Outcome {
    let s = Server::start()?;
    let a = s.attach()?;
    let b = s.attach()?;
    s.screen(a)?;
    let before = s.query("bevy_ui::ui_node::Node")?;
    let focus = s.viewer(a)?.at("focus");
    fs::write(
        s.directory.join("fux.json"),
        serde_json::to_vec(&json!({
            "prefix":"ctrl-a", "bindings":[
                {"key":"down","action":"grow_width"},
                {"key":"right","action":"shrink_width"},
                {"key":"t","action":"tab_new"},
                {"key":"[","action":"tab_previous"}
            ]
        }))?,
    )?;
    s.control(a, "help", "")?;
    eventually(|| {
        Ok(s.painted(a, 24, 80)?
            .contents()
            .contains("down  grow width"))
    })?;
    s.key(a, "escape", false)?;
    s.key(a, "a", true)?;
    s.key(a, "down", false)?;
    assert_eq!(
        s.selected(a, 24, 80)?.as_deref(),
        Some("right  shrink width")
    );
    s.key(a, "right", false)?;
    assert_eq!(
        s.selected(a, 24, 80)?.as_deref(),
        Some("right  shrink width")
    );
    s.key(a, "down", false)?;
    assert_eq!(s.selected(a, 24, 80)?.as_deref(), Some("t  new tab"));
    assert!(!s.column_open(b, 24, 80)?);
    assert_eq!(s.viewer(a)?.at("focus"), focus);
    assert_eq!(s.query("bevy_ui::ui_node::Node")?, before);
    let screen = s.painted(a, 24, 80)?;
    let selected_row = (0..23)
        .find(|y| row(&screen, *y).contains("new tab"))
        .need()?;
    assert!((0..80).any(|x| screen.cell(selected_row, x).is_some_and(|c| c.inverse())));
    s.capture(a, 24, 80, "keybindings-selected")?;
    s.enter(a)?;
    assert_eq!(s.query("fux::model::Tab")?.rows().count(), 2);
    assert!(!s.column_open(a, 24, 80)?);
    assert_ne!(s.viewer(a)?.at("tab"), s.viewer(b)?.at("tab"));
    s.capture(a, 24, 80, "keybindings-new-tab")?;
    Ok(())
}

#[test]
fn a_navigation_key_prefix_still_forwards_its_literal_when_doubled() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    let input = s.directory.join("literal-navigation");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HREADY'; cat > '{}'",
            input.display()
        ),
    )?;
    eventually(|| Ok(s.screen(v)?.starts_with("READY")))?;
    fs::write(
        s.directory.join("fux.json"),
        serde_json::to_vec(&json!({
            "prefix":"up", "bindings":[{"key":"up","action":"terminate"}]
        }))?,
    )?;
    s.control(v, "help", "")?;
    eventually(|| {
        Ok(s.painted(v, 24, 80)?
            .contents()
            .contains("up  terminate process"))
    })?;
    s.key(v, "escape", false)?;
    s.key(v, "up", false)?;
    assert!(s.column_open(v, 24, 80)?);
    s.key(v, "up", false)?;
    assert!(!s.column_open(v, 24, 80)?);
    eventually(|| Ok(fs::read(&input).is_ok_and(|b| b == b"\x1b[A")))?;
    Ok(())
}

#[test]
fn unavailable_selected_command_reports_reason_and_copy_escape_exits_once() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    fs::write(
        s.directory.join("fux.json"),
        serde_json::to_vec(&json!({
            "bindings":[{"key":"[","action":"tab_previous"}]
        }))?,
    )?;
    s.control(v, "help", "")?;
    eventually(|| Ok(!s.painted(v, 24, 80)?.contents().contains("split side")))?;
    let screen = s.painted(v, 24, 80)?;
    let y = (0..23)
        .find(|y| row(&screen, *y).contains("previous tab"))
        .need()?;
    assert!((0..80).any(|x| screen.cell(y, x).is_some_and(|c| c.inverse() && c.dim())));
    s.capture(v, 24, 80, "keybindings-unavailable")?;
    s.enter(v)?;
    assert!(
        s.viewer(v)?
            .at("notice")
            .at("text")
            .as_str()
            .need()?
            .contains("only one tab")
    );
    assert_eq!(s.query("fux::model::Tab")?.rows().count(), 1);
    s.control(v, "copy_mode", "")?;
    s.key(v, " ", false)?;
    assert!(s.painted(v, 24, 80)?.hide_cursor());
    s.key(v, "c", false)?;
    s.key(v, "y", false)?;
    assert!(
        s.viewer(v)?
            .at("notice")
            .at("text")
            .as_str()
            .need()?
            .contains("Space starts")
    );
    assert!(s.painted(v, 24, 80)?.hide_cursor());
    s.key(v, " ", false)?;
    s.key(v, "escape", false)?;
    assert!(!s.painted(v, 24, 80)?.hide_cursor());
    s.control(v, "copy_mode", "")?;
    s.key(v, "pageup", false)?;
    s.key(v, "g", false)?;
    assert_eq!(s.viewer(v)?.at("scrollback"), 0);
    assert!(s.viewer(v)?.at("notice").is_null());
    assert!(!s.painted(v, 24, 80)?.hide_cursor());
    s.control(v, "copy_mode", "")?;
    s.key(v, "q", false)?;
    assert!(!s.painted(v, 24, 80)?.hide_cursor());
    Ok(())
}
