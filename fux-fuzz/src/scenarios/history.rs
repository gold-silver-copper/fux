use super::copy::{COPIED, base64};
use super::*;

const LINES: usize = 60;
const CONTENT_ROWS: usize = 23;

fn top_line(frame: &str) -> String {
    frame
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_owned()
}
fn line(n: usize) -> String {
    format!("LINE-{n:03}")
}
fn scrollback(s: &mut Server, viewer: u64) -> Result<u64> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("history viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .get("scrollback")
        .and_then(Value::as_u64)
        .unwrap_or(0))
}
fn notice(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("history viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .get("notice")
        .cloned()
        .unwrap_or(Value::Null))
}
fn expect_top(s: &mut Server, viewer: u64, label: &str, n: usize) -> Result<()> {
    let wanted = line(n);
    let mut top = String::new();
    let result = s.wait(label, |s| {
        top = top_line(&s.frame(viewer, 24, 80)?);
        Ok(top == wanted)
    });
    let offset = scrollback(s, viewer)?;
    s.journal.record(
        "history_top",
        json!({"label":label,"wanted":wanted,"top":top,"scrollback":offset}),
    )?;
    result.map_err(|e| {
        format!("application: {label}: top row should be {wanted} but is {top:?} (Viewer.scrollback {offset}): {e}").into()
    })
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let a = s.attach(24, 80)?;
    let va = s.frontend(a)?.viewer;
    // Sixty numbered lines and a final marker without a trailing newline, so
    // the live view shows LINE-039..LINE-060 and READY, with 38 lines of history.
    let body: String = (1..=LINES).map(|n| format!("{}\\r\\n", line(n))).collect();
    child_command(
        s,
        a,
        &format!("stty raw -echo; printf '\\033[2J\\033[H{body}READY'; exec cat"),
    )?;
    let first_visible = LINES + 1 - CONTENT_ROWS + 1; // 39
    s.wait("history source painted", |s| {
        let frame = s.frame(va, 24, 80)?;
        Ok(top_line(&frame) == line(first_visible) && frame.contains("\nREADY"))
    })?;
    running(s)?;
    ensure(scrollback(s, va)? == 0, "live view has nonzero scrollback")?;

    // Command scrolling moves by half the viewer height (12 rows).
    s.control(va, json!({"kind":"scroll","order":"previous"}))?;
    expect_top(s, va, "scroll previous once", first_visible - 12)?;
    ensure(scrollback(s, va)? == 12, "scrollback should be 12")?;
    s.control(va, json!({"kind":"scroll","order":"previous"}))?;
    expect_top(s, va, "scroll previous twice", first_visible - 24)?;
    // Far beyond history clamps at the oldest line.
    for _ in 0..5 {
        s.control(va, json!({"kind":"scroll","order":"previous"}))?;
    }
    expect_top(s, va, "scroll clamped at oldest", 1)?;
    s.control(va, json!({"kind":"scroll","order":"next"}))?;
    expect_top(s, va, "scroll next from oldest", 13)?;
    // Ordinary input returns to live output.
    s.send(a, b"Q")?;
    expect_top(s, va, "key returns to live", first_visible)?;
    s.wait("typed key echoed by cat", |s| {
        Ok(s.frame(va, 24, 80)?.contains("READYQ"))
    })?;

    // The wheel over a pane without application mouse mode browses history.
    s.send(a, b"\x1b[<64;10;10M")?;
    expect_top(s, va, "wheel up", first_visible - 12)?;
    s.send(a, b"\x1b[<65;10;10M")?;
    expect_top(s, va, "wheel down", first_visible)?;

    // Copy mode: `u` pages up by one row less than the height; `k` at the top
    // row scrolls one line; a history line copies exactly and copying returns
    // to live output.
    s.send(a, b"\x02c")?;
    s.wait("copy mode", |s| {
        Ok(notice(s, va)?
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| t.starts_with("Copy:")))
    })?;
    s.send(a, b"u")?;
    expect_top(
        s,
        va,
        "copy-mode page up",
        first_visible - (CONTENT_ROWS - 1),
    )?;
    s.send(a, b"k")?;
    expect_top(
        s,
        va,
        "copy-mode line up at top",
        first_visible - CONTENT_ROWS,
    )?;
    let copied = line(first_visible - CONTENT_ROWS);
    s.send(a, b" ")?;
    s.send(a, b"\x1b[F")?; // End: to the last column of the row
    let wanted = format!("\x1b]52;c;{}\x07", base64(copied.as_bytes())).into_bytes();
    let before = s.frontend(a)?.capture.total;
    s.send(a, b"y")?;
    s.wait("history line copied", |s| {
        let bytes = s.frontend(a)?.capture.bytes();
        let new = usize::try_from(s.frontend(a)?.capture.total - before)?;
        Ok(bytes
            .get(bytes.len().saturating_sub(new)..)
            .unwrap_or_default()
            .windows(wanted.len())
            .any(|w| w == wanted))
    })
    .map_err(|e| format!("application: copying {copied} from history: {e}"))?;
    expect_top(s, va, "copy returns to live", first_visible)?;
    ensure(
        notice(s, va)?.get("text") == Some(&json!(COPIED)),
        "copy success not reported",
    )?;

    // New output while a selection is anchored clears it with a notice, and
    // the next copy asks for a new selection instead of copying stale text.
    let b = s.attach(24, 80)?;
    s.send(a, b"\x02c")?;
    s.wait("copy mode again", |s| {
        Ok(notice(s, va)?
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| t.starts_with("Copy:")))
    })?;
    s.send(a, b" ")?;
    s.send(a, b"\x1b[C")?;
    s.send(b, b"Z")?;
    s.wait("other viewer's key echoed", |s| {
        Ok(s.frame(va, 24, 80)?.contains("READYQZ"))
    })?;
    s.send(a, b"y")?;
    let mut last = Value::Null;
    let result = s.wait("stale selection refused", |s| {
        last = notice(s, va)?;
        let text = last.get("text").and_then(Value::as_str).unwrap_or_default();
        Ok(text.starts_with("selection cleared") || text == "Space starts a selection")
    });
    s.journal
        .record("stale_selection", json!({"notice":last}))?;
    result.map_err(|e| {
        format!("application: a selection anchored before new output must be cleared, not copied: notice {last}: {e}").into()
    })
}
