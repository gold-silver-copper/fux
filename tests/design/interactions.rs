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
    // The small viewer's paint resized the PTY and published its size in the
    // same step, so the query reads it at once.
    assert_eq!(dimensions()?, (7, 25));
    s.tab_new(small, Some("hidden-from-large"))?;
    s.screen(large)?;
    eventually(|| Ok(dimensions()? == (23, 80)))?;
    s.scoped(small, "previous", "tab")?;
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
    let initial = s.viewing(a)?.as_u64().need()?;
    s.tab_new(a, Some("remembered"))?;
    let tab = s.on_tab(a)?;
    let focus = s.focused(a)?;
    s.control(a, json!({"kind":"workspace_new","name":"second"}))?;
    let second = s.viewing(a)?.as_u64().need()?;
    s.control(a, json!({"kind":"workspace_new","name":"third"}))?;
    let third = s.viewing(a)?.as_u64().need()?;
    s.control(
        a,
        json!({"kind":"reorder","scope":"workspace","order":"previous"}),
    )?;
    s.control(
        a,
        json!({"kind":"select","scope":"workspace","entity":initial}),
    )?;
    assert_eq!(s.on_tab(a)?, tab);
    assert_eq!(s.focused(a)?, focus);
    s.scoped(a, "next", "workspace")?;
    assert_eq!(s.viewing(a)?, third);
    s.scoped(a, "next", "workspace")?;
    assert_eq!(s.viewing(a)?, second);
    s.scoped(a, "previous", "workspace")?;
    assert_eq!(s.viewing(a)?, third);
    s.control(
        a,
        json!({"kind":"select","scope":"workspace","entity":initial}),
    )?;
    s.control(a, json!({"kind":"choose","chooser":"workspace"}))?;
    s.capture(a, 24, 80, "interaction-workspaces")?;
    s.key(a, "escape", false)?;
    let path = s.directory.join("tabs.scn.ron");
    let workspace = s.workspace_of(a)?;
    s.control(
        a,
        json!({"kind":"save_layout","workspace":workspace,"path":path}),
    )?;
    eventually(|| Ok(fs::metadata(&path).is_ok_and(|m| m.len() > 0)))?;
    s.control(
        a,
        json!({"kind":"load_layout","workspace":workspace,"path":path,"mapping":[]}),
    )?;
    eventually(|| Ok(s.viewing(a)? != initial))?;
    let replacement = s.viewing(a)?;
    assert_eq!(s.viewing(b)?, replacement);
    assert!(s.focused(a)?.as_u64().is_some());
    assert!(s.focused(b)?.as_u64().is_some());
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
    let left = s.focused(v)?.as_u64().need()?;
    let left_file = s.directory.join("visible-input");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HLEFT'; cat > '{}'",
            left_file.display()
        ),
    )?;
    eventually(|| Ok(s.screen(v)?.starts_with("LEFT")))?;
    s.split(v, "horizontal", None)?;
    s.screen(v)?;
    let hidden = s.focused(v)?.as_u64().need()?;
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
    assert_eq!(s.focused(v)?, left);
    s.focus(v, hidden)?;
    assert_eq!(s.focused(v)?, left);
    s.command(v, "focus_last")?;
    assert_eq!(s.focused(v)?, left);
    assert!(fs::read(&hidden_file)?.is_empty());
    s.rpc(
        "world.remove_components",
        json!({"entity":hidden,"components":["bevy_camera::visibility::Visibility"]}),
    )?;
    s.focus(v, hidden)?;
    s.key(v, "R", false)?;
    eventually(|| Ok(fs::read(&hidden_file).is_ok_and(|b| b == b"R")))?;
    Ok(())
}

