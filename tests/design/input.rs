use super::*;

#[test]
fn cursor_uses_first_and_last_content_cells_without_insets() {
    for (position, expected) in [("1;1", (0, 0)), ("9;20", (8, 19))] {
        let s = Server::start();
        let v = s.attach();
        s.resize(v, 10, 20);
        s.painted(v, 10, 20);
        s.run(
            v,
            &format!(
                r#"exec /bin/sh -c 'printf "\033[2J\033[HREADY\033[{position}H"; exec sleep 60'"#
            ),
        );
        eventually(|| {
            let screen = s.painted(v, 10, 20);
            screen.contents().starts_with("READY") && screen.cursor_position() == expected
        });
        let screen = s.painted(v, 10, 20);
        assert_eq!(screen.cursor_position(), expected);
        assert!(!screen.hide_cursor());
    }
}

#[test]
fn tiny_content_clips_the_emulators_minimum_backing_size() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v);
    s.run(v,r"stty raw -echo; printf '\033[2J\033[HREADY'; read ignore; printf '\033[2J\033[H界界界界界\033[H'; sleep 60");
    eventually(|| s.screen(v).starts_with("READY"));
    s.resize(v, 2, 1);
    s.painted(v, 2, 1);
    let before=s.query("fux::model::ProcessState")[0]["components"]["fux::model::ProcessState"]["revision"].as_u64().unwrap();
    s.input(v, json!({"kind":"paste","text":"\n"}));
    eventually(|| {
        s.query("fux::model::ProcessState")[0]["components"]["fux::model::ProcessState"]["revision"]
            .as_u64()
            .unwrap()
            > before
    });
    let screen = s.painted(v, 2, 1);
    assert_eq!(screen.cell(0, 0).unwrap().bgcolor(), Color::Default);
    assert!(!screen.cell(0, 0).unwrap().is_wide());
    assert_eq!(screen.cell(1, 0).unwrap().bgcolor(), Color::Idx(8));
    let state = &s.query("fux::model::ProcessState")[0]["components"]["fux::model::ProcessState"];
    assert_eq!(state["rows"], 2);
    assert_eq!(state["cols"], 2);
}

#[test]
fn mouse_edges_literal_prefix_and_modal_input_are_byte_exact() {
    let s = Server::start();
    let v = s.attach();
    s.resize(v, 6, 12);
    s.painted(v, 6, 12);
    let input = s.directory.join("input.bin");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HREADY\033[?1003h\033[?1006h'; cat > '{}'",
            input.display()
        ),
    );
    eventually(|| s.painted(v, 6, 12).contents().starts_with("READY"));
    s.mouse(v, "press", 0, 0);
    s.mouse(v, "release", 0, 0);
    s.mouse(v, "press", 11, 4);
    s.mouse(v, "release", 11, 4);
    s.mouse(v, "press", 0, 5); // bottom bar
    s.key(v, "b", true);
    // No paint between prefix and mouse: it still must not leak input.
    s.mouse(v, "press", 0, 0);
    s.key(v, "!", false);
    s.input(v, json!({"kind":"paste","text":"forbidden"}));
    s.key(v, "escape", false);
    s.key(v, "b", true);
    s.key(v, "b", true);
    s.control(v, "help", "");
    s.painted(v, 6, 12);
    s.mouse(v, "press", 0, 0);
    s.input(v, json!({"kind":"paste","text":"forbidden"}));
    s.key(v, "escape", false);
    // After dismissal, even before repaint, stale overlay bounds cannot eat input.
    s.mouse(v, "press", 11, 4);
    let mut expected =
        b"\x1b[<0;1;1M\x1b[<0;1;1m\x1b[<0;12;5M\x1b[<0;12;5m\x02\x1b[<0;12;5M".to_vec();
    eventually(|| fs::read(&input).is_ok_and(|bytes| bytes == expected));
    let small = s.rpc("fux.attach", json!({"rows":4,"cols":8}))["viewer"]
        .as_u64()
        .unwrap();
    s.painted(v, 6, 12);
    s.mouse(v, "press", 11, 4); // larger viewer's blank margin
    s.mouse(v, "press", 7, 2);
    expected.extend_from_slice(b"\x1b[<0;8;3M");
    eventually(|| fs::read(&input).is_ok_and(|bytes| bytes == expected));
    let screen = s.painted(v, 6, 12);
    assert!(!screen.cell(0, 11).unwrap().has_contents());
    assert!(!screen.cell(4, 0).unwrap().has_contents());
    s.control(small, "detach", "");
}

