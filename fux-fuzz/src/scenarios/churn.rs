use super::*;
use nix::sys::stat::Mode;

const SETTINGS: &str = "fux::assets::Settings";
const WORKSPACE: &str = "fux::model::Workspace";

fn settings(s: &mut Server) -> Result<Value> {
    s.rpc("world.get_resources", json!({"resource":SETTINGS}))?
        .get("value")
        .cloned()
        .ok_or("Settings resource missing".into())
}
fn history_lines(s: &mut Server) -> Result<u64> {
    Ok(settings(s)?
        .get("history_lines")
        .and_then(Value::as_u64)
        .unwrap_or(0))
}
fn prefix(s: &mut Server) -> Result<String> {
    Ok(settings(s)?
        .get("prefix")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn write(s: &mut Server, text: &str) -> Result<()> {
    fs::write(s.directory.join("fux.json"), text)?;
    Ok(())
}
fn paints(s: &mut Server, v: u64) -> Result<bool> {
    Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
}
fn workspace_names(s: &mut Server) -> Result<Vec<String>> {
    let ws: Vec<u64> = s.query(WORKSPACE)?.iter().map(id).collect::<Result<_>>()?;
    let mut names: Vec<String> = s
        .query("bevy_ecs::name::Name")?
        .iter()
        .filter(|r| id(r).ok().is_some_and(|e| ws.contains(&e)))
        .filter_map(|r| {
            r.pointer("/components/bevy_ecs::name::Name")?
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    names.sort();
    Ok(names)
}
fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("churn viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn expect_history(s: &mut Server, label: &str, want: u64) -> Result<()> {
    let mut seen = 0;
    s.wait(label, |s| {
        seen = history_lines(s)?;
        Ok(seen == want)
    })
    .map_err(|e| {
        format!("application: {label}: history_lines should be {want}, is {seen}: {e}").into()
    })
}
fn healthy_layout(s: &mut Server, v: u64, label: &str, workspaces: usize) -> Result<()> {
    ensure(
        paints(s, v)?,
        &format!("application: {label}: the viewer no longer paints"),
    )?;
    let names = workspace_names(s)?;
    let mut unique = names.clone();
    unique.dedup();
    ensure(
        names.len() == workspaces && unique.len() == names.len(),
        &format!(
            "application: {label}: expected {workspaces} uniquely named workspaces, have {names:?}"
        ),
    )
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| paints(s, v))?;
    let workspace = s.relation(v, "fux::model::Viewing")?;
    let pids = running(s)?;
    ensure(prefix(s)? == "ctrl-b", "default prefix should be ctrl-b")?;
    let default_history = history_lines(s)?;

    // A burst of rewrites: the server must settle on the last valid file,
    // never on an intermediate one.
    for i in 1..=30u64 {
        write(s, &format!(r#"{{"history_lines":{}}}"#, 1000 + i))?;
    }
    write(s, r#"{"history_lines":7777,"prefix":"ctrl-a"}"#)?;
    expect_history(s, "settled on the last of a burst", 7777)?;
    ensure(
        prefix(s)? == "ctrl-a",
        "application: the last rewrite's prefix was not applied",
    )?;
    healthy_layout(s, v, "after a rewrite burst", 1)?;

    // Deleting the file keeps the previous usable configuration.
    fs::remove_file(s.directory.join("fux.json"))?;
    std::thread::sleep(std::time::Duration::from_millis(600));
    ensure(
        history_lines(s)? == 7777 && prefix(s)? == "ctrl-a",
        &format!(
            "application: deleting fux.json changed settings to history {} prefix {}",
            history_lines(s)?,
            prefix(s)?
        ),
    )?;
    healthy_layout(s, v, "after deleting the config", 1)?;

    // Recreating it applies the new file.
    write(s, r#"{"history_lines":8888}"#)?;
    expect_history(s, "recreated config applied", 8888)?;
    ensure(
        prefix(s)? == "ctrl-b",
        "application: a recreated config without a prefix should restore the default prefix",
    )?;

    // Malformed immediately followed by valid ends on the valid one.
    write(s, "{ broken")?;
    write(s, r#"{"history_lines":9999}"#)?;
    expect_history(s, "valid after malformed applied", 9999)?;
    write(s, r#"{"history_lines":9999,"broken":true}"#)?; // unknown field is invalid
    std::thread::sleep(std::time::Duration::from_millis(600));
    ensure(
        history_lines(s)? == 9999,
        "application: an invalid rewrite changed settings",
    )?;
    healthy_layout(s, v, "after malformed rewrites", 1)?;

    // A configured layout arriving while an API load is in flight: the
    // configured reload replaces the workspace, so the in-flight load's
    // target is gone by the time its read completes. It must fail and add
    // nothing; the configured layout must be applied exactly once.
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":"churn.scn.ron"}),
    )?;
    s.wait("saved", |s| Ok(notice_text(s, v)?.starts_with("saved")))?;
    let ron = fs::read_to_string(s.directory.join("churn.scn.ron"))?;
    let pipe = s.directory.join("churn.fifo");
    nix::unistd::mkfifo(&pipe, Mode::S_IRWXU)?;
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":pipe,"mapping":[]}),
    )?;
    s.wait("api load in flight", |s| {
        Ok(notice_text(s, v)?.ends_with("..."))
    })?;
    write(s, r#"{"history_lines":9999,"layout":"churn.scn.ron"}"#)?;
    let mut replaced = workspace;
    s.wait(
        "configured layout replaced the workspace during the in-flight load",
        |s| {
            replaced = s.relation(v, "fux::model::Viewing")?;
            Ok(replaced != workspace)
        },
    )?;
    fs::write(&pipe, &ron)?;
    let mut text = String::new();
    s.wait("in-flight load completed", |s| {
        text = notice_text(s, v)?;
        Ok(!text.ends_with("...") && !text.is_empty())
    })?;
    let names_now = workspace_names(s)?;
    s.journal.record(
        "load_vs_configured",
        json!({"notice":text,"workspaces":names_now}),
    )?;
    healthy_layout(s, v, "after a configured layout raced an API load", 1)?;
    ensure(
        s.relation(v, "fux::model::Viewing")? == replaced,
        "application: the in-flight load replaced the workspace the configured layout had just installed",
    )?;

    // Renaming the layout file out from under the watcher must not disturb
    // the server; moving a modified copy back applies it, and a changed
    // workspace name means it is added rather than replacing.
    let ws_now = s.query(WORKSPACE)?.len();
    fs::rename(
        s.directory.join("churn.scn.ron"),
        s.directory.join("churn-away.scn.ron"),
    )?;
    std::thread::sleep(std::time::Duration::from_millis(600));
    healthy_layout(s, v, "after the layout file was renamed away", ws_now)?;
    let renamed = fs::read_to_string(s.directory.join("churn-away.scn.ron"))?
        .replace("\"main\"", "\"churned\"");
    fs::write(s.directory.join("churn-back.scn.ron"), &renamed)?;
    fs::rename(
        s.directory.join("churn-back.scn.ron"),
        s.directory.join("churn.scn.ron"),
    )?;
    let mut count = ws_now;
    s.wait("layout file moved back with a new workspace name was added", |s| {
        count = s.query(WORKSPACE)?.len();
        Ok(count == ws_now + 1)
    })
    .map_err(|e| format!("application: a layout file moved back into place was not reloaded ({ws_now} -> {count} workspaces): {e}"))?;
    healthy_layout(s, v, "after the renamed layout was added", ws_now + 1)?;
    ensure(
        pids.iter().all(|p| alive(*p)),
        "application: configuration churn terminated a process",
    )?;
    // Ten rapid rewrites of the configured layout file, each a full
    // workspace replacement: the count must not drift and nothing may die.
    let target = s.directory.join("churn.scn.ron");
    let base = fs::read_to_string(&target)?;
    let expected_ws = s.query(WORKSPACE)?.len();
    for i in 0..10 {
        let text = if i % 2 == 0 {
            format!("{base}\n")
        } else {
            base.clone()
        };
        fs::write(&target, text)?;
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    s.wait("layout replacements settle", |s| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let a = s.query(WORKSPACE)?.len();
        std::thread::sleep(std::time::Duration::from_millis(300));
        Ok(a == s.query(WORKSPACE)?.len())
    })?;
    healthy_layout(s, v, "after ten configured-layout rewrites", expected_ws)?;
    ensure(
        pids.iter().all(|p| alive(*p)),
        "application: repeated configured-layout replacement terminated a process",
    )?;

    ensure(
        default_history != 7777,
        "test setup: chosen history values must differ from the default",
    )?;
    Ok(())
}
