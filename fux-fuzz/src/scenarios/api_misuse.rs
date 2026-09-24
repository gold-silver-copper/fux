use super::*;

fn viewer_state(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("api viewer disappeared")?;
    component(row, VIEWER)
}
fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    Ok(viewer_state(s, viewer)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn user_input(s: &mut Server, v: u64, input: Value) -> Result<Value> {
    s.rpc(
        "world.trigger_event",
        json!({"event":"fux::control::UserInput","value":{"viewer":v,"input":input}}),
    )
}
/// A request that must be rejected at deserialization: a JSON-RPC error,
/// not a notice, and no change to the viewer's notice.
fn rejected(s: &mut Server, v: u64, label: &str, event: &str, value: Value) -> Result<()> {
    let before = notice_text(s, v)?;
    let result = s.rpc("world.trigger_event", json!({"event":event,"value":value}));
    s.journal.record(
        "rejected_request",
        json!({"label":label,"error":result.as_ref().err().map(ToString::to_string)}),
    )?;
    ensure(
        result.is_err(),
        &format!("application: {label} should be rejected with a JSON-RPC error but was accepted"),
    )?;
    std::thread::sleep(std::time::Duration::from_millis(50));
    let after = notice_text(s, v)?;
    ensure(
        after == before,
        &format!(
            "application: {label} was rejected but still changed the notice from {before:?} to {after:?}"
        ),
    )
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let pids = running(s)?;
    let leaf = s.relation(v, "fux::model::Focused")?;
    let tab = s.relation(v, "fux::model::OnTab")?;

    // Malformed input and commands are rejected when they deserialize.
    rejected(
        s,
        v,
        "an unsupported key name",
        "fux::control::UserInput",
        json!({"viewer":v,"input":{"kind":"key","key":"f13","ctrl":false,"alt":false,"shift":false}}),
    )?;
    rejected(
        s,
        v,
        "a made-up key name",
        "fux::control::UserInput",
        json!({"viewer":v,"input":{"kind":"key","key":"banana","ctrl":false,"alt":false,"shift":false}}),
    )?;
    rejected(
        s,
        v,
        "an unknown input kind",
        "fux::control::UserInput",
        json!({"viewer":v,"input":{"kind":"teleport"}}),
    )?;
    rejected(
        s,
        v,
        "an unknown command kind",
        "fux::control::Control",
        json!({"viewer":v,"command":{"kind":"explode"}}),
    )?;
    rejected(
        s,
        v,
        "a close with no subject",
        "fux::control::Control",
        json!({"viewer":v,"command":{"kind":"close"}}),
    )?;
    rejected(
        s,
        v,
        "a retired paired command kind",
        "fux::control::Control",
        json!({"viewer":v,"command":{"kind":"tab_select","tab":tab}}),
    )?;
    rejected(
        s,
        v,
        "a mouse action outside the enum",
        "fux::control::UserInput",
        json!({"viewer":v,"input":{"kind":"mouse","action":"hover","button":"left","x":1,"y":1,"ctrl":false,"alt":false,"shift":false}}),
    )?;

    // Well-formed commands naming the wrong or a missing entity report a
    // notice rather than an error, and the two cases stay distinguishable:
    // a missing entity says "target no longer exists", while a live entity of
    // the wrong kind says so instead.
    let scratch = s
        .rpc(
            "world.spawn_entity",
            json!({"components":{"bevy_ecs::name::Name":"scratch"}}),
        )?
        .get("entity")
        .and_then(Value::as_u64)
        .ok_or("spawn returned no entity")?;
    s.rpc("world.despawn_entity", json!({"entity":scratch}))?;
    s.control(v, json!({"kind":"focus","pane":scratch}))?;
    let mut text = String::new();
    s.wait("missing target reported", |s| {
        text = notice_text(s, v)?;
        Ok(text.contains("no longer exists"))
    })
    .map_err(|e| {
        format!("application: focusing a despawned pane should report \"target no longer exists\", notice was {text:?}: {e}")
    })?;
    // A live entity of the wrong kind: the viewer's own tab named as a pane.
    s.control(v, json!({"kind":"focus","pane":tab}))?;
    s.wait("wrong-kind target reported", |s| {
        text = notice_text(s, v)?;
        Ok(text.contains("wrong kind") || text.contains("not a pane"))
    })
    .map_err(|e| {
        format!("application: focusing a tab as a pane should report a wrong-kind notice, notice was {text:?}: {e}")
    })?;
    ensure(
        s.relation(v, "fux::model::OnTab")? == tab && s.relation(v, "fux::model::Focused")? == leaf,
        "a rejected command changed the viewer's tab or focus",
    )?;

    // A zero viewport is accepted and paints nothing; restoring it repaints.
    user_input(s, v, json!({"kind":"resize","rows":0,"cols":0}))?;
    s.wait("zero viewport reflected", |s| {
        let st = viewer_state(s, v)?;
        Ok(st.get("rows") == Some(&json!(0)) && st.get("cols") == Some(&json!(0)))
    })?;
    let paint = s
        .rpc("fux.frame", json!({"viewer":v}))?
        .get("paint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let content: String = paint
        .chars()
        .filter(|c| !c.is_control() && *c != '[')
        .collect();
    s.journal
        .record("zero_paint", json!({"bytes":paint.len()}))?;
    ensure(
        !paint.contains("DEFAULT-SHELL") && content.trim().len() < 40,
        &format!("application: a zero-sized viewport painted content: {paint:?}"),
    )?;
    user_input(s, v, json!({"kind":"resize","rows":24,"cols":80}))?;
    s.wait("viewport restored", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })
    .map_err(|e| format!("application: restoring a zero viewport did not repaint: {e}"))?;

    // Oversized resizes clamp to the documented 4096. The attached frontend
    // then gets a 4096x4096 paint, and every request waits behind it: 2.7-9.5 s
    // for the first such frame in a debug build on a loaded Mac, 0.2-0.3 s in
    // release. As in `walk` and `limits`, allow for it while the viewer is that
    // large -- here 15 s, with the worst measured at 9.5 s.
    let ordinary = s.request_timeout;
    s.request_timeout = std::time::Duration::from_secs(15);
    let oversized = oversized_resize(s, v);
    s.request_timeout = ordinary;
    oversized?;

    // Every probe left the process and the frontend alone.
    ensure(
        pids.iter().all(|p| alive(*p)),
        "application: API misuse terminated a process",
    )?;
    s.healthy()?;
    Ok(())
}

fn oversized_resize(s: &mut Server, v: u64) -> Result<()> {
    user_input(s, v, json!({"kind":"resize","rows":65535,"cols":65535}))?;
    s.wait("oversized resize clamped", |s| {
        let st = viewer_state(s, v)?;
        Ok(st.get("rows") == Some(&json!(4096)) && st.get("cols") == Some(&json!(4096)))
    })
    .map_err(|e| format!("application: a 65535x65535 resize was not clamped to 4096: {e}"))?;
    user_input(s, v, json!({"kind":"resize","rows":24,"cols":80}))?;
    s.wait("viewport restored again", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })
}