#[test]
fn settings_hot_reload_short_empty_and_unicode_help() {
    let s = Server::start();
    let v = s.attach();
    s.resize(v, 8, 32);
    s.control(v, "help", "");
    for _ in 0..30 {
        s.key(v, "down", false);
    }
    assert!(s.viewer(v)["help_scroll"].as_u64().unwrap() > 0);
    fs::write(s.directory.join("fux.json"),serde_json::to_vec(&json!({
        "prefix":"ctrl-a", "bindings":[{"key":"界","action":"custom_界é"},{"key":"d","action":"detach"}]
    })).unwrap()).unwrap();
    eventually(|| s.painted(v, 8, 32).contents().contains("custom 界é"));
    assert_eq!(s.viewer(v)["help_scroll"], 1); // clamp selected action, not viewport offset
    let screen = s.painted(v, 8, 32);
    assert!(!screen.contents().contains("more"));
    assert!(row(&screen, 6).contains("custom 界é"));
    assert!(row(&screen, 4).contains("detach"));
    assert!(row(&screen, 3).contains("Session"));
    assert!(row(&screen, 5).contains("Other"));
    // Content sized: no full-width modal background above the bar.
    assert_eq!(screen.cell(6, 0).unwrap().bgcolor(), Color::Default);
    assert_eq!(screen.cell(6, 31).unwrap().bgcolor(), Color::Idx(8));
    assert!(row(&screen, 2).contains("Commands"));
    assert!((0..32).any(|x| screen.cell(3, x).unwrap().bold()));
    s.key(v, "escape", false);
    s.key(v, "a", true);
    assert_eq!(s.viewer(v)["prefix"], true);
    s.key(v, "界", false); // unknown action remains discoverable and errors visibly
    assert!(
        s.viewer(v)["notice"]
            .as_str()
            .unwrap()
            .contains("unknown action")
    );
    let screen = s.painted(v, 8, 32);
    assert!((0..32).any(|x| screen.cell(7, x).unwrap().fgcolor() == Color::Idx(1)));
    s.control(v, "help", "");
    fs::write(s.directory.join("fux.json"), r#"{"bindings":[]}"#).unwrap();
    eventually(|| s.painted(v, 8, 32).contents().contains("No bindings"));
    s.key(v, "down", false);
    assert_eq!(s.viewer(v)["help_scroll"], 0);
    for (rows, cols) in [(2, 1), (3, 2), (4, 3), (1, 1)] {
        s.resize(v, rows, cols);
        let screen = s.painted(v, rows, cols);
        assert_eq!(
            screen.cell(rows - 1, cols - 1).unwrap().bgcolor(),
            Color::Idx(8)
        );
        assert!(screen.hide_cursor());
    }
}

#[test]
fn viewers_keep_independent_focus_zoom_history_and_exit_status() {
    let s = Server::start();
    let a = s.attach();
    s.control(a, "split_horizontal", "");
    let b = s.attach();
    s.painted(a, 24, 80);
    s.painted(b, 24, 80);
    let b_focus = s.viewer(b)["focus"].clone();
    assert_ne!(s.viewer(a)["focus"], b_focus);
    s.control(a, "zoom", "");
    s.control(a, "scroll_up", "");
    s.painted(a, 24, 80);
    let b_screen = s.painted(b, 24, 80);
    assert_eq!(s.viewer(b)["focus"], b_focus);
    assert_eq!(s.viewer(b)["zoom"], false);
    assert_eq!(s.viewer(b)["scrollback"], 0);
    assert!(b_screen.contents().contains('│'));
    s.control(a, "scroll_down", "");
    s.run(a, "exit 7");
    eventually(|| row(&s.painted(a, 24, 80), 23).contains("exit:7"));
    assert!(s.painted(a, 24, 80).hide_cursor());
    let screen = s.painted(b, 24, 80);
    assert!(row(&screen, 22).contains("exit:7"));
    let marker = (0..80).find(|&x| text(&screen, 22, x) == "[").unwrap();
    assert!(screen.cell(22, marker).unwrap().dim());
    assert!(screen.cell(22, marker).unwrap().inverse());
    s.control(b, "copy", "");
    let screen = s.painted(b, 24, 80);
    assert!((0..80).any(|x| screen.cell(23, x).unwrap().fgcolor() == Color::Idx(3)));
    s.control(b, "not_an_action", "");
    let screen = s.painted(b, 24, 80);
    assert!((0..80).any(|x| screen.cell(23, x).unwrap().fgcolor() == Color::Idx(1)));
}
