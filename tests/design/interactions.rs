use super::*;

#[test]
fn hidden_tabs_stop_constraining_pty_size_even_before_the_switching_viewer_paints() {
    let s = Server::start();
    let large = s.attach();
    let small = s.attach();
    s.resize(small, 8, 25);
    s.screen(large);
    s.painted(small, 8, 25);
    let process = s.query("fux::model::Launch")[0]["entity"].clone();
    let dimensions = || {
        let state = s
            .query("fux::model::ProcessState")
            .into_iter()
            .find(|p| p["entity"] == process)
            .unwrap()["components"]["fux::model::ProcessState"]
            .clone();
        (
            state["rows"].as_u64().unwrap(),
            state["cols"].as_u64().unwrap(),
        )
    };
    assert_eq!(dimensions(), (7, 25));
    s.control(small, "tab_new", "hidden-from-large");
    s.screen(large);
    eventually(|| dimensions() == (23, 80));
    s.control(small, "tab_previous", "");
    s.screen(large);
    eventually(|| dimensions() == (7, 25));
}

#[test]
fn workspace_order_chooser_memory_and_scene_replacement_are_consistent() {
    let s = Server::start();
    let a = s.attach();
    let b = s.attach();
    s.screen(a);
    s.screen(b);
    let initial = s.viewer(a)["workspace"].as_u64().unwrap();
    s.control(a, "tab_new", "remembered");
    let tab = s.viewer(a)["tab"].clone();
    let focus = s.viewer(a)["focus"].clone();
    s.control(a, "workspace_new", "second");
    let second = s.viewer(a)["workspace"].as_u64().unwrap();
    s.control(a, "workspace_new", "third");
    let third = s.viewer(a)["workspace"].as_u64().unwrap();
    s.control(a, "workspace_reorder_previous", "");
    targeted(&s, a, "workspace_select", initial);
    assert_eq!(s.viewer(a)["tab"], tab);
    assert_eq!(s.viewer(a)["focus"], focus);
    s.control(a, "workspace_next", "");
    assert_eq!(s.viewer(a)["workspace"], third);
    s.control(a, "workspace_next", "");
    assert_eq!(s.viewer(a)["workspace"], second);
    s.control(a, "workspace_previous", "");
    assert_eq!(s.viewer(a)["workspace"], third);
    targeted(&s, a, "workspace_select", initial);
    s.control(a, "workspace_choose", "");
    s.capture(a, 24, 80, "interaction-workspaces");
    s.key(a, "escape", false);
    let path = s.directory.join("tabs.scn.ron");
    s.control(a, "save_layout", path.to_str().unwrap());
    eventually(|| fs::metadata(&path).is_ok_and(|m| m.len() > 0));
    s.control(a, "load_layout", path.to_str().unwrap());
    eventually(|| s.viewer(a)["workspace"] != initial);
    let replacement = s.viewer(a)["workspace"].clone();
    assert_eq!(s.viewer(b)["workspace"], replacement);
    assert!(s.viewer(a)["focus"].as_u64().is_some());
    assert!(s.viewer(b)["focus"].as_u64().is_some());
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains("remembered"));
    assert!(!text.contains("Navigation"));
}

#[test]
fn hidden_native_nodes_cannot_receive_focus_or_child_input() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v);
    let left = s.viewer(v)["focus"].as_u64().unwrap();
    let left_file = s.directory.join("visible-input");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HLEFT'; cat > '{}'",
            left_file.display()
        ),
    );
    eventually(|| s.screen(v).starts_with("LEFT"));
    s.control(v, "split_horizontal", "");
    s.screen(v);
    let hidden = s.viewer(v)["focus"].as_u64().unwrap();
    let hidden_file = s.directory.join("hidden-input");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HRIGHT'; cat > '{}'",
            hidden_file.display()
        ),
    );
    eventually(|| s.screen(v).contains("RIGHT"));
    s.rpc(
        "world.insert_components",
        json!({"entity":hidden,"components":{"bevy_camera::visibility::Visibility":"Hidden"}}),
    );
    s.key(v, "K", false);
    eventually(|| fs::read(&left_file).is_ok_and(|b| b == b"K"));
    assert_eq!(s.viewer(v)["focus"], left);
    targeted(&s, v, "focus", hidden);
    assert_eq!(s.viewer(v)["focus"], left);
    s.control(v, "focus_last", "");
    assert_eq!(s.viewer(v)["focus"], left);
    assert!(fs::read(&hidden_file).unwrap().is_empty());
    s.rpc(
        "world.remove_components",
        json!({"entity":hidden,"components":["bevy_camera::visibility::Visibility"]}),
    );
    targeted(&s, v, "focus", hidden);
    s.key(v, "R", false);
    eventually(|| fs::read(&hidden_file).is_ok_and(|b| b == b"R"));
}

