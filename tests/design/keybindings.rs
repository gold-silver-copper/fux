use super::*;

#[test]
fn reserved_menu_keys_override_custom_bindings_and_enter_executes_selected_action() {
    let s = Server::start();
    let a = s.attach();
    let b = s.attach();
    s.screen(a);
    let before = s.query("bevy_ui::ui_node::Node");
    let focus = s.viewer(a)["focus"].clone();
    fs::write(
        s.directory.join("fux.json"),
        serde_json::to_vec(&json!({
            "prefix":"ctrl-a", "bindings":[
                {"key":"down","action":"grow_width"},
                {"key":"right","action":"shrink_width"},
                {"key":"t","action":"tab_new"},
                {"key":"[","action":"tab_previous"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    s.control(a, "help", "");
    eventually(|| s.painted(a, 24, 80).contents().contains("down  grow width"));
    s.key(a, "escape", false);
    s.key(a, "a", true);
    s.key(a, "down", false);
    assert_eq!(s.viewer(a)["help_scroll"], 1);
    s.key(a, "right", false);
    assert_eq!(s.viewer(a)["help_scroll"], 1);
    s.key(a, "down", false);
    assert_eq!(s.viewer(a)["help_scroll"], 2);
    assert_eq!(s.viewer(b)["help_scroll"], 0);
    assert_eq!(s.viewer(a)["focus"], focus);
    assert_eq!(s.query("bevy_ui::ui_node::Node"), before);
    let screen = s.painted(a, 24, 80);
    let selected_row = (0..23)
        .find(|y| row(&screen, *y).contains("new tab"))
        .unwrap();
    assert!((0..80).any(|x| screen.cell(selected_row, x).unwrap().inverse()));
    s.capture(a, 24, 80, "keybindings-selected");
    s.enter(a);
    assert_eq!(s.query("fux::model::Tab").len(), 2);
    assert_eq!(s.viewer(a)["prefix"], false);
    assert_ne!(s.viewer(a)["tab"], s.viewer(b)["tab"]);
    s.capture(a, 24, 80, "keybindings-new-tab");
}

#[test]
fn a_navigation_key_prefix_still_forwards_its_literal_when_doubled() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v);
    let input = s.directory.join("literal-navigation");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HREADY'; cat > '{}'",
            input.display()
        ),
    );
    eventually(|| s.screen(v).starts_with("READY"));
    fs::write(
        s.directory.join("fux.json"),
        serde_json::to_vec(&json!({
            "prefix":"up", "bindings":[{"key":"up","action":"terminate"}]
        }))
        .unwrap(),
    )
    .unwrap();
    s.control(v, "help", "");
    eventually(|| {
        s.painted(v, 24, 80)
            .contents()
            .contains("up  terminate process")
    });
    s.key(v, "escape", false);
    s.key(v, "up", false);
    assert_eq!(s.viewer(v)["prefix"], true);
    s.key(v, "up", false);
    assert_eq!(s.viewer(v)["prefix"], false);
    eventually(|| fs::read(&input).is_ok_and(|b| b == b"\x1b[A"));
}

#[test]
fn unavailable_selected_command_reports_reason_and_copy_escape_exits_once() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v);
    fs::write(
        s.directory.join("fux.json"),
        serde_json::to_vec(&json!({
            "bindings":[{"key":"[","action":"tab_previous"}]
        }))
        .unwrap(),
    )
    .unwrap();
    s.control(v, "help", "");
    eventually(|| !s.painted(v, 24, 80).contents().contains("split side"));
    let screen = s.painted(v, 24, 80);
    let y = (0..23)
        .find(|y| row(&screen, *y).contains("previous tab"))
        .unwrap();
    assert!((0..80).any(|x| {
        let c = screen.cell(y, x).unwrap();
        c.inverse() && c.dim()
    }));
    s.capture(v, 24, 80, "keybindings-unavailable");
    s.enter(v);
    assert!(
        s.viewer(v)["notice"]
            .as_str()
            .unwrap()
            .contains("only one tab")
    );
    assert_eq!(s.query("fux::model::Tab").len(), 1);
    s.control(v, "copy_mode", "");
    s.key(v, " ", false);
    assert!(s.painted(v, 24, 80).hide_cursor());
    s.key(v, "c", false);
    s.key(v, "y", false);
    assert!(
        s.viewer(v)["notice"]
            .as_str()
            .unwrap()
            .contains("Space starts")
    );
    assert!(s.painted(v, 24, 80).hide_cursor());
    s.key(v, " ", false);
    s.key(v, "escape", false);
    assert!(!s.painted(v, 24, 80).hide_cursor());
    s.control(v, "copy_mode", "");
    s.key(v, "pageup", false);
    s.key(v, "g", false);
    assert_eq!(s.viewer(v)["scrollback"], 0);
    assert_eq!(s.viewer(v)["notice"], "");
    assert!(!s.painted(v, 24, 80).hide_cursor());
    s.control(v, "copy_mode", "");
    s.key(v, "q", false);
    assert!(!s.painted(v, 24, 80).hide_cursor());
}
