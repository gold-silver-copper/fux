use super::*;

#[test]
fn hidden_tabs_stop_constraining_pty_size_even_before_the_switching_viewer_paints() -> Outcome {
    let s = Server::start()?;
    let large = s.attach()?;
    let small = s.attach()?;
    s.resize(small, 8, 25)?;
    s.screen(large)?;
    s.painted(small, 8, 25)?;
    let process = s.query("fux::model::Launch")?.at(0).at("entity");
    let dimensions = || -> Result<(u64, u64), String> {
        let state = s
            .query("fux::model::ProcessState")?
            .rows()
            .find(|p| p.at("entity") == process)
            .need()?
            .at("components")
            .at("fux::model::ProcessState");
        Ok((
            state.at("rows").as_u64().need()?,
            state.at("cols").as_u64().need()?,
        ))
    };
    assert_eq!(dimensions()?, (7, 25));
    s.control(small, "tab_new", "hidden-from-large")?;
    s.screen(large)?;
    eventually(|| Ok(dimensions()? == (23, 80)))?;
    s.control(small, "tab_previous", "")?;
    s.screen(large)?;
    eventually(|| Ok(dimensions()? == (7, 25)))?;
    Ok(())
}

#[test]
fn workspace_order_chooser_memory_and_scene_replacement_are_consistent() -> Outcome {
    let s = Server::start()?;
    let a = s.attach()?;
    let b = s.attach()?;
    s.screen(a)?;
    s.screen(b)?;
    let initial = s.viewer(a)?.at("workspace").as_u64().need()?;
    s.control(a, "tab_new", "remembered")?;
    let tab = s.viewer(a)?.at("tab");
    let focus = s.viewer(a)?.at("focus");
    s.control(a, "workspace_new", "second")?;
    let second = s.viewer(a)?.at("workspace").as_u64().need()?;
    s.control(a, "workspace_new", "third")?;
    let third = s.viewer(a)?.at("workspace").as_u64().need()?;
    s.control(a, "workspace_reorder_previous", "")?;
    targeted(&s, a, "workspace_select", initial)?;
    assert_eq!(s.viewer(a)?.at("tab"), tab);
    assert_eq!(s.viewer(a)?.at("focus"), focus);
    s.control(a, "workspace_next", "")?;
    assert_eq!(s.viewer(a)?.at("workspace"), third);
    s.control(a, "workspace_next", "")?;
    assert_eq!(s.viewer(a)?.at("workspace"), second);
    s.control(a, "workspace_previous", "")?;
    assert_eq!(s.viewer(a)?.at("workspace"), third);
    targeted(&s, a, "workspace_select", initial)?;
    s.control(a, "workspace_choose", "")?;
    s.capture(a, 24, 80, "interaction-workspaces")?;
    s.key(a, "escape", false)?;
    let path = s.directory.join("tabs.scn.ron");
    s.control(a, "save_layout", path.to_str().need()?)?;
    eventually(|| Ok(fs::metadata(&path).is_ok_and(|m| m.len() > 0)))?;
    s.control(a, "load_layout", path.to_str().need()?)?;
    eventually(|| Ok(s.viewer(a)?.at("workspace") != initial))?;
    let replacement = s.viewer(a)?.at("workspace");
    assert_eq!(s.viewer(b)?.at("workspace"), replacement);
    assert!(s.viewer(a)?.at("focus").as_u64().is_some());
    assert!(s.viewer(b)?.at("focus").as_u64().is_some());
    let text = fs::read_to_string(path)?;
    assert!(text.contains("remembered"));
    assert!(!text.contains("Navigation"));
    Ok(())
}

