use super::*;

const COLS: u16 = 260;

fn screen(s: &mut Server, f: usize) -> Result<String> {
    Ok(s.frontend(f)?.parser.screen().contents())
}
fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("mouse viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn outer(code: u16, x: u16, y: u16, release: bool) -> Vec<u8> {
    format!(
        "\x1b[<{code};{};{}{}",
        x + 1,
        y + 1,
        if release { 'm' } else { 'M' }
    )
    .into_bytes()
}
fn legacy(code: u8, col: u16, row: u16) -> Vec<u8> {
    vec![
        27,
        b'[',
        b'M',
        code + 32,
        (col as u8).wrapping_add(32),
        (row as u8).wrapping_add(32),
    ]
}
fn file(s: &mut Server, name: &str) -> Result<Vec<u8>> {
    Ok(fs::read(s.directory.join(name)).unwrap_or_default())
}
/// Sends a forwarded probe press and waits for it, proving earlier events that
/// should have been dropped were not merely late.
fn barrier(
    s: &mut Server,
    f: usize,
    name: &str,
    expected: &mut Vec<u8>,
    probe_out: &[u8],
    probe_in: &[u8],
) -> Result<()> {
    s.send(f, probe_in)?;
    expected.extend_from_slice(probe_out);
    let mut actual = Vec::new();
    s.wait("mouse barrier delivered", |s| {
        actual = file(s, name)?;
        Ok(actual.len() >= expected.len())
    })?;
    ensure(
        actual == *expected,
        &format!(
            "application: pane {name} received {:?}, expected {:?}",
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(expected)
        ),
    )
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, COLS)?;
    let v = s.frontend(f)?.viewer;
    // Tab one: legacy encoding only. Tab two: SGR. Each pane is full width, so
    // pane-relative columns reach beyond the legacy encoding's 223 limit.
    child_command(
        s,
        f,
        "stty raw -echo; printf '\\033[?1000h\\033[2J\\033[HLEGACY'; exec cat > legacy.bin",
    )?;
    s.wait("legacy pane ready", |s| {
        Ok(screen(s, f)?.starts_with("LEGACY") && s.directory.join("legacy.bin").is_file())
    })?;
    running(s)?;
    let mut legacy_expected = Vec::new();

    // A press beyond column 223 cannot be encoded in legacy mode; the standard
    // has no byte for it, so nothing may be delivered.
    s.send(f, &outer(0, 250, 1, false))?;
    s.send(f, &outer(0, 250, 1, true))?;
    // Within range the legacy bytes are delivered, including columns above 127.
    barrier(
        s,
        f,
        "legacy.bin",
        &mut legacy_expected,
        &legacy(0, 150, 2),
        &outer(0, 149, 1, false),
    )?;
    barrier(
        s,
        f,
        "legacy.bin",
        &mut legacy_expected,
        &legacy(3, 150, 2),
        &outer(0, 149, 1, true),
    )?;

    // Shift-right-click when the application owns the mouse opens fux's menu
    // instead of reaching the pane.
    s.send(f, &outer(6, 10, 3, false))?;
    s.wait("shift-right-click opened fux's pane menu", |s| {
        let seen = screen(s, f)?.to_lowercase();
        Ok(seen.contains("close") || seen.contains("rename") || seen.contains("terminate"))
    })
    .map_err(|e| format!("application: shift-right-click did not open the pane menu while the app owned the mouse: {e}"))?;
    s.send(f, &outer(6, 10, 3, true))?;
    s.send(f, b"\x1b")?;
    std::thread::sleep(std::time::Duration::from_millis(80));
    s.pump()?;
    barrier(
        s,
        f,
        "legacy.bin",
        &mut legacy_expected,
        &legacy(0, 1, 1),
        &outer(0, 0, 0, false),
    )?;

    // Wheel events on the bar row never reach the pane.
    s.send(f, &outer(64, 5, 23, false))?;
    s.send(f, &outer(65, 5, 23, false))?;
    barrier(
        s,
        f,
        "legacy.bin",
        &mut legacy_expected,
        &legacy(0, 2, 2),
        &outer(0, 1, 1, false),
    )?;

    // API mouse input outside the viewer, and on the bar row, is ignored
    // without an error.
    for (x, y) in [(COLS, 0u16), (0, 24u16), (COLS, 24), (5, 23)] {
        s.rpc(
            "world.trigger_event",
            json!({"event":"fux::control::UserInput","value":{"viewer":v,"input":{"kind":"mouse","action":"press","button":"left","x":x,"y":y,"ctrl":false,"alt":false,"shift":false}}}),
        )?;
    }
    barrier(
        s,
        f,
        "legacy.bin",
        &mut legacy_expected,
        &legacy(0, 3, 3),
        &outer(0, 2, 2, false),
    )?;
    let text = notice_text(s, v)?;
    ensure(
        !text.to_lowercase().contains("error") && !text.contains("panic"),
        &format!("application: out-of-range API mouse input raised a notice: {text:?}"),
    )?;

    // SGR encoding delivers the wide column exactly.
    s.control(v, json!({"kind":"tab_new","name":"sgr"}))?;
    s.wait("second tab", |s| Ok(s.query("fux::model::Tab")?.len() == 2))?;
    child_command(
        s,
        f,
        "stty raw -echo; printf '\\033[?1000h\\033[?1006h\\033[2J\\033[HSGR'; exec cat > sgr.bin",
    )?;
    s.wait("sgr pane ready", |s| {
        Ok(screen(s, f)?.starts_with("SGR") && s.directory.join("sgr.bin").is_file())
    })?;
    let mut sgr_expected = Vec::new();
    barrier(
        s,
        f,
        "sgr.bin",
        &mut sgr_expected,
        b"\x1b[<0;251;2M",
        &outer(0, 250, 1, false),
    )?;
    barrier(
        s,
        f,
        "sgr.bin",
        &mut sgr_expected,
        b"\x1b[<0;251;2m",
        &outer(0, 250, 1, true),
    )?;
    Ok(())
}
