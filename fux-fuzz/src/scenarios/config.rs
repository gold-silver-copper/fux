use super::*;

const CAPTURE: &str = "keys.bin";
/// A group heading of the prefix command column, which only fux paints.
const COLUMN: &str = "Panes";

fn write_config(s: &mut Server, value: &str) -> Result<()> {
    s.journal.record("write_config", json!(value))?;
    fs::write(s.directory.join("fux.json"), value)?;
    Ok(())
}
fn captured(s: &mut Server) -> Result<Vec<u8>> {
    Ok(fs::read(s.directory.join(CAPTURE))?)
}
fn column_open(s: &mut Server, viewer: u64) -> Result<bool> {
    Ok(s.frame(viewer, 24, 80)?.contains(COLUMN))
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    child_command(
        s,
        f,
        &format!("stty raw -echo; printf '\\033[2J\\033[HCONFIG-READY'; exec cat > {CAPTURE}"),
    )?;
    s.wait("capture child ready", |s| {
        Ok(s.frame(v, 24, 80)?.starts_with("CONFIG-READY") && s.directory.join(CAPTURE).is_file())
    })?;
    running(s)?;

    // The default prefix is Ctrl-B: it opens the command column, and pressed
    // twice it sends exactly one literal 0x02 to the pane.
    s.send(f, b"\x02")?;
    s.wait("default prefix opens the command column", |s| {
        column_open(s, v)
    })?;
    ensure(
        captured(s)?.is_empty(),
        "application: the prefix key itself reached the pane",
    )?;
    s.send(f, b"\x02")?;
    s.wait("doubled default prefix sends one literal byte", |s| {
        Ok(captured(s)? == b"\x02")
    })
    .map_err(|e| format!("application: doubled default prefix: {e}"))?;

    // Hot reload to a different prefix, keeping the default binding list.
    // Confirm the reload through the command column, which cannot be confused
    // with a key that merely passed through to the pane.
    write_config(s, r#"{"prefix":"ctrl-a"}"#)?;
    let mut attempts = 0;
    s.wait("reconfigured prefix applied", |s| {
        if column_open(s, v)? {
            return Ok(true);
        }
        attempts += 1;
        s.send(f, b"\x01")?;
        column_open(s, v)
    })
    .map_err(|e| {
        format!("application: after setting prefix to ctrl-a, Ctrl-A never opened the command column: {e}")
    })?;
    s.send(f, b"\x1b")?; // Esc closes the column
    s.wait("column closed", |s| Ok(!column_open(s, v)?))?;
    let base = captured(s)?;
    s.journal.record(
        "prefix_reload",
        json!({"attempts":attempts,"captured":base.len()}),
    )?;

    // Ctrl-B is now ordinary input and reaches the pane unchanged.
    let mut want = base.clone();
    want.push(0x02);
    s.send(f, b"\x02")?;
    s.wait("old prefix is now ordinary input", |s| {
        Ok(captured(s)? == want)
    })
    .map_err(|e| {
        format!("application: after the prefix changed, Ctrl-B should reach the pane: {e}")
    })?;

    // Doubling the configured prefix sends exactly one literal 0x01.
    s.send(f, b"\x01")?;
    s.wait("configured prefix opens the column", |s| column_open(s, v))?;
    ensure(
        captured(s)? == want,
        "application: the configured prefix key itself reached the pane",
    )?;
    s.send(f, b"\x01")?;
    want.push(0x01);
    s.wait("doubled configured prefix sends one literal byte", |s| {
        Ok(captured(s)? == want)
    })
    .map_err(|e| format!("application: doubling the configured prefix: {e}"))?;

    // A default binding still runs under the new prefix, and its key never
    // reaches the pane.
    let leaves = |s: &mut Server| -> Result<usize> { Ok(s.query("fux::model::PaneView")?.len()) };
    let before = leaves(s)?;
    let capture_leaf = s.relation(v, "fux::model::Focused")?;
    s.send(f, b"\x01")?;
    s.wait("column open for the binding", |s| column_open(s, v))?;
    s.send(f, b"h")?;
    s.wait("binding executed under the new prefix", |s| {
        Ok(leaves(s)? > before)
    })
    .map_err(|e| format!("application: ctrl-a h did not split the pane: {e}"))?;
    ensure(
        captured(s)? == want,
        "application: a binding's key also reached the pane",
    )?;

    // Invalid configuration keeps the previous usable one, so the prefix stays
    // ctrl-a rather than reverting to the default.
    write_config(s, "{ not valid json")?;
    // The split focused the new pane; return to the capture child.
    s.control(v, json!({"kind":"focus","pane":capture_leaf}))?;
    s.wait("capture pane refocused", |s| {
        Ok(s.relation(v, "fux::model::Focused")? == capture_leaf)
    })?;
    let mut after = want.clone();
    after.push(0x02);
    s.send(f, b"\x02")?;
    s.wait("malformed reload keeps the previous prefix", |s| {
        Ok(captured(s)?.starts_with(&after))
    })
    .map_err(|e| {
        format!("application: after a malformed reload, Ctrl-B should still be ordinary input: {e}")
    })?;
    Ok(())
}