#[test]
fn nested_swap_and_existing_tab_workspace_moves_keep_process_identity_and_history() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v);
    let a = s.viewer(v)["focus"].as_u64().unwrap();
    s.run(v, "printf 'RETAINED-HISTORY\\n'");
    eventually(|| s.screen(v).contains("RETAINED-HISTORY"));
    s.control(v, "split_horizontal", "");
    s.screen(v);
    s.control(v, "split_vertical", "");
    s.screen(v);
    let c = s.viewer(v)["focus"].as_u64().unwrap();
    let before = s.query("fux::model::ProcessState");
    targeted(&s, v, "swap", a);
    s.screen(v);
    assert_eq!(s.viewer(v)["focus"], c);
    s.control(v, "tab_new", "destination");
    let tab = s.viewer(v)["tab"].as_u64().unwrap();
    s.control(v, "tab_previous", "");
    targeted(&s, v, "focus", a);
    targeted(&s, v, "move_tab", tab);
    s.screen(v);
    assert_eq!(s.viewer(v)["tab"], tab);
    assert_eq!(s.viewer(v)["focus"], a);
    let screen = s.painted(v, 24, 80);
    assert!(screen.contents().contains('│'));
    assert!(screen.contents().contains("RETAINED-HISTORY"));
    s.control(v, "workspace_new", "destination-workspace");
    let root = s.viewer(v)["workspace"].as_u64().unwrap();
    s.control(v, "workspace_previous", "");
    assert_eq!(s.viewer(v)["focus"], a);
    targeted(&s, v, "move_workspace", root);
    s.screen(v);
    assert_eq!(s.viewer(v)["workspace"], root);
    assert_eq!(s.viewer(v)["focus"], a);
    let after = s.query("fux::model::ProcessState");
    for old in before {
        let retained = after.iter().find(|p| p["entity"] == old["entity"]).unwrap();
        assert_eq!(
            retained["components"]["fux::model::ProcessState"]["pid"],
            old["components"]["fux::model::ProcessState"]["pid"]
        );
    }
    assert!(s.screen(v).contains("RETAINED-HISTORY"));
}

fn targeted(s: &Server, viewer: u64, action: &str, target: u64) {
    s.rpc("world.trigger_event", json!({"event":"fux::control::Control","value":{"viewer":viewer,"action":action,"target":target}}));
}

#[test]
fn tabs_bar_native_click_chooser_and_independent_focus_survive_switches() {
    let s = Server::start();
    let a = s.attach();
    let b = s.attach();
    s.screen(a);
    s.screen(b);
    let first = s.viewer(a)["tab"].as_u64().unwrap();
    let focus = s.viewer(a)["focus"].clone();
    let original = s.query("fux::model::ProcessState");
    s.control(a, "tab_new", "logs");
    let second = s.viewer(a)["tab"].as_u64().unwrap();
    assert_ne!(first, second);
    assert_eq!(s.viewer(b)["tab"], first);
    let screen = s.painted(a, 24, 80);
    assert!(row(&screen, 23).contains("logs"));
    let log_x = (0..80).find(|x| text(&screen, 23, *x) == "l").unwrap();
    assert!(screen.cell(23, log_x).unwrap().inverse());
    s.control(a, "tab_previous", "");
    assert_eq!(s.viewer(a)["focus"], focus);
    let screen = s.painted(a, 24, 80);
    let log_x = (0..40).find(|x| text(&screen, 23, *x) == "l").unwrap();
    s.mouse(a, "press", log_x, 23);
    assert_eq!(s.viewer(a)["tab"], second);
    assert_eq!(s.viewer(b)["focus"], focus);
    s.control(a, "tab_choose", "");
    let screen = s.painted(a, 24, 80);
    assert!(screen.contents().contains("choose tab"));
    assert!(screen.hide_cursor());
    s.key(a, "enter", false);
    assert_eq!(s.viewer(a)["tab"], first);
    assert_eq!(s.viewer(a)["focus"], focus);
    let current = s.query("fux::model::ProcessState");
    for process in original {
        let retained = current
            .iter()
            .find(|p| p["entity"] == process["entity"])
            .unwrap();
        assert_eq!(
            retained["components"]["fux::model::ProcessState"]["pid"],
            process["components"]["fux::model::ProcessState"]["pid"]
        );
    }
    s.capture(a, 24, 80, "interaction-tabs");
}

