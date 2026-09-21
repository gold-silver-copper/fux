use super::*;

const CLEARED: &str = "selection cleared";

fn screen(s: &mut Server, f: usize) -> Result<String> {
    Ok(s.frontend(f)?.parser.screen().contents())
}
fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("selection viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn enter_copy_mode(s: &mut Server, f: usize, v: u64) -> Result<()> {
    s.send(f, b"\x02")?;
    s.wait("prefix open", |s| Ok(screen(s, f)?.contains("Panes")))?;
    s.send(f, b"c")?;
    s.wait("copy mode entered", |s| {
        Ok(notice_text(s, v)?.starts_with("Copy:"))
    })
}
/// Anchors a selection and extends it, so an invalidation has something to clear.
fn anchor(s: &mut Server, f: usize) -> Result<()> {
    s.send(f, b" ")?;
    for _ in 0..3 {
        s.send(f, b"\x1b[C")?;
    }
    std::thread::sleep(std::time::Duration::from_millis(80));
    s.pump()
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    child_command(
        s,
        f,
        "stty raw -echo; printf '\\033[2J\\033[H'; i=1; while [ $i -le 40 ]; do printf 'SEL-%02d\\r\\n' $i; i=$((i+1)); done; printf 'READY'; exec cat > /dev/null",
    )?;
    s.wait("selection source painted", |s| {
        Ok(screen(s, f)?.contains("READY"))
    })?;
    running(s)?;

    // Resizing under an anchored selection clears it with a visible notice,
    // rather than silently copying different text later.
    enter_copy_mode(s, f, v)?;
    anchor(s, f)?;
    s.resize(f, 20, 60)?;
    let mut notice = String::new();
    s.wait("resize cleared the selection", |s| {
        notice = notice_text(s, v)?;
        Ok(notice.contains(CLEARED))
    })
    .map_err(|e| {
        format!("application: resizing under an anchored selection should clear it with a notice, saw {notice:?}: {e}")
    })?;
    s.journal
        .record("resize_invalidation", json!({"notice":notice}))?;

    // Copying with no anchor asks for a selection instead of copying anything.
    let before = s.frontend(f)?.capture.total;
    s.send(f, b"y")?;
    let mut asked = String::new();
    s.wait("copy without an anchor reports", |s| {
        asked = notice_text(s, v)?;
        Ok(!asked.is_empty() && !asked.contains(CLEARED))
    })
    .map_err(|e| format!("application: copying with no selection reported nothing: {e}"))?;
    s.journal
        .record("copy_without_anchor", json!({"notice":asked}))?;
    let bytes = s.frontend(f)?.capture.bytes();
    let new = usize::try_from(s.frontend(f)?.capture.total.saturating_sub(before))?;
    let tail = bytes
        .get(bytes.len().saturating_sub(new)..)
        .unwrap_or_default();
    ensure(
        !tail.windows(5).any(|w| w == b"\x1b]52;"),
        "application: copying with no selection still wrote an OSC 52 sequence",
    )?;

    // Explicit scrolling under an anchored selection also clears it.
    s.send(f, b"q")?;
    std::thread::sleep(std::time::Duration::from_millis(80));
    s.pump()?;
    enter_copy_mode(s, f, v)?;
    anchor(s, f)?;
    s.send(f, b"u")?; // page up inside copy mode
    let mut scrolled = String::new();
    s.wait("scrolling cleared the selection", |s| {
        scrolled = notice_text(s, v)?;
        Ok(scrolled.contains(CLEARED) || scrolled.starts_with("Copy:"))
    })?;
    s.journal
        .record("scroll_invalidation", json!({"notice":scrolled}))?;

    // Leaving copy mode restores ordinary input to the pane.
    s.send(f, b"q")?;
    s.wait("copy mode exited", |s| {
        Ok(!notice_text(s, v)?.starts_with("Copy:"))
    })?;
    Ok(())
}
