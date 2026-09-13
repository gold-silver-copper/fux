use super::*;

/// Application mouse reporting and alternate screens through the real PTY path.
pub(super) fn run(binary: &Path) -> Result<()> {
    let logs = tempfile::tempdir()?;
    let root = Root::new(
        "fmouse-",
        &[
            std::env::current_exe()?.to_string_lossy().into_owned(),
            "fixture-worker".into(),
            "mouse-app".into(),
            logs.path().to_string_lossy().into_owned(),
        ],
    )?;
    let config = root.path().join("config/fux/config.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)? + "clipboard = \"write-only\"\n",
    )?;
    let mut h = Harness {
        root,
        binary: binary.into(),
        viewers: Vec::new(),
        finished: false,
    };
    h.add(&[])?;
    h.wait(
        |h| Ok(h.text(0).contains("MOUSE_APP_READY")),
        "reporting app ready",
        8,
    )?;
    h.send(0, b"\x01|")?;
    let tabs = h.state(|tabs| panes(&tabs[0]) == 2, "two reporting apps", 8)?;
    let left = logs
        .path()
        .join(format!("{}.input", tabs[0]["panes"][0]["pid"]));
    let right = logs
        .path()
        .join(format!("{}.input", tabs[0]["panes"][1]["pid"]));
    h.wait(
        |h| Ok(h.text(0).matches("MOUSE_APP_READY").count() == 2),
        "both reporting apps ready",
        8,
    )?;
    h.send(0, b"\x1b[<68;3;3M")?;
    h.wait(
        |h| Ok(h.text(0).contains("History pane 1") && h.text(0).contains("offset 3")),
        "Shift browses left reporting app",
        8,
    )?;
    let left_before = fs::read(&left)?;
    let right_before = fs::read(&right)?;
    h.send(0, b"\x1b[<64;60;3M")?;
    h.wait(
        |_| Ok(fs::read(&right)?.len() >= right_before.len() + b"\x1b[<64;20;3M".len()),
        "right app receives wheel while left browses",
        8,
    )?;
    ensure!(fs::read(&left)? == left_before, "wheel leaked to left app");
    ensure!(
        &fs::read(&right)?[right_before.len()..] == b"\x1b[<64;20;3M",
        "right mouse coordinates or bytes changed: {:?}",
        fs::read(&right)?
    );
    ensure!(
        h.text(0).contains("History pane 1"),
        "app mouse dismissed unrelated history"
    );
    let right_before = fs::read(&right)?;
    h.send(0, b"\x1b[<68;60;3M")?;
    h.wait(
        |h| Ok(h.text(0).contains("History pane 2")),
        "Shift browses right reporting app",
        8,
    )?;
    ensure!(
        fs::read(&right)? == right_before,
        "Shift wheel reached application"
    );
    h.send(0, b"MOUSE_SENTINEL")?;
    h.wait(
        |_| Ok(fs::read(&right)?.ends_with(b"MOUSE_SENTINEL")),
        "typing reaches focused reporting app",
        8,
    )?;
    ensure!(
        &fs::read(&right)?[right_before.len()..] == b"MOUSE_SENTINEL",
        "typing not byte exact"
    );
    h.wait(
        |h| Ok(h.text(0).contains("History pane 1")),
        "typing retains only unrelated history",
        8,
    )?;
    h.send(0, b"\x1b[<68;60;3M")?;
    h.wait(
        |h| Ok(h.text(0).contains("History pane 2")),
        "right history before buffer switch",
        8,
    )?;
    // Public pane-addressed input changes the app's buffer without local typing
    // first clearing history, so reconciliation itself must invalidate the view.
    h.cli(&["send-keys", "2", "\\x02"])?;
    h.wait(
        |h| Ok(h.text(0).contains("ALTERNATE_READY") && h.text(0).contains("History pane 1")),
        "alternate entry invalidates right history only",
        8,
    )?;
    h.send(0, b"\x1b[<68;60;3M")?;
    h.wait(
        |h| Ok(h.text(0).contains("History pane 2")),
        "Shift history in alternate buffer",
        8,
    )?;
    h.cli(&["send-keys", "2", "\\x03"])?;
    h.wait(
        |h| Ok(h.text(0).contains("PRIMARY_RETURNED") && h.text(0).contains("History pane 1")),
        "primary return invalidates alternate history",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("History pane")),
        "Escape restores final history without commands",
        8,
    )?;
    ensure!(
        !h.text(0).contains("split side by side"),
        "Escape opened commands"
    );
    ensure!(
        fs::read(&left)? == left_before,
        "local history or Escape leaked to left app"
    );
    ensure!(
        &fs::read(&right)?[right_before.len()..] == b"MOUSE_SENTINEL\x02\x03",
        "history or Escape leaked to right app"
    );
    h.send(0, b"\x1b[<4;3;3M\x1b[<36;5;4M")?;
    h.wait(
        |h| Ok(h.text(0).contains("Copy")),
        "active left selection before lost release",
        8,
    )?;
    let before_click = fs::read(&right)?;
    h.send(0, b"\x1b[<0;60;3M\x1b[<0;60;3m")?;
    h.wait(
        |_| Ok(fs::read(&right)?.len() >= before_click.len() + b"\x1b[<0;20;3M\x1b[<0;20;3m".len()),
        "fresh click recovers capture and reaches app",
        8,
    )?;
    h.hold(|h| !h.text(0).contains("Copy"), 0.1)?;
    ensure!(
        &fs::read(&right)?[before_click.len()..] == b"\x1b[<0;20;3M\x1b[<0;20;3m",
        "fresh app gesture was lost, duplicated or retargeted"
    );
    // Ignored popup buttons still own their tails, including when a keyboard
    // command switches to the controller parser before release over another app.
    for button in [1, 2] {
        for enter_copy in [false, true] {
            let before_left = fs::read(&left)?;
            let before_right = fs::read(&right)?;
            h.send(0, b"\x01")?;
            h.wait(
                |h| Ok(h.text(0).contains("split stacked")),
                "popup before auxiliary press",
                8,
            )?;
            h.send(0, format!("\x1b[<{button};3;3M").as_bytes())?;
            if enter_copy {
                h.send(0, b"[")?;
                h.wait(
                    |h| Ok(h.text(0).contains("Copy")),
                    "popup keyboard command before auxiliary release",
                    8,
                )?;
            } else {
                h.send(0, b"\x1b")?;
                h.wait(
                    |h| Ok(!h.text(0).contains("split stacked")),
                    "popup dismisses before auxiliary release",
                    8,
                )?;
            }
            h.send(0, format!("\x1b[<{button};3;3m").as_bytes())?;
            if enter_copy {
                h.send(0, b"\x1b")?;
                h.wait(
                    |h| Ok(!h.text(0).contains("Copy")),
                    "copy dismisses after auxiliary release",
                    8,
                )?;
            }
            h.send(0, b"AFTER_AUXILIARY")?;
            h.wait(
                |_| Ok(fs::read(&right)?.len() >= before_right.len() + b"AFTER_AUXILIARY".len()),
                "ordinary input after auxiliary popup gesture",
                8,
            )?;
            ensure!(
                fs::read(&left)? == before_left,
                "popup auxiliary gesture leaked to other app"
            );
            ensure!(
                &fs::read(&right)?[before_right.len()..] == b"AFTER_AUXILIARY",
                "popup auxiliary gesture leaked or swallowed input"
            );
        }
    }
    for copy in [false, true] {
        let before = fs::read(&right)?;
        let copies = h.viewers[0].screen.callbacks().0.len();
        h.send(0, b"\x01[ ")?;
        h.wait(
            |h| Ok(h.text(0).contains("Copy selection")),
            "selection before q or copy finish",
            8,
        )?;
        h.send(0, if copy { b"hhy" } else { b"q" })?;
        h.wait(
            |h| Ok(!h.text(0).contains("Copy selection") && !h.text(0).contains("Copy ")),
            "q or successful copy returns normal",
            8,
        )?;
        if copy {
            h.wait(
                |h| Ok(h.viewers[0].screen.callbacks().0.len() == copies + 1),
                "successful clipboard delivery",
                8,
            )?;
            ensure!(
                h.viewers[0]
                    .screen
                    .callbacks()
                    .0
                    .last()
                    .is_some_and(|data| data == b"RUQ="),
                "clipboard must contain selected ED text"
            );
        }
        h.send(0, b"AFTER_COPY_EXIT")?;
        h.wait(
            |_| Ok(fs::read(&right)?.len() >= before.len() + b"AFTER_COPY_EXIT".len()),
            "exact input after q or copy finish",
            8,
        )?;
        ensure!(
            &fs::read(&right)?[before.len()..] == b"AFTER_COPY_EXIT",
            "copy finish leaked or swallowed input"
        );
    }
    // A paste into focused B resumes only B; its command-looking content is literal.
    h.send(0, b"\x1b[<68;3;3M\x1b[<68;60;3M")?;
    h.wait(
        |h| Ok(h.text(0).contains("History pane 2")),
        "both histories before literal paste",
        8,
    )?;
    let before = fs::read(&right)?;
    let paste = "\x1b[200~PASTE\x01|界\x1b[201~PASTE_DONE".as_bytes();
    h.send(0, paste)?;
    h.wait(
        |_| Ok(fs::read(&right)?.len() >= before.len() + paste.len()),
        "byte-exact paste reaches focused app",
        8,
    )?;
    ensure!(
        &fs::read(&right)?[before.len()..] == paste,
        "paste bytes changed or executed a command"
    );
    h.wait(
        |h| Ok(h.text(0).contains("History pane 1")),
        "paste preserves other pane history",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("History pane")),
        "history dismissal after paste",
        8,
    )?;

    // Buffer invalidation dismisses Copy, but an unfinished paste still belongs
    // to its old parser until the delimiter arrives; its tail must never execute.
    let before = fs::read(&right)?;
    h.send(0, b"\x01[\x1b[200~discarded")?;
    h.wait(
        |h| Ok(h.text(0).contains("Copy ")),
        "copy owns unfinished paste",
        8,
    )?;
    h.cli(&["send-keys", "2", "\\x02"])?;
    h.wait(
        |h| Ok(h.text(0).contains("ALTERNATE_READY") && !h.text(0).contains("Copy ")),
        "buffer switch dismisses copy during paste",
        8,
    )?;
    h.send(0, b"\x01|\x1b[201~AFTER_DRAIN")?;
    h.wait(
        |_| Ok(fs::read(&right)?.len() >= before.len() + b"\x02AFTER_DRAIN".len()),
        "next input follows drained cancelled paste",
        8,
    )?;
    ensure!(
        &fs::read(&right)?[before.len()..] == b"\x02AFTER_DRAIN",
        "cancelled paste tail reached app"
    );
    ensure!(
        panes(&h.tabs("default")?[0]) == 2,
        "cancelled paste tail executed a split"
    );
    ensure!(
        fs::read(&left)? == left_before,
        "copy or paste reached unrelated app"
    );
    h.cli(&["send-keys", "2", "\\x03"])?;
    h.wait(
        |h| Ok(h.text(0).contains("PRIMARY_RETURNED")),
        "primary buffer restored after paste drain",
        8,
    )?;
    h.cli(&["split", "vertical", "--ratio", "7000", "--no-focus"])?;
    let nested = h.state(
        |tabs| panes(&tabs[0]) == 3,
        "nested reporting app layout",
        8,
    )?;
    let areas = nested[0]["panes"].as_array().context("nested panes")?;
    ensure!(
        areas[1]["geometry"]["height"] != areas[2]["geometry"]["height"],
        "nested panes must have unequal heights"
    );
    let third = logs.path().join(format!("{}.input", areas[2]["pid"]));
    let region = |h: &Harness, index: usize| -> Result<Vec<String>> {
        let geometry = &areas[index]["geometry"];
        let x = usize::try_from(geometry["x"].as_u64().context("pane x")?)?;
        let y = usize::try_from(geometry["y"].as_u64().context("pane y")?)?;
        let width = usize::try_from(geometry["width"].as_u64().context("pane width")?)?;
        let height = usize::try_from(geometry["height"].as_u64().context("pane height")?)?;
        Ok(h.rows(0)
            .iter()
            .skip(y)
            .take(height.saturating_sub(1))
            .map(|row| row.chars().skip(x).take(width).collect())
            .collect())
    };
    h.wait(
        |h| Ok(third.is_file() && h.text(0).matches("MOUSE_APP_READY").count() >= 2),
        "nested reporting app ready",
        8,
    )?;
    let wheel = |index: usize, shift: bool| -> Result<String> {
        let geometry = &areas[index]["geometry"];
        Ok(format!(
            "\x1b[<{};{};{}M",
            if shift { 68 } else { 64 },
            geometry["x"].as_u64().context("pane x")? + 3,
            geometry["y"].as_u64().context("pane y")? + 2
        ))
    };
    let live_a = region(&h, 0)?;
    let live_c = region(&h, 2)?;
    h.send(0, wheel(0, true)?.as_bytes())?;
    h.wait(
        |h| Ok(region(h, 0)? != live_a && h.text(0).contains("offset 3")),
        "nested A history",
        8,
    )?;
    let history_a = region(&h, 0)?;
    h.send(0, wheel(2, true)?.as_bytes())?;
    h.wait(
        |h| Ok(region(h, 2)? != live_c && h.text(0).contains("History pane 3")),
        "nested C history beside A",
        8,
    )?;
    ensure!(
        region(&h, 0)? == history_a,
        "nested C wheel changed A history"
    );
    let history_c = region(&h, 2)?;
    h.send(0, wheel(0, true)?.as_bytes())?;
    h.wait(
        |h| Ok(region(h, 0)? != history_a && h.text(0).contains("offset 6")),
        "nested return to A preserves C",
        8,
    )?;
    ensure!(
        region(&h, 2)? == history_c,
        "nested A wheel changed C history"
    );
    let preserved_a = region(&h, 0)?;
    let before_nested = fs::read(&right)?;
    let before_third = fs::read(&third)?;
    let expected = b"\x1b[<64;3;2M";
    h.send(0, wheel(1, false)?.as_bytes())?;
    h.wait(
        |_| Ok(fs::read(&right)?.len() >= before_nested.len() + expected.len()),
        "nested B receives relative mouse bytes",
        8,
    )?;
    ensure!(
        &fs::read(&right)?[before_nested.len()..] == expected,
        "nested wheel coordinates or bytes changed"
    );
    ensure!(
        h.bar(0).trim_end().ends_with('2'),
        "nested wheel changed application focus"
    );
    h.cli(&["send-keys", "2", "\\x02"])?;
    h.wait(
        |h| Ok(h.text(0).contains("ALTERNATE_READY")),
        "nested B alternate screen",
        8,
    )?;
    let before_alt_mouse = fs::read(&right)?;
    h.send(0, wheel(1, false)?.as_bytes())?;
    h.wait(
        |_| Ok(fs::read(&right)?.len() >= before_alt_mouse.len() + expected.len()),
        "alternate application receives nested mouse",
        8,
    )?;
    ensure!(
        &fs::read(&right)?[before_alt_mouse.len()..] == expected,
        "alternate app mouse changed"
    );
    ensure!(
        region(&h, 0)? == preserved_a && region(&h, 2)? == history_c,
        "application mouse or buffer switch changed unrelated histories"
    );
    ensure!(
        fs::read(&left)? == left_before && fs::read(&third)? == before_third,
        "nested local history leaked input"
    );
    h.finish()?;
    println!(
        "PASS real reporting app mouse bytes, Shift history override and screen buffer transitions"
    );
    Ok(())
}
