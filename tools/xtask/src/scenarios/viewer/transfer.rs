use super::*;

/// Exercise input after two workspace moves with enough observation time for source cleanup.
pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new(
        "fmove-input-",
        &[
            "/bin/sh".into(),
            "-c".into(),
            "printf COPY_TARGET; exec cat".into(),
        ],
    )?;
    let mut h = Harness {
        root,
        binary: binary.into(),
        viewers: Vec::new(),
        finished: false,
    };
    h.add(&[])?;
    h.wait(
        |h| Ok(h.text(0).contains("COPY_TARGET")),
        "source ready",
        10,
    )?;
    h.cli(&["workspace", "new", "other"])?;
    h.send(0, b"\x01yfollowed\r")?;
    h.wait(
        |h| Ok(h.bar(0).starts_with(" followed")),
        "first workspace move",
        10,
    )?;
    h.hold(|_| true, 1.0)?;
    h.send(0, b"\x01i")?;
    h.wait(
        |h| Ok(h.text(0).contains("to workspace")),
        "existing destination ready",
        10,
    )?;
    h.send(0, b"\r")?;
    h.wait(
        |h| Ok(h.bar(0).starts_with(" other")),
        "second workspace move",
        10,
    )?;
    h.send(0, b"FOLLOW_STILL_ALIVE\r")?;
    h.wait(
        |h| Ok(h.text(0).contains("FOLLOW_STILL_ALIVE")),
        "input after two moves",
        10,
    )?;
    h.finish()?;
    println!("PASS input after two live workspace moves");
    Ok(())
}
