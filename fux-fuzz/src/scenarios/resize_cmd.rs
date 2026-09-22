use super::*;

fn dims(s: &mut Server, pane: u64) -> Result<(u64, u64)> {
    let state = states(s)?
        .into_iter()
        .find(|(e, _)| *e == pane)
        .map(|(_, st)| st)
        .ok_or("pane state missing")?;
    Ok((
        state.get("rows").and_then(Value::as_u64).unwrap_or(0),
        state.get("cols").and_then(Value::as_u64).unwrap_or(0),
    ))
}
fn pane_of(s: &mut Server, leaf: u64) -> Result<u64> {
    s.query("fux::model::PaneView")?
        .iter()
        .find(|r| id(r).ok() == Some(leaf))
        .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64())
        .ok_or("leaf has no pane".into())
}
fn resize(s: &mut Server, v: u64, axis: &str, grow: bool) -> Result<()> {
    s.control(v, json!({"kind":"resize","axis":axis,"grow":grow}))
}
fn wait_dims(
    s: &mut Server,
    label: &str,
    a: u64,
    b: u64,
    mut ok: impl FnMut((u64, u64), (u64, u64)) -> bool,
) -> Result<((u64, u64), (u64, u64))> {
    let mut seen = ((0, 0), (0, 0));
    s.wait(label, |s| {
        seen = (dims(s, a)?, dims(s, b)?);
        Ok(ok(seen.0, seen.1))
    })
    .map_err(|e| format!("application: {label}: sizes {seen:?}: {e}"))?;
    Ok(seen)
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let leaf_a = s.relation(v, "fux::model::Focused")?;
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HRSZ-B'; exec cat > /dev/null"}))?;
    let leaf_b = s.relation(v, "fux::model::Focused")?;
    s.wait("side by side", |s| {
        Ok(s.frame(v, 24, 80)?.contains("RSZ-B"))
    })?;
    let pids = running(s)?;
    let (a, b) = (pane_of(s, leaf_a)?, pane_of(s, leaf_b)?);
    let (da, db) = wait_dims(s, "initial split sizes", a, b, |x, y| x.1 > 0 && y.1 > 0)?;
    let total = da.1 + db.1;
    s.journal
        .record("initial", json!({"a":da,"b":db,"total":total}))?;
    ensure(
        total + 1 == 80,
        &format!("side-by-side widths should sum to 79 with one separator, got {da:?} {db:?}"),
    )?;

    // Growing the focused pane's width takes columns from its sibling and
    // conserves the total; shrinking gives them back.
    resize(s, v, "horizontal", true)?;
    let (ga, gb) = wait_dims(s, "grow width", a, b, |x, y| y.1 > db.1 && x.1 < da.1)?;
    ensure(
        ga.1 + gb.1 == total,
        &format!("application: width grow did not conserve columns: {ga:?} {gb:?}"),
    )?;
    ensure(
        ga.0 == da.0 && gb.0 == db.0,
        "application: a width resize changed heights",
    )?;
    resize(s, v, "horizontal", false)?;
    let (sa, sb) = wait_dims(s, "shrink width", a, b, |x, y| y.1 < gb.1 && x.1 > ga.1)?;
    ensure(
        sa.1 + sb.1 == total,
        "application: width shrink did not conserve columns",
    )?;

    // Many grows never push the sibling below the 2-column backing minimum,
    // and the total stays conserved throughout.
    for _ in 0..40 {
        resize(s, v, "horizontal", true)?;
    }
    // Growth is bounded by a flex floor, so the sibling settles above the
    // backing minimum rather than at it. Let the widths stop changing, then
    // judge the documented invariants.
    // A frame request applies the queued layout changes and negotiates PTY
    // sizes synchronously; the reflected ProcessState follows on a later
    // update. Two equal 5 ms polls can straddle that publication, so require
    // a bounded run of consecutive equal observations instead.
    s.frame(v, 24, 80)?;
    let mut ma = (0, 0);
    let mut mb = (0, 0);
    let mut stable_polls = 0;
    s.wait("widths settle after repeated grows", |s| {
        let now = (dims(s, a)?, dims(s, b)?);
        if now == (ma, mb) {
            stable_polls += 1;
        } else {
            stable_polls = 0;
            ma = now.0;
            mb = now.1;
        }
        Ok(stable_polls >= 20)
    })?;
    s.journal
        .record("after_many_grows", json!({"a":ma,"b":mb,"total":total}))?;
    ensure(
        ma.1 < da.1,
        &format!("application: repeated grows did not shrink the sibling at all: {ma:?} vs {da:?}"),
    )?;
    ensure(
        ma.1 >= 2,
        &format!("application: repeated grows shrank the sibling below 2 columns: {ma:?}"),
    )?;
    ensure(
        ma.1 + mb.1 == total,
        &format!("application: repeated grows lost columns: {ma:?} {mb:?}"),
    )?;
    ensure(
        s.relation(v, "fux::model::Focused")? == leaf_b,
        "resizing changed focus",
    )?;

    // A height resize in a layout with no stacked container is a no-op that
    // reports nothing and changes nothing.
    let before_notice = s
        .query(VIEWER)?
        .iter()
        .find(|r| id(r).ok() == Some(v))
        .and_then(|r| r.pointer("/components/fux::model::Viewer/notice").cloned());
    resize(s, v, "vertical", true)?;
    std::thread::sleep(std::time::Duration::from_millis(150));
    let (na, nb) = (dims(s, a)?, dims(s, b)?);
    ensure(
        (na, nb) == (ma, mb),
        &format!(
            "application: a height resize with no stacked container changed sizes: {na:?} {nb:?}"
        ),
    )?;
    let after_notice = s
        .query(VIEWER)?
        .iter()
        .find(|r| id(r).ok() == Some(v))
        .and_then(|r| r.pointer("/components/fux::model::Viewer/notice").cloned());
    ensure(
        after_notice.as_ref().and_then(|n| n.get("error")) != Some(&json!(true)),
        &format!(
            "application: a no-op height resize raised an error: {after_notice:?} (was {before_notice:?})"
        ),
    )?;

    // The same rules hold for heights, in a fresh tab. A pane carries one
    // flex factor for whichever axis its container uses, so reusing a pane
    // stressed along the other axis would start the stack lopsided.
    s.control(v, json!({"kind":"tab_new","name":"stacked"}))?;
    s.wait("fresh tab", |s| Ok(s.query("fux::model::Tab")?.len() == 2))?;
    let leaf_top = s.relation(v, "fux::model::Focused")?;
    let b = pane_of(s, leaf_top)?;
    s.control(v, json!({"kind":"split","axis":"vertical","program":"stty raw -echo; printf '\\033[2J\\033[HRSZ-C'; exec cat > /dev/null"}))?;
    let leaf_c = s.relation(v, "fux::model::Focused")?;
    s.wait("stacked", |s| Ok(s.frame(v, 24, 80)?.contains("RSZ-C")))?;
    let c = pane_of(s, leaf_c)?;
    let (hb, hc) = wait_dims(s, "initial stacked sizes", b, c, |x, y| x.0 > 0 && y.0 > 0)?;
    let rows_total = hb.0 + hc.0;
    resize(s, v, "vertical", true)?;
    let (gb2, gc2) = wait_dims(s, "grow height", b, c, |x, y| y.0 > hc.0 && x.0 < hb.0)?;
    ensure(
        gb2.0 + gc2.0 == rows_total,
        "application: height grow did not conserve rows",
    )?;
    ensure(
        gb2.1 == hb.1 && gc2.1 == hc.1,
        "application: a height resize changed widths",
    )?;
    for _ in 0..40 {
        resize(s, v, "vertical", true)?;
    }
    let mut mb2 = (0, 0);
    let mut mc2 = (0, 0);
    s.wait("heights settle after repeated grows", |s| {
        let now = (dims(s, b)?, dims(s, c)?);
        let stable = now == (mb2, mc2);
        mb2 = now.0;
        mc2 = now.1;
        Ok(stable)
    })?;
    s.journal.record(
        "after_many_height_grows",
        json!({"b":mb2,"c":mc2,"total":rows_total}),
    )?;
    ensure(
        mb2.0 >= 2 && mb2.0 + mc2.0 == rows_total,
        &format!(
            "application: repeated height grows broke the minimum or conservation: {mb2:?} {mc2:?}"
        ),
    )?;
    ensure(
        pids.iter().all(|p| alive(*p)),
        "resizing terminated a process",
    )?;
    Ok(())
}