#[test]
fn hidden_native_nodes_cannot_receive_focus_or_child_input() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    let left = s.viewer(v)?.at("focus").as_u64().need()?;
    let left_file = s.directory.join("visible-input");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HLEFT'; cat > '{}'",
            left_file.display()
        ),
    )?;
    eventually(|| Ok(s.screen(v)?.starts_with("LEFT")))?;
    s.control(v, "split_horizontal", "")?;
    s.screen(v)?;
    let hidden = s.viewer(v)?.at("focus").as_u64().need()?;
    let hidden_file = s.directory.join("hidden-input");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HRIGHT'; cat > '{}'",
            hidden_file.display()
        ),
    )?;
    eventually(|| Ok(s.screen(v)?.contains("RIGHT")))?;
    s.rpc(
        "world.insert_components",
        json!({"entity":hidden,"components":{"bevy_camera::visibility::Visibility":"Hidden"}}),
    )?;
    s.key(v, "K", false)?;
    eventually(|| Ok(fs::read(&left_file).is_ok_and(|b| b == b"K")))?;
    assert_eq!(s.viewer(v)?.at("focus"), left);
    targeted(&s, v, "focus", hidden)?;
    assert_eq!(s.viewer(v)?.at("focus"), left);
    s.control(v, "focus_last", "")?;
    assert_eq!(s.viewer(v)?.at("focus"), left);
    assert!(fs::read(&hidden_file)?.is_empty());
    s.rpc(
        "world.remove_components",
        json!({"entity":hidden,"components":["bevy_camera::visibility::Visibility"]}),
    )?;
    targeted(&s, v, "focus", hidden)?;
    s.key(v, "R", false)?;
    eventually(|| Ok(fs::read(&hidden_file).is_ok_and(|b| b == b"R")))?;
    Ok(())
}

#[test]
fn nested_swap_and_existing_tab_workspace_moves_keep_process_identity_and_history() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    let a = s.viewer(v)?.at("focus").as_u64().need()?;
    s.run(v, "printf 'RETAINED-HISTORY\\n'")?;
    eventually(|| Ok(s.screen(v)?.contains("RETAINED-HISTORY")))?;
    s.control(v, "split_horizontal", "")?;
    s.screen(v)?;
    s.control(v, "split_vertical", "")?;
    s.screen(v)?;
    let c = s.viewer(v)?.at("focus").as_u64().need()?;
    let before = s.query("fux::model::ProcessState")?;
    targeted(&s, v, "swap", a)?;
    s.screen(v)?;
    assert_eq!(s.viewer(v)?.at("focus"), c);
    s.control(v, "tab_new", "destination")?;
    let tab = s.viewer(v)?.at("tab").as_u64().need()?;
    s.control(v, "tab_previous", "")?;
    targeted(&s, v, "focus", a)?;
    targeted(&s, v, "move_tab", tab)?;
    s.screen(v)?;
    assert_eq!(s.viewer(v)?.at("tab"), tab);
    assert_eq!(s.viewer(v)?.at("focus"), a);
    let screen = s.painted(v, 24, 80)?;
    assert!(screen.contents().contains('│'));
    assert!(screen.contents().contains("RETAINED-HISTORY"));
    s.control(v, "workspace_new", "destination-workspace")?;
    let root = s.viewer(v)?.at("workspace").as_u64().need()?;
    s.control(v, "workspace_previous", "")?;
    assert_eq!(s.viewer(v)?.at("focus"), a);
    targeted(&s, v, "move_workspace", root)?;
    s.screen(v)?;
    assert_eq!(s.viewer(v)?.at("workspace"), root);
    assert_eq!(s.viewer(v)?.at("focus"), a);
    let after = s.query("fux::model::ProcessState")?;
    for old in before.rows() {
        let retained = after
            .rows()
            .find(|p| p.at("entity") == old.at("entity"))
            .need()?;
        assert_eq!(
            retained
                .at("components")
                .at("fux::model::ProcessState")
                .at("pid"),
            old.at("components")
                .at("fux::model::ProcessState")
                .at("pid")
        );
    }
    assert!(s.screen(v)?.contains("RETAINED-HISTORY"));
    Ok(())
}

fn targeted(s: &Server, viewer: u64, action: &str, target: u64) -> Result<(), String> {
    s.rpc("world.trigger_event", json!({"event":"fux::control::Control","value":{"viewer":viewer,"action":action,"target":target}}))?;
    Ok(())
}

