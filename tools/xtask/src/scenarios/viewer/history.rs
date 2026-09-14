use super::*;

/// Independent histories and input ownership through a real PTY.
pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new(
        "fhistory-",
        &[
            "/bin/sh".into(),
            "-c".into(),
            "printf READY; exec cat".into(),
        ],
    )?;
    let mut h = Harness {
        root,
        binary: binary.into(),
        viewers: Vec::new(),
        finished: false,
    };
    h.add(&[])?;
    h.wait(|h| Ok(h.text(0).contains("READY")), "history first pane", 8)?;
    h.send(0, b"\x01|")?;
    h.state(|tabs| panes(&tabs[0]) == 2, "history two panes", 8)?;
    super::gesture::verify(&mut h)?;
    h.send(0, b"\x1b[<0;3;3M\x1b[<0;3;3m")?;
    let lines = |prefix: char| {
        (1..=80)
            .map(|n| format!("{prefix}{n:03}\r"))
            .collect::<String>()
    };
    h.send(0, lines('A').as_bytes())?;
    h.wait(
        |h| Ok(h.text(0).contains("A080")),
        "left numbered history",
        8,
    )?;
    h.send(0, b"\x1b[<0;60;3M\x1b[<0;60;3m")?;
    h.send(0, lines('B').as_bytes())?;
    h.wait(
        |h| Ok(h.text(0).contains("B080")),
        "right numbered history",
        8,
    )?;
    h.add(&[])?;
    h.wait(
        |h| Ok(h.text(1).contains("B080")),
        "independent live viewer",
        8,
    )?;
    let content = |h: &Harness, index, right: bool| {
        h.rows(index)
            .into_iter()
            .take(22)
            .map(|row| {
                row.chars()
                    .skip(if right { 40 } else { 0 })
                    .take(34)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    let live_a = content(&h, 0, false);
    let live_b = content(&h, 0, true);
    let other = h.settled(1)?;
    let focused = h
        .bar(0)
        .split('│')
        .next_back()
        .unwrap_or_default()
        .to_owned();
    h.send(0, b"\x1b[<64;3;3M")?;
    h.wait(
        |h| Ok(content(h, 0, false) != live_a && h.text(0).contains("offset 3")),
        "wheel A history",
        8,
    )?;
    let history_a = content(&h, 0, false);
    h.send(0, b"\x1b[<64;60;3M")?;
    h.wait(
        |h| Ok(content(h, 0, true) != live_b && h.text(0).contains("offset 3")),
        "A and B independently scrolled",
        8,
    )?;
    ensure!(
        content(&h, 0, false) == history_a,
        "B wheel changed A history: {history_a:?} -> {:?}",
        content(&h, 0, false)
    );
    let history_b = content(&h, 0, true);
    h.send(0, b"\x1b[<64;3;3M")?;
    h.wait(
        |h| Ok(content(h, 0, false) != history_a && h.text(0).contains("offset 6")),
        "return to A history",
        8,
    )?;
    ensure!(
        content(&h, 0, true) == history_b,
        "A wheel changed B history"
    );
    ensure!(
        h.text(1) == other,
        "history crossed viewers:\n{other}\n--- now ---\n{}",
        h.text(1)
    );
    ensure!(h.bar(0).ends_with(&focused), "wheel changed keyboard focus");
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(content(h, 0, false) == live_a),
        "Escape restores only A",
        8,
    )?;
    ensure!(
        content(&h, 0, true) == history_b,
        "Escape reset unrelated history"
    );
    ensure!(
        !h.text(0).contains("split side by side"),
        "Escape opened commands"
    );
    h.send(0, b"NORMAL_SENTINEL\r")?;
    h.wait(
        |h| {
            Ok(content(h, 0, true)
                .iter()
                .any(|row| row.contains("NORMAL_SENTINEL")))
        },
        "typing restores focused B",
        8,
    )?;
    ensure!(
        !content(&h, 0, false)
            .iter()
            .any(|row| row.contains("NORMAL_SENTINEL")),
        "typing reached wrong pane"
    );
    h.send(0, b"\x01[ ")?;
    h.wait(
        |h| Ok(h.text(0).contains("Copy selection")),
        "active keyboard selection",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("Copy")),
        "selection Escape returns normal",
        8,
    )?;
    h.send(0, b"AFTER_SELECTION\r")?;
    h.wait(
        |h| Ok(h.text(0).contains("AFTER_SELECTION")),
        "input after selection dismissal",
        8,
    )?;
    h.send(0, b"\x01[\x01")?;
    h.wait(
        |h| Ok(h.text(0).contains("split side by side")),
        "prefix exits copy into commands",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("split side by side")),
        "command Escape returns normal",
        8,
    )?;
    h.send(0, b"\x1b[<64;3;3M")?;
    h.wait(
        |h| Ok(h.text(0).contains("offset 3")),
        "history before geometry change",
        8,
    )?;
    // Shared geometry follows the smallest viewer. vt100 preserves the top of
    // the screen on shrink, so the history's first row remains A056.
    h.resize(0, 12, 60)?;
    h.wait(
        |h| {
            Ok(h.rows(0).len() == 12
                && h.rows(0).first().is_some_and(|row| row.starts_with("A056"))
                && h.text(0).contains("offset 3"))
        },
        "scrolled history refreshed at smaller geometry",
        8,
    )?;
    h.resize(0, 3, 12)?;
    h.wait(|h| Ok(h.rows(0).len() == 3), "tiny scrolled history", 8)?;
    h.resize(0, 24, 80)?;
    h.wait(
        |h| {
            Ok(h.rows(0).first().is_some_and(|row| row.starts_with("A056"))
                && h.viewers[0]
                    .screen
                    .screen()
                    .cell(20, 39)
                    .is_some_and(|cell| cell.contents() == "│"))
        },
        "restored scrolled history geometry",
        8,
    )?;
    h.send(0, b"\x1b[<4;3;3M\x1b[<36;5;4M")?;
    h.wait(
        |h| Ok(h.text(0).contains("Copy selection")),
        "history selection before resize",
        8,
    )?;
    h.resize(0, 12, 60)?;
    h.wait(
        |h| {
            Ok(h.rows(0).len() == 12
                && h.text(0).contains("Copy")
                && !h.text(0).contains("Copy selection"))
        },
        "resize clears history selection and capture",
        8,
    )?;
    h.send(0, b"\x1b[<0;60;20m")?;
    h.resize(0, 24, 80)?;
    h.wait(
        |h| {
            Ok(h.rows(0).len() == 24
                && h.text(0).contains("Copy")
                && !h.text(0).contains("Copy selection")
                && h.viewers.iter().all(|viewer| {
                    viewer
                        .screen
                        .screen()
                        .cell(20, 39)
                        .is_some_and(|cell| cell.contents() == "│")
                }))
        },
        "selected history restores without stale capture",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        // The resize sequence promoted passive history to explicit Copy. Wait
        // for that owner to dismiss before sending a prefix; absence of the
        // passive hint alone was already true before Escape was processed.
        |h| Ok(!h.text(0).contains("History pane") && !h.text(0).contains("Copy")),
        "geometry history dismissal",
        8,
    )?;
    h.send(0, b"\x01")?;
    h.wait(
        |h| Ok(h.text(0).contains("split stacked")),
        "pointer command popup",
        8,
    )?;
    click_popup_entry(&mut h, "split stacked")?;
    h.state(
        |tabs| panes(&tabs[0]) == 3,
        "popup click creates nested pane once",
        8,
    )?;
    h.send(0, b"\x01")?;
    h.wait(
        |h| Ok(h.text(0).contains("split side by side")),
        "popup before wheel",
        8,
    )?;
    let popup_before = h.text(0);
    h.send(0, b"\x1b[<65;79;5M")?;
    h.wait(
        |h| Ok(h.text(0) != popup_before && h.text(0).contains("more")),
        "wheel scrolls command popup",
        8,
    )?;
    h.send(0, b"\x1b[<0;1;2M\x1b[<0;1;2m")?;
    h.wait(
        |h| Ok(!h.text(0).contains("more")),
        "outside click dismisses popup",
        8,
    )?;
    h.send(0, b"AFTER_POPUP\r")?;
    h.wait(
        |h| Ok(h.text(0).contains("AFTER_POPUP")),
        "normal input after popup mouse dismissal",
        8,
    )?;
    h.send(0, b"\x01;")?;
    h.wait(
        |h| Ok(h.text(0).contains("Rename pane")),
        "rename field before pointer checks",
        8,
    )?;
    click_popup_entry(&mut h, "Rename pane")?;
    h.wait(
        |h| Ok(h.text(0).contains("Rename pane")),
        "rename heading click stays in field",
        8,
    )?;
    h.send(0, b"discarded\x1b[<0;1;1M\x1b[<0;1;1m")?;
    h.wait(
        |h| Ok(!h.text(0).contains("Rename pane") && !h.text(0).contains("split side by side")),
        "outside click dismisses rename normally",
        8,
    )?;
    h.send(0, b"AFTER_RENAME_CLICK\r")?;
    h.wait(
        |h| Ok(h.text(0).contains("AFTER_RENAME_CLICK")),
        "input after rename outside click",
        8,
    )?;
    ensure!(
        panes(&h.tabs("default")?[0]) == 3,
        "popup release caused another command"
    );
    h.finish()?;
    println!("PASS independent pane histories, Escape, focus, prefix and viewer isolation");
    Ok(())
}
