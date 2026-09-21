//! Counts and sizes nobody wrote down: hundreds of panes, a thousand tabs,
//! fifty workspaces, multi-kilobyte names, a wide-glyph paste, and a scene
//! round trip of the large layout. Response times are recorded, not just
//! bounded.
use super::invariant::{self, World};
use super::*;
use std::time::Instant;

const PANES: usize = 200;
const TABS: usize = 1000;
const WORKSPACES: usize = 50;
const FRAME_BOUND_MS: u128 = 2000;

fn timed_frame(s: &mut Server, v: u64, label: &str, times: &mut Vec<Value>) -> Result<String> {
    let start = Instant::now();
    let paint = s
        .rpc("fux.frame", json!({"viewer":v}))?
        .get("paint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let ms = start.elapsed().as_millis();
    times.push(json!({"label":label,"ms":ms,"paint_bytes":paint.len()}));
    ensure(
        ms <= FRAME_BOUND_MS,
        &format!("application: fux.frame took {ms} ms {label}, over the {FRAME_BOUND_MS} ms bound"),
    )?;
    Ok(paint)
}
fn no_panic(s: &mut Server, label: &str) -> Result<()> {
    let err = s.stderr_text()?;
    let bad = err
        .lines()
        .find(|l| l.contains("panicked") || l.contains("thread '") || l.contains("RUST_BACKTRACE"));
    ensure(
        bad.is_none(),
        &format!("application: server stderr shows a panic {label}: {bad:?}"),
    )
}
fn check(s: &mut Server, v: u64, label: &str, cap: usize) -> Result<World> {
    let w = World::read(s)?;
    let problems = invariant::violations(s, &w, cap, &[])?;
    ensure(
        problems.is_empty(),
        &format!(
            "application: invariants broken {label}: {}",
            problems.join(" | ")
        ),
    )?;
    no_panic(s, label)?;
    let _ = v;
    Ok(w)
}
fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("scale viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn settled(s: &mut Server, v: u64, label: &str) -> Result<String> {
    let mut text = String::new();
    s.wait(label, |s| {
        text = notice_text(s, v)?;
        Ok(!text.is_empty() && !text.ends_with("..."))
    })?;
    Ok(text)
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(200, 400)?;
    // Requests over a large world legitimately take longer than the default
    // 500 ms; time them rather than fail them.
    s.request_timeout = std::time::Duration::from_secs(10);
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 200, 400)?.contains("DEFAULT-SHELL"))
    })?;
    running(s)?;
    let mut times = Vec::new();
    timed_frame(s, v, "at start", &mut times)?;
    let start_leaf = s.relation(v, "fux::model::Focused")?;
    let home_ws = s.relation(v, "fux::model::Viewing")?;

    // Hundreds of panes in one tab of a large viewer. A split halves the
    // focused pane, so splitting the newest pane repeatedly runs out of room
    // and is refused; split the largest painted rectangle instead, found from
    // the paint and identified by the marker each pane prints.
    let started = Instant::now();
    let mut markers: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for i in 0..PANES {
        let paint = s
            .rpc("fux.frame", json!({"viewer":v}))?
            .get("paint")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let rects = invariant::painted_panes(&paint, 199);
        let largest = rects
            .iter()
            .max_by_key(|(_, a, b, h)| u64::from(b - a) * u64::from(*h))
            .copied();
        let (target, axis) = match largest {
            Some((row, a, b, h)) => {
                let text = s.frame(v, 200, 400)?;
                let line = text
                    .lines()
                    .nth(usize::from(row.saturating_sub(1)))
                    .unwrap_or_default();
                let cell: String = line
                    .chars()
                    .skip(usize::from(a.saturating_sub(1)))
                    .take(8)
                    .collect();
                let name = cell.split(' ').next().unwrap_or_default().to_owned();
                (
                    markers.get(&name).copied(),
                    if b - a >= h * 2 {
                        "horizontal"
                    } else {
                        "vertical"
                    },
                )
            }
            None => (None, "horizontal"),
        };
        if let Some(leaf) = target {
            s.control(v, json!({"kind":"focus","pane":leaf}))?;
        }
        let n = i + 1;
        s.control(v, json!({"kind":"split","axis":axis,"program":format!("stty raw -echo; S=SP; printf \"\\033[2J\\033[H${{S}}{n}\"; exec cat > /dev/null")}))?;
        let leaf = s.relation(v, "fux::model::Focused")?;
        markers.insert(format!("SP{n}"), leaf);
        s.wait("pane marker printed", |s| {
            Ok(s.frame(v, 200, 400)?.contains(&format!("SP{n}")))
        })?;
        if i % 50 == 49 {
            timed_frame(s, v, &format!("with {} panes", i + 2), &mut times)?;
        }
    }
    s.wait("all pane processes running", |s| {
        Ok(states(s)?
            .iter()
            .all(|(_, st)| st.pointer("/status/kind") == Some(&json!("running"))))
    })?;
    let w = check(s, v, "with hundreds of panes", PANES + 8)?;
    s.journal.record(
        "panes",
        json!({"created":PANES,"views":w.views.len(),"processes":w.states.len(),"ms":started.elapsed().as_millis()}),
    )?;
    ensure(
        w.views.len() == PANES + 1,
        &format!("expected {} views, have {}", PANES + 1, w.views.len()),
    )?;
    // Every pane is at least the 2x2 backing minimum, never zero.
    for (_, st) in &w.states {
        let (r, c) = (
            st.get("rows").and_then(Value::as_u64).unwrap_or(0),
            st.get("cols").and_then(Value::as_u64).unwrap_or(0),
        );
        ensure(
            r >= 2 && c >= 2,
            &format!("application: a pane negotiated to {r}x{c}, below the 2x2 backing minimum"),
        )?;
    }

    // A thousand tabs in one workspace, by moving one pane through new tabs;
    // each move leaves an empty tab behind, which the README retains.
    s.control(v, json!({"kind":"focus","pane":start_leaf}))?;
    let started = Instant::now();
    for i in 0..TABS {
        let t0 = Instant::now();
        s.control(
            v,
            json!({"kind":"move","to":{"kind":"new_tab","name":format!("t{i}")}}),
        )?;
        if i % 100 == 99 {
            times.push(json!({"label":format!("move to new tab #{}", i + 1),"ms":t0.elapsed().as_millis()}));
        }
        if i % 250 == 249 {
            timed_frame(s, v, &format!("with {} tabs", i + 2), &mut times)?;
        }
    }
    let w = check(s, v, "with a thousand tabs", PANES + 8)?;
    s.journal.record(
        "tabs",
        json!({"created":TABS,"tabs":w.tabs.len(),"ms":started.elapsed().as_millis()}),
    )?;
    ensure(
        w.tabs.len() >= TABS,
        &format!("expected at least {TABS} tabs, have {}", w.tabs.len()),
    )?;
    let paint = timed_frame(s, v, "bar with a thousand tabs", &mut times)?;
    let bar = s
        .frame(v, 24, 80)?
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned();
    ensure(
        bar.contains(&format!("t{}", TABS - 1))
            || bar.contains('…')
            || bar.contains(&format!("t{}", TABS - 1)[..2]),
        &format!("application: the bar does not show the active tab among a thousand: {bar:?}"),
    )?;
    ensure(!paint.is_empty(), "empty paint with a thousand tabs")?;

    // Fifty workspaces, the same way.
    let started = Instant::now();
    for i in 0..WORKSPACES {
        s.control(
            v,
            json!({"kind":"move","to":{"kind":"new_workspace","name":format!("w{i}")}}),
        )?;
    }
    let w = check(s, v, "with fifty workspaces", PANES + 8)?;
    s.journal.record("workspaces", json!({"created":WORKSPACES,"workspaces":w.workspaces.len(),"ms":started.elapsed().as_millis()}))?;
    ensure(
        w.workspaces.len() >= WORKSPACES,
        &format!(
            "expected at least {WORKSPACES} workspaces, have {}",
            w.workspaces.len()
        ),
    )?;
    timed_frame(s, v, "with fifty workspaces", &mut times)?;

    // Names of several kilobytes and of wide glyphs.
    let long = "n".repeat(4096);
    let ws = s.relation(v, "fux::model::Viewing")?;
    let tab = s.relation(v, "fux::model::OnTab")?;
    s.control(
        v,
        json!({"kind":"rename","subject":{"workspace":ws},"name":long}),
    )?;
    s.control(
        v,
        json!({"kind":"rename","subject":{"tab":tab},"name":"界".repeat(300)}),
    )?;
    s.control(
        v,
        json!({"kind":"rename","subject":{"pane":start_leaf},"name":"界".repeat(200)}),
    )?;
    let paint = timed_frame(s, v, "with kilobyte and wide-glyph names", &mut times)?;
    let faults = super::chrome::inspect(&paint, 200, 400)?;
    ensure(
        faults.is_empty(),
        &format!(
            "application: huge names broke the paint: {}",
            faults.join(" | ")
        ),
    )?;
    check(s, v, "after huge names", PANES + 8)?;

    // A 64 KiB paste of wide glyphs into a raw pane, captured to a file.
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; exec cat > paste.bin"}))?;
    let sink = s.relation(v, "fux::model::Focused")?;
    s.wait("paste sink ready", |s| {
        Ok(s.directory.join("paste.bin").is_file())
    })?;
    running(s)?;
    let wide = "界".repeat(21_845); // 65,535 bytes
    s.rpc(
        "world.trigger_event",
        json!({"event":"fux::control::UserInput","value":{"viewer":v,"input":{"kind":"paste","text":wide}}}),
    )?;
    s.rpc(
        "world.trigger_event",
        json!({"event":"fux::control::UserInput","value":{"viewer":v,"input":{"kind":"key","key":"!","ctrl":false,"alt":false,"shift":false}}}),
    )?;
    let mut expected = wide.clone().into_bytes();
    expected.push(b'!');
    s.wait("wide paste delivered byte-exact", |s| {
        Ok(fs::read(s.directory.join("paste.bin"))? == expected)
    })
    .map_err(|e| {
        format!("application: a 64 KiB wide-glyph paste was not delivered exactly: {e}")
    })?;
    let _ = sink;

    // Scene round trip of the large home workspace.
    s.control(
        v,
        json!({"kind":"select","scope":"workspace","entity":home_ws}),
    )?;
    s.wait("home workspace selected", |s| {
        Ok(s.relation(v, "fux::model::Viewing")? == home_ws)
    })?;
    let started = Instant::now();
    s.control(
        v,
        json!({"kind":"save_layout","workspace":home_ws,"path":"large.scn.ron"}),
    )?;
    let saved = settled(s, v, "large save")?;
    ensure(
        saved.starts_with("saved"),
        &format!("application: saving the large layout failed: {saved}"),
    )?;
    let bytes = fs::metadata(s.directory.join("large.scn.ron"))?.len();
    let save_ms = started.elapsed().as_millis();
    let started = Instant::now();
    s.control(
        v,
        json!({"kind":"load_layout","workspace":home_ws,"path":"large.scn.ron","mapping":[]}),
    )?;
    let loaded = settled(s, v, "large load")?;
    ensure(
        loaded.starts_with("loaded"),
        &format!("application: loading the large layout failed: {loaded}"),
    )?;
    let load_ms = started.elapsed().as_millis();
    s.journal.record(
        "large_scene",
        json!({"bytes":bytes,"save_ms":save_ms,"load_ms":load_ms}),
    )?;
    let w = check(s, v, "after the large round trip", PANES + 8)?;
    ensure(
        w.tabs.len() >= TABS,
        "application: the large round trip lost tabs",
    )?;
    timed_frame(s, v, "after the large round trip", &mut times)?;

    s.journal.record("frame_times", json!(times))?;
    // Passing cases are removed with their journals; keep the numbers.
    println!("SCALE-TIMES {}", serde_json::to_string(&times)?);
    no_panic(s, "at the end")?;
    Ok(())
}