#[test]
fn interactive_close_is_modal_captured_and_automation_is_explicit() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v);
    s.control(v, "split_horizontal", "");
    s.screen(v);
    let close = s.viewer(v)["focus"].as_u64().unwrap();
    let other = s
        .query("fux::model::PaneView")
        .into_iter()
        .map(|p| p["entity"].as_u64().unwrap())
        .find(|e| *e != close)
        .unwrap();
    targeted(&s, v, "tab_close", other);
    assert_eq!(s.query("fux::model::PaneView").len(), 2);
    assert!(
        s.viewer(v)["notice"]
            .as_str()
            .unwrap()
            .contains("wrong kind")
    );
    let tab = s.viewer(v)["tab"].as_u64().unwrap();
    targeted(&s, v, "close", tab);
    assert_eq!(s.query("fux::model::PaneView").len(), 2);
    s.key(v, "b", true);
    s.key(v, "x", false);
    let screen = s.painted(v, 24, 80);
    assert!(screen.contents().contains("y confirm"));
    assert!(screen.hide_cursor());
    s.capture(v, 24, 80, "interaction-confirm");
    targeted(&s, v, "focus", other);
    s.input(v, json!({"kind":"paste","text":"y"}));
    assert_eq!(s.query("fux::model::PaneView").len(), 2);
    s.key(v, "y", false);
    assert!(
        s.query("fux::model::PaneView")
            .iter()
            .all(|p| p["entity"] != close)
    );
    assert_eq!(s.viewer(v)["focus"], other);
    s.control(v, "close", "");
    assert!(s.query("fux::model::PaneView").is_empty());
    assert!(s.query("fux::model::Launch").is_empty());
    s.control(v, "split_horizontal", "");
    assert_eq!(s.query("fux::model::PaneView").len(), 1);
}

#[test]
fn directional_previous_last_focus_and_rearrangement_preserve_processes() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v);
    let left = s.viewer(v)["focus"].as_u64().unwrap();
    s.control(v, "split_horizontal", "");
    s.screen(v);
    let right = s.viewer(v)["focus"].as_u64().unwrap();
    s.control(v, "focus_left", "");
    assert_eq!(s.viewer(v)["focus"], left);
    s.control(v, "focus_last", "");
    assert_eq!(s.viewer(v)["focus"], right);
    s.screen(v);
    s.control(v, "focus_previous", "");
    assert_eq!(s.viewer(v)["focus"], left);
    s.control(v, "focus_right", "");
    assert_eq!(s.viewer(v)["focus"], right);
    s.control(v, "swap_left", "");
    s.screen(v);
    s.control(v, "focus_right", "");
    assert_eq!(s.viewer(v)["focus"], left);
    let before = s.query("fux::model::Launch");
    s.control(v, "zoom", "");
    assert_eq!(s.viewer(v)["zoom"], true);
    s.control(v, "move_new_tab", "moved");
    assert_eq!(s.viewer(v)["zoom"], false);
    assert_eq!(s.viewer(v)["focus"], left);
    assert_eq!(s.query("fux::model::Launch"), before);
    s.control(v, "zoom", "");
    s.control(v, "tab_previous", "");
    assert_eq!(s.viewer(v)["zoom"], false);
    assert_eq!(s.viewer(v)["focus"], right);
}

#[test]
fn context_menu_captures_unfocused_pane_and_grouped_help_marks_unavailable() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v);
    s.key(v, "b", true);
    let screen = s.painted(v, 24, 80);
    assert!(screen.contents().contains("Panes"));
    let mut disabled = false;
    for y in 0..23 {
        for x in 0..80 {
            disabled |= screen.cell(y, x).unwrap().dim();
        }
    }
    assert!(disabled);
    s.key(v, "escape", false);
    s.control(v, "split_horizontal", "");
    s.screen(v);
    let focused = s.viewer(v)["focus"].clone();
    s.input(v, json!({"kind":"mouse","action":"press","button":2,"x":0,"y":0,"ctrl":false,"alt":false,"shift":true}));
    assert_eq!(s.viewer(v)["focus"], focused);
    assert!(s.painted(v, 24, 80).contents().contains("Panes:"));
    s.capture(v, 24, 80, "interaction-menu");
    s.key(v, "escape", false);
    assert_eq!(s.viewer(v)["focus"], focused);
    s.control(v, "tab_choose", "");
    s.capture(v, 24, 80, "interaction-chooser");
}

#[test]
fn remote_viewer_removal_during_an_overlay_never_panics_the_server() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v);
    s.key(v, "b", true);
    s.key(v, "p", false);
    assert!(s.painted(v, 24, 80).contents().contains("Panes:"));
    s.rpc("world.despawn_entity", json!({"entity":v}));
    for key in ["down", "enter", "escape", "b"] {
        s.key(v, key, key == "b");
    }
    s.input(v, json!({"kind":"paste_begin"}));
    s.input(v, json!({"kind":"paste","text":"late"}));
    s.mouse(v, "press", 3, 3);
    for action in [
        "pane_menu",
        "split_horizontal",
        "focus_next",
        "tab_new",
        "copy_mode",
    ] {
        s.control(v, action, "");
    }
    assert!(s.request("rpc.discover", Value::Null).is_ok());
    assert!(
        s.request("fux.frame", json!({"viewer":v})).unwrap()["detach"]
            .as_bool()
            .unwrap()
    );
    let fresh = s.attach();
    assert!(s.screen(fresh).contains("main"));
    s.key(fresh, "b", true);
    s.key(fresh, "p", false);
    assert!(s.painted(fresh, 24, 80).contents().contains("Panes:"));
}