#[test]
fn nested_swap_and_existing_tab_workspace_moves_keep_process_identity_and_history() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    let a = s.focused(v)?.as_u64().need()?;
    s.run(v, "printf 'RETAINED-HISTORY\\n'")?;
    eventually(|| Ok(s.screen(v)?.contains("RETAINED-HISTORY")))?;
    s.split(v, "horizontal", None)?;
    s.screen(v)?;
    s.split(v, "vertical", None)?;
    s.screen(v)?;
    let c = s.focused(v)?.as_u64().need()?;
    // The pid is published when a process starts, asynchronously after the
    // split; compare pids only once every process has one.
    eventually(|| {
        Ok(s.query("fux::model::ProcessState")?.rows().all(|p| {
            !p.at("components")
                .at("fux::model::ProcessState")
                .at("status")
                .at("pid")
                .is_null()
        }))
    })?;
    let before = s.query("fux::model::ProcessState")?;
    s.control(v, json!({"kind":"swap","with":a}))?;
    s.screen(v)?;
    assert_eq!(s.focused(v)?, c);
    s.tab_new(v, Some("destination"))?;
    let tab = s.on_tab(v)?.as_u64().need()?;
    s.scoped(v, "previous", "tab")?;
    s.focus(v, a)?;
    s.control(v, json!({"kind":"move","to":{"kind":"tab","tab":tab}}))?;
    s.screen(v)?;
    assert_eq!(s.on_tab(v)?, tab);
    assert_eq!(s.focused(v)?, a);
    let screen = s.painted(v, 24, 80)?;
    assert!(screen.contents().contains('│'));
    assert!(screen.contents().contains("RETAINED-HISTORY"));
    s.control(
        v,
        json!({"kind":"workspace_new","name":"destination-workspace"}),
    )?;
    let root = s.viewing(v)?.as_u64().need()?;
    s.scoped(v, "previous", "workspace")?;
    assert_eq!(s.focused(v)?, a);
    s.control(
        v,
        json!({"kind":"move","to":{"kind":"workspace","workspace":root}}),
    )?;
    s.screen(v)?;
    assert_eq!(s.viewing(v)?, root);
    assert_eq!(s.focused(v)?, a);
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
                .at("status")
                .at("pid"),
            old.at("components")
                .at("fux::model::ProcessState")
                .at("status")
                .at("pid")
        );
    }
    assert!(s.screen(v)?.contains("RETAINED-HISTORY"));
    Ok(())
}

#[test]
fn tabs_bar_native_click_chooser_and_independent_focus_survive_switches() -> Outcome {
    let s = Server::start()?;
    let a = s.attach()?;
    let b = s.attach()?;
    s.screen(a)?;
    s.screen(b)?;
    let first = s.on_tab(a)?.as_u64().need()?;
    let focus = s.focused(a)?;
    let original = s.query("fux::model::ProcessState")?;
    s.tab_new(a, Some("logs"))?;
    let second = s.on_tab(a)?.as_u64().need()?;
    assert_ne!(first, second);
    assert_eq!(s.on_tab(b)?, first);
    let screen = s.painted(a, 24, 80)?;
    assert!(row(&screen, 23).contains("logs"));
    let log_x = (0..80).find(|x| text(&screen, 23, *x) == "l").need()?;
    assert!(screen.cell(23, log_x).need()?.inverse());
    s.scoped(a, "previous", "tab")?;
    assert_eq!(s.focused(a)?, focus);
    let screen = s.painted(a, 24, 80)?;
    let log_x = (0..40).find(|x| text(&screen, 23, *x) == "l").need()?;
    s.mouse(a, "press", log_x, 23)?;
    assert_eq!(s.on_tab(a)?, second);
    assert_eq!(s.focused(b)?, focus);
    s.control(a, json!({"kind":"choose","chooser":"tab"}))?;
    let screen = s.painted(a, 24, 80)?;
    assert!(screen.contents().contains("choose tab"));
    assert!(screen.hide_cursor());
    s.key(a, "enter", false)?;
    assert_eq!(s.on_tab(a)?, first);
    assert_eq!(s.focused(a)?, focus);
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
                .at("status")
                .at("pid"),
            process
                .at("components")
                .at("fux::model::ProcessState")
                .at("status")
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
    s.split(v, "horizontal", None)?;
    s.screen(v)?;
    let close = s.focused(v)?.as_u64().need()?;
    let other = s
        .query("fux::model::PaneView")?
        .rows()
        .filter_map(|p| p.at("entity").as_u64())
        .find(|e| *e != close)
        .need()?;
    s.close(v, json!({"tab":other}))?;
    assert_eq!(s.query("fux::model::PaneView")?.rows().count(), 2);
    assert!(
        s.viewer(v)?
            .at("notice")
            .at("text")
            .as_str()
            .need()?
            .contains("wrong kind")
    );
    let tab = s.on_tab(v)?.as_u64().need()?;
    s.close(v, json!({"pane":tab}))?;
    assert_eq!(s.query("fux::model::PaneView")?.rows().count(), 2);
    s.key(v, "b", true)?;
    s.key(v, "x", false)?;
    let screen = s.painted(v, 24, 80)?;
    assert!(screen.contents().contains("y confirm"));
    assert!(screen.hide_cursor());
    s.capture(v, 24, 80, "interaction-confirm")?;
    s.focus(v, other)?;
    s.input(v, json!({"kind":"paste","text":"y"}))?;
    assert_eq!(s.query("fux::model::PaneView")?.rows().count(), 2);
    s.key(v, "y", false)?;
    assert!(
        s.query("fux::model::PaneView")?
            .rows()
            .all(|p| p.at("entity") != close)
    );
    assert_eq!(s.focused(v)?, other);
    s.close(v, json!({"pane":s.focused(v)?}))?;
    assert!(s.query("fux::model::PaneView")?.rows().next().is_none());
    assert!(s.query("fux::model::Launch")?.rows().next().is_none());
    s.split(v, "horizontal", None)?;
    assert_eq!(s.query("fux::model::PaneView")?.rows().count(), 1);
    Ok(())
}

