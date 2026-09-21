use super::*;

const COLUMN: &str = "Panes";
const CAPTURE: &str = "overlay.bin";

/// What the user actually sees, from the frontend's own screen. Reading this
/// instead of polling `fux.frame` once per poll avoids perturbing the server's
/// paint coalescing, and is the same surface a person would judge.
fn screen(s: &mut Server, f: usize) -> Result<String> {
    Ok(s.frontend(f)?.parser.screen().contents())
}
fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("overlay viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn leaves(s: &mut Server) -> Result<usize> {
    Ok(s.query("fux::model::PaneView")?.len())
}
fn captured(s: &mut Server) -> Result<Vec<u8>> {
    Ok(fs::read(s.directory.join(CAPTURE))?)
}
/// Presses the prefix and waits for the column. A brief settle before polling
/// lets the frontend deliver the paint; polling from the same instant starves
/// that delivery and is a harness artefact, not a property under test.
fn open_column(s: &mut Server, f: usize) -> Result<()> {
    // Idempotent: pressing the prefix while the column is already open is the
    // documented doubled-prefix path, which closes it and sends a literal byte.
    if screen(s, f)?.contains(COLUMN) {
        return Ok(());
    }
    s.send(f, b"\x02")?;
    let mut seen = String::new();
    s.wait("command column open", |s| {
        seen = screen(s, f)?;
        Ok(seen.contains(COLUMN))
    })
    .map_err(|e| -> Box<dyn std::error::Error> {
        let shown: Vec<&str> = seen
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .collect();
        format!("command column did not open: {e}; seen: {shown:?}").into()
    })
}
/// A lone Escape uses a 35 ms disambiguation deadline, so the next key must
/// not follow inside that window or the decoder reads one Alt-modified key.
fn escape(s: &mut Server, f: usize) -> Result<()> {
    s.send(f, b"\x1b")?;
    std::thread::sleep(std::time::Duration::from_millis(80));
    s.pump()
}
fn column_closed(s: &mut Server, f: usize) -> Result<bool> {
    Ok(!screen(s, f)?.contains(COLUMN))
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    child_command(
        s,
        f,
        &format!("stty raw -echo; printf '\\033[2J\\033[HOVERLAY-READY'; exec cat > {CAPTURE}"),
    )?;
    s.wait("overlay child ready", |s| {
        Ok(screen(s, f)?.starts_with("OVERLAY-READY") && s.directory.join(CAPTURE).is_file())
    })?;
    running(s)?;
    let leaf = s.relation(v, "fux::model::Focused")?;

    // An unavailable action explains why without acting: with one pane, the
    // next-pane binding must report a reason and change nothing.
    ensure(leaves(s)? == 1, "expected exactly one pane")?;
    open_column(s, f)?;
    ensure(
        captured(s)?.is_empty(),
        "application: the prefix key itself reached the pane",
    )?;
    s.send(f, b"\t")?;
    let mut reason = String::new();
    s.wait("unavailable action reported", |s| {
        reason = notice_text(s, v)?;
        Ok(!reason.is_empty())
    })
    .map_err(|e| format!("application: next pane with one pane reported nothing: {e}"))?;
    s.journal
        .record("unavailable_action", json!({"notice":reason}))?;
    ensure(
        reason.contains("one pane"),
        &format!("application: next pane with one pane should explain why, said {reason:?}"),
    )?;
    ensure(
        leaves(s)? == 1 && s.relation(v, "fux::model::Focused")? == leaf,
        "application: an unavailable action changed the layout",
    )?;
    ensure(
        captured(s)?.is_empty(),
        "application: a prefix shortcut key reached the pane",
    )?;

    // An unknown shortcut key leaves the column open.
    open_column(s, f)?;
    s.send(f, b"\x1b[24~")?; // F12, not bound by default
    s.wait("unknown shortcut leaves the column open", |s| {
        Ok(screen(s, f)?.contains(COLUMN))
    })
    .map_err(|e| format!("application: an unknown shortcut closed the command column: {e}"))?;
    ensure(
        captured(s)?.is_empty(),
        "application: an unknown shortcut key reached the pane",
    )?;
    escape(s, f)?;
    s.wait("escape closes the column", |s| column_closed(s, f))?;

    // A rename prompt: typing then Enter renames, and the bar shows it.
    open_column(s, f)?;
    s.send(f, b"r")?;
    s.wait("rename prompt open", |s| {
        Ok(screen(s, f)?.to_lowercase().contains("name"))
    })?;
    s.send(f, b"RENAMED")?;
    s.send(f, b"\r")?;
    s.wait("rename applied", |s| Ok(screen(s, f)?.contains("RENAMED")))
        .map_err(|e| format!("application: rename prompt did not apply: {e}"))?;
    ensure(
        captured(s)?.is_empty(),
        "application: prompt text reached the pane",
    )?;

    // Escape cancels a rename without changing the name.
    open_column(s, f)?;
    s.send(f, b"r")?;
    s.wait("rename prompt open again", |s| {
        Ok(screen(s, f)?.to_lowercase().contains("name"))
    })?;
    s.send(f, b"DISCARDED")?;
    // Wait for the typed text to be painted before asserting it disappears.
    s.wait("discarded text visible", |s| {
        Ok(screen(s, f)?.contains("DISCARDED"))
    })?;
    escape(s, f)?;
    s.wait("rename cancelled", |s| {
        let frame = screen(s, f)?;
        Ok(frame.contains("RENAMED") && !frame.contains("DISCARDED"))
    })
    .map_err(|e| format!("application: escape did not cancel the rename: {e}"))?;

    // A confirmation: `n` cancels and the pane survives.
    open_column(s, f)?;
    s.send(f, b"x")?;
    s.wait("close confirmation open", |s| {
        Ok(screen(s, f)?.to_lowercase().contains("close"))
    })?;
    s.send(f, b"n")?;
    s.wait("confirmation cancelled", |s| {
        Ok(leaves(s)? == 1 && column_closed(s, f)?)
    })
    .map_err(|e| format!("application: `n` did not cancel the close confirmation: {e}"))?;
    ensure(
        captured(s)?.is_empty(),
        "application: a confirmation key reached the pane",
    )?;

    // Escape also cancels a confirmation.
    open_column(s, f)?;
    s.send(f, b"x")?;
    s.wait("confirmation open again", |s| {
        Ok(screen(s, f)?.to_lowercase().contains("close"))
    })?;
    escape(s, f)?;
    s.wait("escape cancelled the confirmation", |s| {
        Ok(leaves(s)? == 1 && column_closed(s, f)?)
    })?;

    // A chooser cancels on `q` without creating anything.
    let tabs_before = s.query("fux::model::Tab")?.len();
    open_column(s, f)?;
    s.send(f, b"T")?;
    s.wait("tab chooser open", |s| {
        Ok(screen(s, f)?.to_lowercase().contains("tab"))
    })?;
    s.send(f, b"q")?;
    s.wait("chooser cancelled", |s| {
        Ok(s.query("fux::model::Tab")?.len() == tabs_before && column_closed(s, f)?)
    })
    .map_err(|e| format!("application: `q` did not cancel the tab chooser: {e}"))?;
    ensure(
        captured(s)?.is_empty(),
        "application: a chooser key reached the pane",
    )?;

    // Pasted text never becomes menu commands: with the column open, a paste
    // whose characters are bound shortcuts must not execute any of them.
    open_column(s, f)?;
    let before = leaves(s)?;
    s.send(f, b"\x1b[200~hvxt\x1b[201~")?;
    escape(s, f)?;
    s.wait("paste settled", |s| column_closed(s, f))?;
    let after = leaves(s)?;
    let tabs_after = s.query("fux::model::Tab")?.len();
    s.journal.record(
        "paste_in_column",
        json!({"panes_before":before,"panes_after":after,"tabs":tabs_after}),
    )?;
    ensure(
        after == before,
        &format!("application: a paste executed menu commands: pane count {before} became {after}"),
    )?;
    ensure(
        tabs_after == tabs_before,
        "application: a paste created a tab through a menu shortcut",
    )?;
    Ok(())
}
