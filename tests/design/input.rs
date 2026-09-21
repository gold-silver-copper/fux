use super::*;

#[test]
fn cursor_uses_first_and_last_content_cells_without_insets() -> Outcome {
    for (position, expected) in [("1;1", (0, 0)), ("9;20", (8, 19))] {
        let s = Server::start()?;
        let v = s.attach()?;
        s.resize(v, 10, 20)?;
        s.painted(v, 10, 20)?;
        s.run(
            v,
            &format!(
                r#"exec /bin/sh -c 'printf "\033[2J\033[HREADY\033[{position}H"; exec sleep 60'"#
            ),
        )?;
        eventually(|| {
            let screen = s.painted(v, 10, 20)?;
            Ok(screen.contents().starts_with("READY") && screen.cursor_position() == expected)
        })?;
        let screen = s.painted(v, 10, 20)?;
        assert_eq!(screen.cursor_position(), expected);
        assert!(!screen.hide_cursor());
    }
    Ok(())
}

#[test]
fn tiny_content_uses_exact_one_cell_backing_without_wide_halves() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?;
    s.run(v,r"stty raw -echo; printf '\033[2J\033[HREADY'; read ignore; printf '\033[2J\033[H界界界界界\033[H'; sleep 60")?;
    eventually(|| Ok(s.screen(v)?.starts_with("READY")))?;
    s.resize(v, 2, 1)?;
    s.painted(v, 2, 1)?;
    let before = s
        .query("fux::model::ProcessState")?
        .at(0)
        .at("components")
        .at("fux::model::ProcessState")
        .at("revision")
        .as_u64()
        .need()?;
    s.input(v, json!({"kind":"paste","text":"\n"}))?;
    eventually(|| {
        Ok(s.query("fux::model::ProcessState")?
            .at(0)
            .at("components")
            .at("fux::model::ProcessState")
            .at("revision")
            .as_u64()
            .need()?
            > before)
    })?;
    let screen = s.painted(v, 2, 1)?;
    assert_eq!(screen.cell(0, 0).need()?.bgcolor(), Color::Default);
    assert!(!screen.cell(0, 0).need()?.is_wide());
    assert_eq!(screen.cell(1, 0).need()?.bgcolor(), Color::Idx(8));
    let state = &s
        .query("fux::model::ProcessState")?
        .at(0)
        .at("components")
        .at("fux::model::ProcessState");
    assert_eq!(state.at("rows"), 1);
    assert_eq!(state.at("cols"), 1);
    Ok(())
}

#[test]
fn mouse_edges_literal_prefix_and_modal_input_are_byte_exact() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.resize(v, 6, 12)?;
    s.painted(v, 6, 12)?;
    let input = s.directory.join("input.bin");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HREADY\033[?1003h\033[?1006h'; cat > '{}'",
            input.display()
        ),
    )?;
    eventually(|| Ok(s.painted(v, 6, 12)?.contents().starts_with("READY")))?;
    s.mouse(v, "press", 0, 0)?;
    s.mouse(v, "release", 0, 0)?;
    s.mouse(v, "press", 11, 4)?;
    s.mouse(v, "release", 11, 4)?;
    s.mouse(v, "press", 0, 5)?; // bottom bar
    s.key(v, "b", true)?;
    // No paint between prefix and mouse: it still must not leak input.
    s.mouse(v, "press", 0, 0)?;
    s.key(v, "!", false)?;
    s.input(v, json!({"kind":"paste","text":"forbidden"}))?;
    s.key(v, "escape", false)?;
    s.key(v, "b", true)?;
    s.key(v, "b", true)?;
    s.command(v, "help")?;
    s.painted(v, 6, 12)?;
    s.mouse(v, "press", 0, 0)?;
    s.input(v, json!({"kind":"paste","text":"forbidden"}))?;
    s.key(v, "escape", false)?;
    // After dismissal, even before repaint, stale overlay bounds cannot eat input.
    s.mouse(v, "press", 11, 4)?;
    let mut expected =
        b"\x1b[<0;1;1M\x1b[<0;1;1m\x1b[<0;12;5M\x1b[<0;12;5m\x02\x1b[<0;12;5M".to_vec();
    eventually(|| Ok(fs::read(&input).is_ok_and(|bytes| bytes == expected)))?;
    let small = s
        .rpc("fux.attach", json!({"rows":4,"cols":8}))?
        .at("viewer")
        .as_u64()
        .need()?;
    s.painted(v, 6, 12)?;
    s.mouse(v, "press", 11, 4)?; // larger viewer's blank margin
    s.mouse(v, "press", 7, 2)?;
    expected.extend_from_slice(b"\x1b[<0;8;3M");
    eventually(|| Ok(fs::read(&input).is_ok_and(|bytes| bytes == expected)))?;
    let screen = s.painted(v, 6, 12)?;
    assert!(!screen.cell(0, 11).need()?.has_contents());
    assert!(!screen.cell(4, 0).need()?.has_contents());
    s.command(small, "detach")?;
    Ok(())
}

