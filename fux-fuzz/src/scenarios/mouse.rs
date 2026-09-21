use super::*;

/// One pane that requested a mouse protocol, located by the marker it painted.
struct Pane {
    file: &'static str,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

/// The outer terminal always speaks SGR to the frontend (it enables 1003/1006).
fn outer(code: u16, x: u16, y: u16, release: bool) -> Vec<u8> {
    format!(
        "\x1b[<{code};{};{}{}",
        x + 1,
        y + 1,
        if release { 'm' } else { 'M' }
    )
    .into_bytes()
}

/// What xterm semantics say the application should receive for an event at a
/// pane-relative one-based cell, given the protocol the child requested. This
/// is the standard's table, written independently of the application.
fn expected(mode: u16, sgr: bool, code: u16, col: u16, row: u16, release: bool) -> Vec<u8> {
    let motion = code & 32 != 0;
    let button = code & 3;
    let scroll = code & 64 != 0;
    let wanted = match mode {
        1000 => !motion,
        1002 => !motion || button != 3,
        _ => true,
    };
    if !wanted || scroll && release {
        return Vec::new();
    }
    if sgr {
        format!(
            "\x1b[<{code};{col};{row}{}",
            if release { 'm' } else { 'M' }
        )
        .into_bytes()
    } else {
        let byte = if release { 3 + (code & !3 & 31) } else { code };
        vec![
            27,
            b'[',
            b'M',
            byte as u8 + 32,
            col as u8 + 32,
            row as u8 + 32,
        ]
    }
}

fn locate(frame: &str, marker: &str) -> Result<(u16, u16)> {
    for (y, line) in frame.lines().enumerate() {
        if let Some(index) = line.find(marker) {
            let x = line.get(..index).ok_or("marker index")?.chars().count();
            return Ok((u16::try_from(x)?, u16::try_from(y)?));
        }
    }
    Err(format!("{marker} not painted: {frame:?}").into())
}

pub(super) fn run(s: &mut Server, mode: u16, sgr: bool, split: bool) -> Result<()> {
    let f = s.attach(24, 80)?;
    let viewer = s.frontend(f)?.viewer;
    let request = format!("\\033[?{mode}h{}", if sgr { "\\033[?1006h" } else { "" });
    child_command(
        s,
        f,
        &format!("stty raw -echo; printf '{request}\\033[2J\\033[HPANE-A'; exec cat > mouse-a"),
    )?;
    s.wait("mouse pane A ready", |s| {
        Ok(s.frame(viewer, 24, 80)?.starts_with("PANE-A") && s.directory.join("mouse-a").is_file())
    })?;
    if split {
        s.control(viewer, json!({"kind":"split","axis":"horizontal","program":format!("stty raw -echo; printf '{request}\\033[2J\\033[HPANE-B'; exec cat > mouse-b")}))?;
        s.wait("mouse pane B ready", |s| {
            Ok(
                s.frame(viewer, 24, 80)?.contains("PANE-B")
                    && s.directory.join("mouse-b").is_file(),
            )
        })?;
    }
    running(s)?;
    let frame = s.frame(viewer, 24, 80)?;
    let (ax, ay) = locate(&frame, "PANE-A")?;
    ensure((ax, ay) == (0, 0), "pane A is not at the top-left cell")?;
    let mut panes = vec![Pane {
        file: "mouse-a",
        x: 0,
        y: 0,
        w: 80,
        h: 23,
    }];
    let mut separator = None;
    if split {
        let (bx, by) = locate(&frame, "PANE-B")?;
        ensure(bx > 1 || by > 1, "pane B overlaps pane A")?;
        let first = panes.first_mut().ok_or("no pane A")?;
        if by == 0 {
            // Side by side, one separator column between them.
            first.w = bx - 1;
            separator = Some((bx - 1, 5));
            panes.push(Pane {
                file: "mouse-b",
                x: bx,
                y: 0,
                w: 80 - bx,
                h: 23,
            });
        } else {
            first.h = by - 1;
            separator = Some((5, by - 1));
            panes.push(Pane {
                file: "mouse-b",
                x: 0,
                y: by,
                w: 80,
                h: 23 - by,
            });
        }
    }
    s.journal.record(
        "mouse_layout",
        json!({"mode":mode,"sgr":sgr,"panes":panes.iter().map(|p| json!({"file":p.file,"x":p.x,"y":p.y,"w":p.w,"h":p.h})).collect::<Vec<_>>(),"separator":separator}),
    )?;

    // (outer bytes, pane index, expected pane bytes)
    let mut events: Vec<(Vec<u8>, Option<usize>, Vec<u8>)> = Vec::new();
    for (index, pane) in panes.iter().enumerate() {
        let mut at = |code: u16, dx: u16, dy: u16, release: bool| {
            let (x, y) = (pane.x + dx, pane.y + dy);
            let want = expected(mode, sgr, code, dx + 1, dy + 1, release);
            events.push((outer(code, x, y, release), Some(index), want));
        };
        at(0, 0, 0, false);
        at(0, 0, 0, true);
        at(0, pane.w - 1, pane.h - 1, false);
        at(0, pane.w - 1, pane.h - 1, true);
        at(2, 1, 1, false);
        at(2, 1, 1, true);
        at(1, 2, 2, false);
        at(1, 2, 2, true);
        at(16, 0, 1, false);
        at(16, 0, 1, true);
        at(8, 1, 0, false);
        at(8, 1, 0, true);
        at(0, 3, 3, false);
        at(32, 4, 4, false);
        at(32, 5, 5, false);
        at(0, 5, 5, true);
        at(35, 6, 6, false);
        at(64, 1, 1, false);
        at(65, 1, 1, false);
        at(80, 1, 1, false);
    }
    // The bar and a separator belong to fux, never to an application.
    events.push((outer(0, 10, 23, false), None, Vec::new()));
    events.push((outer(0, 10, 23, true), None, Vec::new()));
    if let Some((x, y)) = separator {
        events.push((outer(0, x, y, false), None, Vec::new()));
        events.push((outer(0, x, y, true), None, Vec::new()));
    }
    // A final forwarded event per pane flushes anything wrongly forwarded.
    for (index, pane) in panes.iter().enumerate() {
        events.push((
            outer(0, pane.x, pane.y, false),
            Some(index),
            expected(mode, sgr, 0, 1, 1, false),
        ));
    }

    let mut files: Vec<Vec<u8>> = panes.iter().map(|_| Vec::new()).collect();
    let mut failures = Vec::new();
    for (index, (bytes, pane, want)) in events.iter().enumerate() {
        s.send(f, bytes)?;
        let Some(pane) = *pane else { continue };
        if want.is_empty() {
            continue;
        }
        let file = panes.get(pane).ok_or("unknown pane")?.file;
        let path = s.directory.join(file);
        let expected_file = files.get_mut(pane).ok_or("unknown pane file")?;
        expected_file.extend_from_slice(want);
        let mut actual = Vec::new();
        let ok = s
            .wait("mouse event delivered", |s| {
                actual = fs::read(s.directory.join(file))?;
                Ok(actual.len() >= expected_file.len())
            })
            .is_ok()
            && actual == *expected_file;
        if !ok {
            failures.push(json!({
                "event": index,
                "sent": String::from_utf8_lossy(bytes),
                "pane": file,
                "expected_tail": String::from_utf8_lossy(want),
                "expected_file": String::from_utf8_lossy(expected_file),
                "actual_file": String::from_utf8_lossy(&actual),
            }));
            // Judge later events independently of this one.
            *expected_file = fs::read(path)?;
        }
    }
    s.journal.record(
        "mouse_verified",
        json!({"events":events.len(),"failures":failures}),
    )?;
    ensure(
        failures.is_empty(),
        &format!(
            "application: {} of {} mouse events were not delivered as xterm {} mode {} requires: {}",
            failures.len(),
            events.len(),
            if sgr { "SGR" } else { "legacy" },
            mode,
            serde_json::to_string(&failures)?
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xterm_table_filters_motion_by_mode_and_encodes_release() {
        assert_eq!(expected(1000, true, 0, 1, 1, false), b"\x1b[<0;1;1M");
        assert_eq!(expected(1000, true, 0, 1, 1, true), b"\x1b[<0;1;1m");
        assert!(expected(1000, true, 32, 1, 1, false).is_empty());
        assert_eq!(expected(1002, true, 32, 2, 3, false), b"\x1b[<32;2;3M");
        assert!(expected(1002, true, 35, 1, 1, false).is_empty());
        assert_eq!(expected(1003, true, 35, 1, 1, false), b"\x1b[<35;1;1M");
        assert_eq!(expected(1000, false, 0, 1, 1, false), b"\x1b[M !!");
        assert_eq!(expected(1000, false, 0, 1, 1, true), b"\x1b[M#!!");
        assert_eq!(expected(1000, false, 16, 1, 1, true), b"\x1b[M3!!");
        assert_eq!(expected(1000, false, 64, 80, 23, false), b"\x1b[M`p7");
        assert_eq!(outer(0, 0, 0, false), b"\x1b[<0;1;1M");
    }
}