#[test]
fn directional_previous_last_focus_and_rearrangement_preserve_processes() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    let left = s.focused(v)?.as_u64().need()?;
    s.split(v, "horizontal", None)?;
    s.screen(v)?;
    let right = s.focused(v)?.as_u64().need()?;
    s.control(v, json!({"kind":"focus_direction","direction":"left"}))?;
    assert_eq!(s.focused(v)?, left);
    s.command(v, "focus_last")?;
    assert_eq!(s.focused(v)?, right);
    s.screen(v)?;
    s.command(v, "focus_previous")?;
    assert_eq!(s.focused(v)?, left);
    s.control(v, json!({"kind":"focus_direction","direction":"right"}))?;
    assert_eq!(s.focused(v)?, right);
    s.control(v, json!({"kind":"swap_direction","direction":"left"}))?;
    s.screen(v)?;
    s.control(v, json!({"kind":"focus_direction","direction":"right"}))?;
    assert_eq!(s.focused(v)?, left);
    let before = s.query("fux::model::Launch")?;
    s.command(v, "zoom")?;
    assert_eq!(s.viewer(v)?.at("zoom"), true);
    s.control(
        v,
        json!({"kind":"move","to":{"kind":"new_tab","name":"moved"}}),
    )?;
    assert_eq!(s.viewer(v)?.at("zoom"), false);
    assert_eq!(s.focused(v)?, left);
    assert_eq!(s.query("fux::model::Launch")?, before);
    s.command(v, "zoom")?;
    s.scoped(v, "previous", "tab")?;
    assert_eq!(s.viewer(v)?.at("zoom"), false);
    assert_eq!(s.focused(v)?, right);
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
    s.split(v, "horizontal", None)?;
    s.screen(v)?;
    let focused = s.focused(v)?;
    s.input(v, json!({"kind":"mouse","action":"press","button":"right","x":0,"y":0,"ctrl":false,"alt":false,"shift":true}))?;
    assert_eq!(s.focused(v)?, focused);
    assert!(s.painted(v, 24, 80)?.contents().contains("Panes:"));
    s.capture(v, 24, 80, "interaction-menu")?;
    s.key(v, "escape", false)?;
    assert_eq!(s.focused(v)?, focused);
    s.control(v, json!({"kind":"choose","chooser":"tab"}))?;
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
    for command in [
        json!({"kind":"menu","subject":{"pane":v}}),
        json!({"kind":"split","axis":"horizontal","program":null}),
        json!({"kind":"focus_next"}),
        json!({"kind":"tab_new","name":null}),
        json!({"kind":"copy_mode"}),
    ] {
        s.control(v, command)?;
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

#[test]
fn unknown_control_action_names_are_rejected_at_the_api_boundary() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    let rejected = s.request(
        "world.trigger_event",
        json!({"event":"fux::control::Control","value":{"viewer":v,"command":{"kind":"custom_界é"}}}),
    );
    assert!(rejected.is_err());
    assert!(s.viewer(v)?.at("notice").is_null());
    assert!(s.request("rpc.discover", Value::Null).is_ok());
    s.split(v, "horizontal", None)?;
    assert_eq!(s.query("fux::model::PaneView")?.rows().count(), 2);
    Ok(())
}
