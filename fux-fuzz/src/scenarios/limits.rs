use super::*;

fn viewer_state(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("viewer disappeared")?;
    component(row, VIEWER)
}
fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    Ok(viewer_state(s, viewer)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
/// An API-only viewer: no frontend process, so viewport limits can be probed
/// at sizes no real terminal would have.
fn api_attach(s: &mut Server, rows: u64, cols: u64) -> Result<u64> {
    s.rpc("fux.attach", json!({"rows":rows,"cols":cols}))?
        .get("viewer")
        .and_then(Value::as_u64)
        .ok_or("attach returned no viewer".into())
}
fn dims(s: &mut Server, viewer: u64) -> Result<(u64, u64)> {
    let state = viewer_state(s, viewer)?;
    Ok((
        state.get("rows").and_then(Value::as_u64).unwrap_or(0),
        state.get("cols").and_then(Value::as_u64).unwrap_or(0),
    ))
}
fn settled(s: &mut Server, viewer: u64, label: &str) -> Result<String> {
    let mut text = String::new();
    s.wait(label, |s| {
        text = notice_text(s, viewer)?;
        Ok(!text.is_empty())
    })?;
    Ok(text)
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let pids = running(s)?;
    let first_pane = *pids.first().ok_or("no first process")?;

    // Attach dimensions are clamped to the documented 4096 maximum rather than
    // wrapping or being rejected.
    let huge = api_attach(s, 99_999, 99_999)?;
    let clamped = dims(s, huge)?;
    s.journal.record(
        "attach_clamp",
        json!({"requested":99_999,"clamped":clamped}),
    )?;
    ensure(
        clamped == (4096, 4096),
        &format!("application: attach should clamp oversized dimensions to 4096, got {clamped:?}"),
    )?;

    // A pane's real size is the smallest across its viewers, so give this
    // viewer a workspace of its own: its pane is then sized by it alone and
    // the copy viewport genuinely exceeds the documented cell cap.
    s.control(huge, json!({"kind":"workspace_new","name":"huge"}))?;
    s.wait("huge workspace pane sized by its only viewer", |s| {
        Ok(states(s)?
            .iter()
            .any(|(_, state)| state.get("rows").and_then(Value::as_u64).unwrap_or(0) > 1000))
    })?;
    s.control(huge, json!({"kind":"copy_mode"}))?;
    let refused = settled(s, huge, "oversized copy viewport reported")?;
    s.journal
        .record("copy_cell_cap", json!({"notice":refused}))?;
    ensure(
        refused.contains("262144") || refused.to_lowercase().contains("cells"),
        &format!(
            "application: copy mode on an oversized viewport should report the cell cap, said {refused:?}"
        ),
    )?;
    ensure(
        alive(first_pane),
        "application: a refused copy viewport disturbed a process",
    )?;
    s.control(huge, json!({"kind":"detach"}))?;
    s.wait("huge viewer detached", |s| {
        Ok(!s
            .query(VIEWER)?
            .iter()
            .any(|row| id(row).ok() == Some(huge)))
    })?;

    // With the clipboard disabled by default, a copy is refused with the
    // documented reason and nothing is delivered to the outer terminal.
    let before = s.frontend(f)?.capture.total;
    s.control(v, json!({"kind":"copy"}))?;
    let disabled = settled(s, v, "disabled clipboard reported")?;
    s.journal
        .record("clipboard_disabled", json!({"notice":disabled}))?;
    ensure(
        disabled.contains("clipboard disabled"),
        &format!("application: copy with the clipboard disabled should say so, said {disabled:?}"),
    )?;
    let bytes = s.frontend(f)?.capture.bytes();
    let new = usize::try_from(s.frontend(f)?.capture.total.saturating_sub(before))?;
    let tail = bytes
        .get(bytes.len().saturating_sub(new)..)
        .unwrap_or_default();
    ensure(
        !tail.windows(5).any(|w| w == b"\x1b]52;"),
        "application: a refused copy still wrote an OSC 52 sequence",
    )?;

    // A zero-sized viewport is accepted and paints nothing, and copy mode on
    // it reports rather than capturing an empty grid.
    let zero = api_attach(s, 0, 0)?;
    ensure(
        s.frame(zero, 1, 1)?.is_empty(),
        "application: a zero viewport painted content",
    )?;
    s.control(zero, json!({"kind":"copy_mode"}))?;
    let zero_notice = settled(s, zero, "zero viewport copy mode reported")?;
    s.journal
        .record("zero_copy_mode", json!({"notice":zero_notice}))?;
    ensure(
        !zero_notice.starts_with("Copy:"),
        &format!("application: copy mode started on a zero-sized viewport: {zero_notice:?}"),
    )?;
    s.control(zero, json!({"kind":"detach"}))?;

    // An oversized paste is refused by the documented bound, and the pane
    // receives nothing.
    let over = "x".repeat(64 * 1024 + 1);
    s.rpc(
        "world.trigger_event",
        json!({"event":"fux::control::UserInput","value":{"viewer":v,"input":{"kind":"paste","text":over}}}),
    )?;
    let paste_notice = settled(s, v, "oversized paste reported")?;
    ensure(
        paste_notice.contains("64 KiB"),
        &format!(
            "application: an oversized paste should report the 64 KiB bound, said {paste_notice:?}"
        ),
    )?;

    // The server stays healthy and the original process is untouched.
    ensure(
        alive(first_pane),
        "application: probing limits terminated the original process",
    )?;
    s.wait("viewer still usable", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    Ok(())
}
