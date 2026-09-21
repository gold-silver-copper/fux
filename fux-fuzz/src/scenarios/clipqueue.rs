use super::*;

const FULL: &str = "queue is full";
const DISABLED: &str = "clipboard disabled";

fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("clipboard viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn osc_count(paint: &str) -> usize {
    paint.matches("\x1b]52;").count()
}
/// One `copy` command, returning the notice it produced.
fn copy(s: &mut Server, w: u64) -> Result<String> {
    s.control(w, json!({"kind":"copy"}))?;
    let mut text = String::new();
    s.wait("copy reported", |s| {
        text = notice_text(s, w)?;
        Ok(!text.is_empty())
    })?;
    Ok(text)
}
fn paint(s: &mut Server, w: u64) -> Result<String> {
    Ok(s.rpc("fux.frame", json!({"viewer":w}))?
        .get("paint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    running(s)?;
    // An API-only viewer has no watch stream, so nothing paints for it until
    // asked. Queued copies therefore accumulate instead of draining.
    let w = s
        .rpc("fux.attach", json!({"rows":24,"cols":80}))?
        .get("viewer")
        .and_then(Value::as_u64)
        .ok_or("attach returned no viewer")?;
    let mut accepted = 0;
    let mut refused = None;
    for attempt in 1..=17 {
        let text = copy(s, w)?;
        if text.contains(FULL) {
            refused = Some(attempt);
            break;
        }
        ensure(
            text.contains("copied"),
            &format!(
                "application: copy {attempt} neither succeeded nor reported a full queue: {text:?}"
            ),
        )?;
        accepted += 1;
        // Only ordinary key input clears a notice; a lone Escape reaches the
        // shell as a byte it ignores, so the next wait sees its own result.
        s.rpc(
            "world.trigger_event",
            json!({"event":"fux::control::UserInput","value":{"viewer":w,"input":{"kind":"key","key":"escape","ctrl":false,"alt":false,"shift":false}}}),
        )?;
    }
    s.journal.record(
        "queue_fill",
        json!({"accepted":accepted,"refused_at":refused}),
    )?;
    ensure(
        accepted == 16 && refused == Some(17),
        &format!(
            "application: the clipboard queue should accept 16 copies and refuse the 17th; accepted {accepted}, refused at {refused:?}"
        ),
    )?;

    // One paint delivers all sixteen, in one frame; the next delivers none.
    let first = paint(s, w)?;
    let delivered = osc_count(&first);
    s.journal
        .record("queue_drain", json!({"delivered":delivered}))?;
    ensure(
        delivered == 16,
        &format!(
            "application: a paint after filling the queue carried {delivered} OSC 52 sequences, expected 16"
        ),
    )?;
    let second = paint(s, w)?;
    ensure(
        osc_count(&second) == 0,
        "application: a second paint re-delivered queued copies",
    )?;

    // Queue five more, then disable the clipboard by rewriting the config.
    // Pending copies are dropped, never delivered, and new copies are refused.
    for _ in 0..5 {
        let text = copy(s, w)?;
        ensure(
            text.contains("copied"),
            &format!("re-fill copy failed: {text:?}"),
        )?;
        s.rpc(
            "world.trigger_event",
            json!({"event":"fux::control::UserInput","value":{"viewer":w,"input":{"kind":"key","key":"escape","ctrl":false,"alt":false,"shift":false}}}),
        )?;
    }
    fs::write(
        s.directory.join("fux.json"),
        serde_json::to_vec(&json!({}))?,
    )?;
    s.journal
        .record("write_config", json!({"clipboard":"default (disabled)"}))?;
    let mut attempts = 0;
    let mut last = String::new();
    s.wait("clipboard disabled by reload", |s| {
        attempts += 1;
        last = copy(s, w)?;
        Ok(last.contains(DISABLED))
    })
    .map_err(|e| format!("application: rewriting fux.json without a clipboard key did not disable it (last notice {last:?}): {e}"))?;
    s.journal
        .record("disabled_after", json!({"attempts":attempts}))?;
    let after = paint(s, w)?;
    ensure(
        osc_count(&after) == 0,
        &format!(
            "application: copies queued before the clipboard was disabled were still delivered ({} sequences)",
            osc_count(&after)
        ),
    )?;
    s.control(w, json!({"kind":"detach"}))?;
    Ok(())
}
