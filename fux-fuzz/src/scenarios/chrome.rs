use super::*;

/// Renders a painted frame at exactly the requested size and reports any cell
/// that breaks a documented painting rule.
fn inspect(paint: &str, rows: u16, cols: u16) -> Result<Vec<String>> {
    // One extra row and column so overflow is visible rather than clipped.
    let mut parser = vt100::Parser::new(rows + 1, cols + 1, 0);
    parser.process(paint.as_bytes());
    let screen = parser.screen();
    let mut faults = Vec::new();
    for y in 0..=rows {
        for x in 0..=cols {
            let Some(cell) = screen.cell(y, x) else {
                faults.push(format!("missing cell at {y},{x}"));
                continue;
            };
            if (y == rows || x == cols)
                && (!cell.contents().trim().is_empty() || cell.bgcolor() != vt100::Color::Default)
            {
                faults.push(format!(
                    "painted outside the {rows}x{cols} viewport at {y},{x}: {:?}",
                    cell.contents()
                ));
            }
            // A wide glyph needs two columns; one must never start in the last.
            if cell.is_wide() && x + 1 >= cols {
                faults.push(format!(
                    "wide glyph starts in the last column at {y},{x}: {:?}",
                    cell.contents()
                ));
            }
        }
    }
    Ok(faults)
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    // A full row of wide glyphs, so every column boundary is a glyph boundary
    // candidate as the viewport narrows.
    child_command(
        s,
        f,
        "stty raw -echo; printf '\\033[2J\\033[H'; i=1; while [ $i -le 40 ]; do printf '\\347\\225\\214'; i=$((i+1)); done; printf '\\r\\nREADY'; exec cat > /dev/null",
    )?;
    s.wait("wide glyph row painted", |s| {
        Ok(s.frame(v, 24, 80)?.contains('\u{754c}') && s.frame(v, 24, 80)?.contains("READY"))
    })?;
    running(s)?;

    // Odd and even narrow widths both split a two-column glyph differently.
    let mut all = Vec::new();
    for (rows, cols) in [
        (24u16, 80u16),
        (6, 9),
        (6, 8),
        (5, 7),
        (4, 5),
        (3, 4),
        (3, 3),
        (2, 2),
    ] {
        s.resize(f, rows, cols)?;
        s.wait("viewer resized", |s| {
            let value = s.rpc("fux.frame", json!({"viewer":v}))?;
            Ok(value.get("paint").is_some())
        })?;
        // Let the negotiated PTY size settle before judging the paint.
        s.wait("size settled", |s| {
            let rows_now = s
                .query(VIEWER)?
                .iter()
                .find(|r| id(r).ok() == Some(v))
                .and_then(|r| r.pointer("/components/fux::model::Viewer/rows")?.as_u64());
            Ok(rows_now == Some(u64::from(rows)))
        })?;
        let value = s.rpc("fux.frame", json!({"viewer":v}))?;
        let paint = value
            .get("paint")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let faults = inspect(paint, rows, cols)?;
        s.journal.record(
            "chrome_size",
            json!({"rows":rows,"cols":cols,"faults":faults}),
        )?;
        if !faults.is_empty() {
            all.push(json!({"rows":rows,"cols":cols,"faults":faults}));
        }
    }
    ensure(
        all.is_empty(),
        &format!(
            "application: painting broke documented rules at {} size(s): {}",
            all.len(),
            serde_json::to_string(&all)?
        ),
    )?;

    // With several tabs and a narrow viewport, the active tab stays visible.
    // Content is not expected to survive shrinking to 2x2 and back; vt100
    // resizing is not paragraph reflow. Only the negotiated size matters here.
    s.resize(f, 24, 80)?;
    s.wait("restored size", |s| {
        let dims = s
            .query(VIEWER)?
            .iter()
            .find(|r| id(r).ok() == Some(v))
            .and_then(|r| {
                Some((
                    r.pointer("/components/fux::model::Viewer/rows")?.as_u64()?,
                    r.pointer("/components/fux::model::Viewer/cols")?.as_u64()?,
                ))
            });
        Ok(dims == Some((24, 80)))
    })?;
    for name in ["alpha", "bravo", "charlie", "delta", "echo"] {
        s.control(v, json!({"kind":"tab_new","name":name}))?;
    }
    s.wait("tabs created", |s| {
        Ok(s.query("fux::model::Tab")?.len() >= 6)
    })?;
    let active = s.relation(v, "fux::model::OnTab")?;
    let active_name = s
        .query("bevy_ecs::name::Name")?
        .iter()
        .find(|r| id(r).ok() == Some(active))
        .and_then(|r| {
            r.pointer("/components/bevy_ecs::name::Name")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_default();
    s.journal
        .record("active_tab", json!({"entity":active,"name":active_name}))?;
    let mut missing = Vec::new();
    for cols in [80u16, 40, 20, 12, 8, 4, 2] {
        s.resize(f, 24, cols)?;
        s.wait("narrow size settled", |s| {
            let value = s
                .query(VIEWER)?
                .iter()
                .find(|r| id(r).ok() == Some(v))
                .and_then(|r| r.pointer("/components/fux::model::Viewer/cols")?.as_u64());
            Ok(value == Some(u64::from(cols)))
        })?;
        let frame = s.frame(v, 24, cols)?;
        let bar = frame.lines().last().unwrap_or_default().to_owned();
        // Narrow labels use a cell-aware ellipsis, so at small widths the
        // active tab can legitimately render as one character plus an ellipsis.
        let first: String = active_name.chars().take(1).collect();
        let shown = !first.is_empty() && bar.contains(&first);
        s.journal.record(
            "tab_overflow",
            json!({"cols":cols,"bar":bar,"active_shown":shown}),
        )?;
        if !shown && cols >= 4 {
            missing.push(json!({"cols":cols,"bar":bar}));
        }
    }
    ensure(
        missing.is_empty(),
        &format!(
            "application: the active tab left the bar at {}: {}",
            missing.len(),
            serde_json::to_string(&missing)?
        ),
    )
}