#[test]
fn tabs_bar_native_click_chooser_and_independent_focus_survive_switches() -> Outcome {
    let s = Server::start()?;
    let a = s.attach()?;
    let b = s.attach()?;
    s.screen(a)?;
    s.screen(b)?;
    let first = s.viewer(a)?.at("tab").as_u64().need()?;
    let focus = s.viewer(a)?.at("focus");
    let original = s.query("fux::model::ProcessState")?;
    s.control(a, "tab_new", "logs")?;
    let second = s.viewer(a)?.at("tab").as_u64().need()?;
    assert_ne!(first, second);
    assert_eq!(s.viewer(b)?.at("tab"), first);
    let screen = s.painted(a, 24, 80)?;
    assert!(row(&screen, 23).contains("logs"));
    let log_x = (0..80).find(|x| text(&screen, 23, *x) == "l").need()?;
    assert!(screen.cell(23, log_x).need()?.inverse());
    s.control(a, "tab_previous", "")?;
    assert_eq!(s.viewer(a)?.at("focus"), focus);
    let screen = s.painted(a, 24, 80)?;
    let log_x = (0..40).find(|x| text(&screen, 23, *x) == "l").need()?;
    s.mouse(a, "press", log_x, 23)?;
    assert_eq!(s.viewer(a)?.at("tab"), second);
    assert_eq!(s.viewer(b)?.at("focus"), focus);
    s.control(a, "tab_choose", "")?;
    let screen = s.painted(a, 24, 80)?;
    assert!(screen.contents().contains("choose tab"));
    assert!(screen.hide_cursor());
    s.key(a, "enter", false)?;
    assert_eq!(s.viewer(a)?.at("tab"), first);
    assert_eq!(s.viewer(a)?.at("focus"), focus);
    let current = s.query("fux::model::ProcessState")?;
    for process in original.rows() {
        let retained = current
            .rows()
            .find(|p| p.at("entity") == process.at("entity"))
            .need()?;
        assert_eq!(
            retained
                .at("components")
                .at("fux::model::ProcessState")
                .at("pid"),
            process
                .at("components")
                .at("fux::model::ProcessState")
                .at("pid")
        );
    }
    s.capture(a, 24, 80, "interaction-tabs")?;
    Ok(())
}

#[test]
fn interactive_close_is_modal_captured_and_automation_is_explicit() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    s.control(v, "split_horizontal", "")?;
    s.screen(v)?;
    let close = s.viewer(v)?.at("focus").as_u64().need()?;
    let other = s
        .query("fux::model::PaneView")?
        .rows()
        .filter_map(|p| p.at("entity").as_u64())
        .find(|e| *e != close)
        .need()?;
    targeted(&s, v, "tab_close", other)?;
    assert_eq!(s.query("fux::model::PaneView")?.rows().count(), 2);
    assert!(
        s.viewer(v)?
            .at("notice")
            .as_str()
            .need()?
            .contains("wrong kind")
    );
    let tab = s.viewer(v)?.at("tab").as_u64().need()?;
    targeted(&s, v, "close", tab)?;
    assert_eq!(s.query("fux::model::PaneView")?.rows().count(), 2);
    s.key(v, "b", true)?;
    s.key(v, "x", false)?;
    let screen = s.painted(v, 24, 80)?;
    assert!(screen.contents().contains("y confirm"));
    assert!(screen.hide_cursor());
    s.capture(v, 24, 80, "interaction-confirm")?;
    targeted(&s, v, "focus", other)?;
    s.input(v, json!({"kind":"paste","text":"y"}))?;
    assert_eq!(s.query("fux::model::PaneView")?.rows().count(), 2);
    s.key(v, "y", false)?;
    assert!(
        s.query("fux::model::PaneView")?
            .rows()
            .all(|p| p.at("entity") != close)
    );
    assert_eq!(s.viewer(v)?.at("focus"), other);
    s.control(v, "close", "")?;
    assert!(s.query("fux::model::PaneView")?.rows().next().is_none());
    assert!(s.query("fux::model::Launch")?.rows().next().is_none());
    s.control(v, "split_horizontal", "")?;
    assert_eq!(s.query("fux::model::PaneView")?.rows().count(), 1);
    Ok(())
}

