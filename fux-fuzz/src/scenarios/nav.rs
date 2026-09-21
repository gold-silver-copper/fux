use super::*;

/// Builds a known geometry and checks the documented ranking rule:
/// directional focus ranks pane centers by cross-axis distance, then forward
/// distance, then entity ID, and never wraps at an edge.
pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    running(s)?;
    let p1 = s.relation(v, "fux::model::Focused")?;

    // P1 | P2, then P1 split vertically into P1 over P3.
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HPANE-2'; exec cat > /dev/null"}))?;
    let p2 = s.relation(v, "fux::model::Focused")?;
    s.wait("pane 2 painted", |s| {
        Ok(s.frame(v, 24, 80)?.contains("PANE-2"))
    })?;
    s.control(v, json!({"kind":"focus","pane":p1}))?;
    s.control(v, json!({"kind":"split","axis":"vertical","program":"stty raw -echo; printf '\\033[2J\\033[HPANE-3'; exec cat > /dev/null"}))?;
    let p3 = s.relation(v, "fux::model::Focused")?;
    s.wait("pane 3 painted", |s| {
        Ok(s.frame(v, 24, 80)?.contains("PANE-3"))
    })?;
    ensure(
        p1 != p2 && p2 != p3 && p1 != p3,
        "expected three distinct panes",
    )?;
    let pids = running(s)?;
    ensure(pids.len() == 3, "expected three processes")?;
    s.journal
        .record("nav_layout", json!({"p1":p1,"p2":p2,"p3":p3}))?;

    // Directional focus from each pane, re-establishing the origin each time.
    for (from, direction, want, label) in [
        (p1, "right", Some(p2), "right from the top-left pane"),
        (p1, "down", Some(p3), "down from the top-left pane"),
        (p3, "up", Some(p1), "up from the bottom-left pane"),
        (p3, "right", Some(p2), "right from the bottom-left pane"),
        (p1, "left", None, "left at the left edge"),
        (p1, "up", None, "up at the top edge"),
        (p2, "right", None, "right at the right edge"),
    ] {
        s.control(v, json!({"kind":"focus","pane":from}))?;
        s.wait("origin focused", |s| {
            Ok(s.relation(v, "fux::model::Focused")? == from)
        })?;
        s.control(v, json!({"kind":"focus_direction","direction":direction}))?;
        let expected = want.unwrap_or(from);
        let mut landed = from;
        let result = s.wait(label, |s| {
            landed = s.relation(v, "fux::model::Focused")?;
            Ok(landed == expected)
        });
        s.journal.record(
            "directional_focus",
            json!({"label":label,"from":from,"direction":direction,"expected":expected,"landed":landed}),
        )?;
        result.map_err(|e| -> Box<dyn std::error::Error> {
            format!(
                "application: {label}: focus should be {expected} but is {landed}{}: {e}",
                if want.is_none() {
                    " (an edge must not wrap)"
                } else {
                    ""
                }
            )
            .into()
        })?;
    }

    // focus_last returns to the previously focused pane, both ways.
    s.control(v, json!({"kind":"focus","pane":p1}))?;
    s.control(v, json!({"kind":"focus","pane":p2}))?;
    s.wait("p2 focused", |s| {
        Ok(s.relation(v, "fux::model::Focused")? == p2)
    })?;
    s.control(v, json!({"kind":"focus_last"}))?;
    s.wait("focus_last returns to the previous pane", |s| {
        Ok(s.relation(v, "fux::model::Focused")? == p1)
    })
    .map_err(|e| format!("application: focus_last after an explicit focus: {e}"))?;
    s.control(v, json!({"kind":"focus_last"}))?;
    s.wait("focus_last toggles back", |s| {
        Ok(s.relation(v, "fux::model::Focused")? == p2)
    })
    .map_err(|e| format!("application: focus_last is not its own inverse: {e}"))?;

    // focus_next visits every pane and returns to the start.
    s.control(v, json!({"kind":"focus","pane":p1}))?;
    s.wait("p1 focused", |s| {
        Ok(s.relation(v, "fux::model::Focused")? == p1)
    })?;
    let mut visited = vec![p1];
    for _ in 0..3 {
        let current = s.relation(v, "fux::model::Focused")?;
        s.control(v, json!({"kind":"focus_next"}))?;
        let mut next = current;
        s.wait("focus_next moves", |s| {
            next = s.relation(v, "fux::model::Focused")?;
            Ok(next != current)
        })
        .map_err(|e| format!("application: focus_next did not move from {current}: {e}"))?;
        visited.push(next);
    }
    s.journal
        .record("focus_cycle", json!({"visited":visited}))?;
    let mut unique = visited.clone();
    unique.sort_unstable();
    unique.dedup();
    ensure(
        unique.len() == 3 && visited.first() == visited.last(),
        &format!(
            "application: focus_next over three panes should visit each once and return to the start: {visited:?}"
        ),
    )?;

    // Swapping exchanges positions without touching processes.
    s.control(v, json!({"kind":"focus","pane":p1}))?;
    s.wait("p1 focused for swap", |s| {
        Ok(s.relation(v, "fux::model::Focused")? == p1)
    })?;
    s.control(v, json!({"kind":"swap_direction","direction":"down"}))?;
    s.wait("swap moved the pane's content", |s| {
        let frame = s.frame(v, 24, 80)?;
        Ok(frame.contains("PANE-3") && frame.contains("DEFAULT-SHELL"))
    })?;
    ensure(
        pids.iter().all(|p| alive(*p)),
        "application: swapping panes terminated a process",
    )?;
    ensure(
        s.relation(v, "fux::model::Focused")? == p1,
        "application: swap moved focus off the swapped pane",
    )?;
    Ok(())
}