#[test]
fn settings_hot_reload_short_empty_and_unicode_help() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.resize(v, 8, 32)?;
    s.command(v, "help")?;
    for _ in 0..30 {
        s.key(v, "down", false)?;
    }
    let last = s.selected(v, 8, 32)?.need()?;
    assert!(!last.contains("split side by side"), "{last}");
    fs::write(
        s.directory.join("fux.json"),
        serde_json::to_vec(&json!({
            "prefix":"ctrl-a", "bindings":[{"key":"界","action":"custom_界é"},{"key":"d","action":"detach"}]
        }))?,
    )?;
    eventually(|| Ok(s.painted(v, 8, 32)?.contents().contains("custom 界é")))?;
    // The selection clamps to the last action, not to a viewport offset.
    let selected = s.selected(v, 8, 32)?.need()?;
    assert!(selected.starts_with("界  custom"), "{selected}");
    let screen = s.painted(v, 8, 32)?;
    assert!(!screen.contents().contains("more"));
    assert!(row(&screen, 6).contains("custom 界é"));
    assert!(row(&screen, 4).contains("detach"));
    assert!(row(&screen, 3).contains("Session"));
    assert!(row(&screen, 5).contains("Other"));
    // Content sized: no full-width modal background above the bar.
    assert_eq!(screen.cell(6, 0).need()?.bgcolor(), Color::Default);
    assert_eq!(screen.cell(6, 31).need()?.bgcolor(), Color::Idx(8));
    assert!(row(&screen, 2).contains("Commands"));
    assert!((0..32).any(|x| screen.cell(3, x).is_some_and(|c| c.bold())));
    s.key(v, "escape", false)?;
    s.key(v, "a", true)?;
    assert!(s.column_open(v, 8, 32)?);
    s.key(v, "界", false)?; // unknown action remains discoverable and errors visibly
    assert!(
        s.viewer(v)?
            .at("notice")
            .at("text")
            .as_str()
            .need()?
            .contains("unknown action")
    );
    let screen = s.painted(v, 8, 32)?;
    assert!((0..32).any(|x| {
        screen
            .cell(7, x)
            .is_some_and(|c| c.fgcolor() == Color::Idx(1))
    }));
    s.command(v, "help")?;
    fs::write(s.directory.join("fux.json"), r#"{"bindings":[]}"#)?;
    eventually(|| Ok(s.painted(v, 8, 32)?.contents().contains("No bindings")))?;
    s.key(v, "down", false)?;
    assert!(s.column_open(v, 8, 32)?);
    assert_eq!(s.selected(v, 8, 32)?, None);
    for (rows, cols) in [(2, 1), (3, 2), (4, 3), (1, 1)] {
        s.resize(v, rows, cols)?;
        let screen = s.painted(v, rows, cols)?;
        assert_eq!(
            screen.cell(rows - 1, cols - 1).need()?.bgcolor(),
            Color::Idx(8)
        );
        assert!(screen.hide_cursor());
    }
    Ok(())
}

#[test]
fn viewers_keep_independent_focus_zoom_history_and_exit_status() -> Outcome {
    let s = Server::start()?;
    let a = s.attach()?;
    s.split(a, "horizontal", None)?;
    let b = s.attach()?;
    s.painted(a, 24, 80)?;
    s.painted(b, 24, 80)?;
    let b_focus = s.focused(b)?;
    assert_ne!(s.focused(a)?, b_focus);
    s.command(a, "zoom")?;
    s.control(a, json!({"kind":"scroll","order":"previous"}))?;
    s.painted(a, 24, 80)?;
    let b_screen = s.painted(b, 24, 80)?;
    assert_eq!(s.focused(b)?, b_focus);
    assert_eq!(s.viewer(b)?.at("zoom"), false);
    assert_eq!(s.viewer(b)?.at("scrollback"), 0);
    assert!(b_screen.contents().contains('│'));
    s.control(a, json!({"kind":"scroll","order":"next"}))?;
    s.run(a, "exit 7")?;
    eventually(|| Ok(row(&s.painted(a, 24, 80)?, 23).contains("exit:7")))?;
    assert!(s.painted(a, 24, 80)?.hide_cursor());
    let screen = s.painted(b, 24, 80)?;
    assert!(row(&screen, 22).contains("exit:7"));
    let marker = (0..80).find(|&x| text(&screen, 22, x) == "[").need()?;
    assert!(screen.cell(22, marker).need()?.dim());
    assert!(screen.cell(22, marker).need()?.inverse());
    s.command(b, "copy")?;
    let screen = s.painted(b, 24, 80)?;
    assert!((0..80).any(|x| {
        screen
            .cell(23, x)
            .is_some_and(|c| c.fgcolor() == Color::Idx(3))
    }));
    // An unknown action name no longer reaches the viewer: the request itself fails.
    assert!(s.control(b, json!({"kind":"not_an_action"})).is_err());
    let screen = s.painted(b, 24, 80)?;
    assert!(!(0..80).any(|x| {
        screen
            .cell(23, x)
            .is_some_and(|c| c.fgcolor() == Color::Idx(1))
    }));
    Ok(())
}
