use super::*;

pub(super) fn run(binary: &Path) -> Result<()> {
    let mut h = Harness {
        root: Root::new(
            "fgesture-",
            &[
                "/bin/sh".into(),
                "-c".into(),
                "printf READY; exec cat".into(),
            ],
        )?,
        binary: binary.into(),
        viewers: Vec::new(),
        finished: false,
    };
    h.add(&[])?;
    h.wait(
        |h| Ok(h.text(0).contains("READY")),
        "gesture fixture ready",
        8,
    )?;
    h.send(0, b"\x01|")?;
    h.state(|tabs| panes(&tabs[0]) == 2, "gesture two panes", 8)?;
    verify(&mut h).context("gesture assertions")?;
    h.finish().context("gesture fixture cleanup")?;
    println!("PASS fresh-press recovery, canceled layout and auxiliary gesture ownership");
    Ok(())
}

pub(super) fn verify(h: &mut Harness) -> Result<()> {
    let original_layout = h.cli(&["layout", "1", "export"])?;
    h.send(0, b"\x1b[<8;3;3M\x1b[<40;60;3M")?;
    h.wait(
        |h| Ok(h.text(0).contains("release to apply")),
        "unreleased layout preview",
        8,
    )?;
    h.send(0, b"\x1b[<0;60;3M\x1b[<0;60;3m")?;
    h.wait(
        |h| Ok(!h.text(0).contains("release to apply") && h.bar(0).trim_end().ends_with('2')),
        "fresh click abandons old preview and focuses B",
        8,
    )?;
    ensure!(
        h.cli(&["layout", "1", "export"])? == original_layout,
        "fresh click committed the stale layout preview"
    );
    h.send(0, b"\x1b[<2;3;3M")?;
    h.wait(
        |h| Ok(h.text(0).contains("Pane 1 actions")),
        "right menu before missing release",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("Pane 1 actions")),
        "right menu dismissed without release",
        8,
    )?;
    h.send(0, b"\x1b[<2;60;3M\x1b[<2;60;3m")?;
    h.wait(
        |h| Ok(h.text(0).contains("Pane 2 actions")),
        "fresh right press opens B menu",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("Pane 2 actions")),
        "recovered menu dismisses normally",
        8,
    )?;
    Ok(())
}
