use super::*;

/// A focused real-input counterpart to the generated dismissal/rename traces.
pub(super) fn run(binary: &Path) -> Result<()> {
    let mut h = Harness {
        root: Root::new(
            "fmodal-",
            &[
                "/bin/sh".into(),
                "-c".into(),
                "stty raw -echo; printf READY; exec cat".into(),
            ],
        )?,
        binary: binary.into(),
        viewers: Vec::new(),
        finished: false,
    };
    h.add(&[])?;
    h.wait(
        |h| Ok(h.text(0).contains("READY")),
        "modal fixture ready",
        8,
    )?;
    h.send(0, b"\x01|")?;
    h.state(|tabs| panes(&tabs[0]) == 2, "modal two panes", 8)?;
    h.wait(
        |h| Ok(h.bar(0).trim_end().ends_with('2')),
        "modal focus ready",
        8,
    )?;
    let layout = h.cli(&["layout", "1", "export"])?;
    let identities = |h: &Harness| -> Result<Value> {
        let tabs = h.tabs("default")?;
        Ok(
            serde_json::json!({"name":tabs[0]["name"], "panes":tabs[0]["panes"].as_array().context("panes")?.iter().map(|pane| serde_json::json!([pane["id"],pane["pid"],pane["label"]])).collect::<Vec<_>>()}),
        )
    };
    let original = identities(&h)?;
    for (index, (key, title)) in [
        (b';', "Rename pane"),
        (b',', "Rename tab"),
        (b'x', "Close pane"),
        (b'r', "Resize"),
        (b'[', "Copy ·"),
        (b'?', "Pane 2 actions"),
    ]
    .into_iter()
    .enumerate()
    {
        h.send(0, &[1, key])?;
        h.wait(|h| Ok(h.text(0).contains(title)), title, 8)?;
        h.send(0, b"\x1b")?;
        h.wait(
            |h| {
                let text = h.text(0);
                Ok(!text.contains(title)
                    && !text.contains("Esc dismiss")
                    && !text.contains("split side by side"))
            },
            "one Escape dismisses modal",
            8,
        )?;
        ensure!(
            !h.text(0).contains("split side by side"),
            "Escape opened command popup after {title}: {}",
            h.text(0)
        );
        let marker = format!("NORMAL-{index}");
        h.send(0, format!("\r\n{marker}\r\n").as_bytes())?;
        h.wait(
            |h| Ok(h.text(0).contains(&marker)),
            "normal application input after dismissal",
            8,
        )?;
        ensure!(
            h.text(0).matches(&marker).count() == 1,
            "normal input duplicated"
        );
        ensure!(
            identities(&h)? == original,
            "dismissal mutated pane/tab identity or label"
        );
        ensure!(
            h.cli(&["layout", "1", "export"])? == layout,
            "dismissal mutated layout"
        );
    }
    h.finish()?;
    println!("PASS one-Escape modal dismissal, normal input and no unintended mutations");
    Ok(())
}