#[test]
fn directional_previous_last_focus_and_rearrangement_preserve_processes() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    let left = s.viewer(v)?.at("focus").as_u64().need()?;
    s.control(v, "split_horizontal", "")?;
    s.screen(v)?;
    let right = s.viewer(v)?.at("focus").as_u64().need()?;
    s.control(v, "focus_left", "")?;
    assert_eq!(s.viewer(v)?.at("focus"), left);
    s.control(v, "focus_last", "")?;
    assert_eq!(s.viewer(v)?.at("focus"), right);
    s.screen(v)?;
    s.control(v, "focus_previous", "")?;
    assert_eq!(s.viewer(v)?.at("focus"), left);
    s.control(v, "focus_right", "")?;
    assert_eq!(s.viewer(v)?.at("focus"), right);
    s.control(v, "swap_left", "")?;
    s.screen(v)?;
    s.control(v, "focus_right", "")?;
    assert_eq!(s.viewer(v)?.at("focus"), left);
    let before = s.query("fux::model::Launch")?;
    s.control(v, "zoom", "")?;
    assert_eq!(s.viewer(v)?.at("zoom"), true);
    s.control(v, "move_new_tab", "moved")?;
    assert_eq!(s.viewer(v)?.at("zoom"), false);
    assert_eq!(s.viewer(v)?.at("focus"), left);
    assert_eq!(s.query("fux::model::Launch")?, before);
    s.control(v, "zoom", "")?;
    s.control(v, "tab_previous", "")?;
    assert_eq!(s.viewer(v)?.at("zoom"), false);
    assert_eq!(s.viewer(v)?.at("focus"), right);
    Ok(())
}

#[test]
fn context_menu_captures_unfocused_pane_and_grouped_help_marks_unavailable() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    s.key(v, "b", true)?;
    let screen = s.painted(v, 24, 80)?;
    assert!(screen.contents().contains("Panes"));
    let mut disabled = false;
    for y in 0..23 {
        for x in 0..80 {
            disabled |= screen.cell(y, x).need()?.dim();
        }
    }
    assert!(disabled);
    s.key(v, "escape", false)?;
    s.control(v, "split_horizontal", "")?;
    s.screen(v)?;
    let focused = s.viewer(v)?.at("focus");
    s.input(v, json!({"kind":"mouse","action":"press","button":2,"x":0,"y":0,"ctrl":false,"alt":false,"shift":true}))?;
    assert_eq!(s.viewer(v)?.at("focus"), focused);
    assert!(s.painted(v, 24, 80)?.contents().contains("Panes:"));
    s.capture(v, 24, 80, "interaction-menu")?;
    s.key(v, "escape", false)?;
    assert_eq!(s.viewer(v)?.at("focus"), focused);
    s.control(v, "tab_choose", "")?;
    s.capture(v, 24, 80, "interaction-chooser")?;
    Ok(())
}

#[test]
fn remote_viewer_removal_during_an_overlay_never_panics_the_server() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    s.key(v, "b", true)?;
    s.key(v, "p", false)?;
    assert!(s.painted(v, 24, 80)?.contents().contains("Panes:"));
    s.rpc("world.despawn_entity", json!({"entity":v}))?;
    for key in ["down", "enter", "escape", "b"] {
        s.key(v, key, key == "b")?;
    }
    s.input(v, json!({"kind":"paste_begin"}))?;
    s.input(v, json!({"kind":"paste","text":"late"}))?;
    s.mouse(v, "press", 3, 3)?;
    for action in [
        "pane_menu",
        "split_horizontal",
        "focus_next",
        "tab_new",
        "copy_mode",
    ] {
        s.control(v, action, "")?;
    }
    assert!(s.request("rpc.discover", Value::Null).is_ok());
    assert!(
        s.request("fux.frame", json!({"viewer":v}))?
            .at("detach")
            .as_bool()
            .need()?
    );
    let fresh = s.attach()?;
    assert!(s.screen(fresh)?.contains("main"));
    s.key(fresh, "b", true)?;
    s.key(fresh, "p", false)?;
    assert!(s.painted(fresh, 24, 80)?.contents().contains("Panes:"));
    Ok(())
}
