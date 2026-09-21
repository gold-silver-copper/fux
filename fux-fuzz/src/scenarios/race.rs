use super::*;
use nix::sys::stat::Mode;
use std::path::Path;

const PANE_VIEW: &str = "fux::model::PaneView";
const WORKSPACE: &str = "fux::model::Workspace";

fn state(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("race viewer disappeared")?;
    component(row, VIEWER)
}
fn notice(s: &mut Server, viewer: u64) -> Result<Value> {
    Ok(state(s, viewer)?
        .get("notice")
        .cloned()
        .unwrap_or(Value::Null))
}
fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    Ok(notice(s, viewer)?
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn count(s: &mut Server, component: &str) -> Result<usize> {
    Ok(s.query(component)?.len())
}
fn screen(s: &mut Server, f: usize) -> Result<String> {
    Ok(s.frontend(f)?.parser.screen().contents())
}
fn api_attach(s: &mut Server, workspace: Option<&str>) -> Result<u64> {
    s.rpc(
        "fux.attach",
        json!({"workspace":workspace,"rows":24,"cols":80}),
    )?
    .get("viewer")
    .and_then(Value::as_u64)
    .ok_or("attach returned no viewer".into())
}
fn settled(s: &mut Server, viewer: u64, label: &str) -> Result<Value> {
    let mut last = Value::Null;
    s.wait(label, |s| {
        last = notice(s, viewer)?;
        let text = last.get("text").and_then(Value::as_str).unwrap_or_default();
        Ok(!text.is_empty() && !text.ends_with("..."))
    })?;
    Ok(last)
}
/// A named pipe holds a scene load in flight until it is written, so the race
/// with a concurrent command is exact rather than a matter of scheduling.
fn fifo(s: &mut Server, name: &str) -> Result<std::path::PathBuf> {
    let path = s.directory.join(name);
    nix::unistd::mkfifo(&path, Mode::S_IRWXU)?;
    Ok(path)
}
fn release(path: &Path, text: &str) -> Result<()> {
    // Opening for write blocks until the loader has opened for read.
    fs::write(path, text)?;
    Ok(())
}
fn rename_prompt_open(s: &mut Server, f: usize) -> Result<bool> {
    Ok(screen(s, f)?.to_lowercase().contains("rename"))
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(screen(s, f)?.contains("DEFAULT-SHELL"))
    })?;
    let shell_pid = *running(s)?.first().ok_or("no shell")?;
    let ws1 = s.relation(v, "fux::model::Viewing")?;
    let leaf1 = s.relation(v, "fux::model::Focused")?;
    let pane1 = s
        .query(PANE_VIEW)?
        .iter()
        .find(|r| id(r).ok() == Some(leaf1))
        .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64())
        .ok_or("first pane")?;

    // A second workspace holding a second view of the shell's process, so the
    // process outlives the first workspace and a saved layout that names it
    // stays loadable after that workspace is gone.
    s.control(v, json!({"kind":"workspace_new","name":"second"}))?;
    s.wait("second workspace", |s| Ok(count(s, WORKSPACE)? == 2))?;
    let ws2 = s.relation(v, "fux::model::Viewing")?;
    ensure(ws2 != ws1, "viewer did not move to the new workspace")?;
    let tab2 = s.relation(v, "fux::model::OnTab")?;
    let extra = s
        .rpc(
            "world.spawn_entity",
            json!({"components":{PANE_VIEW:{"pane":pane1}}}),
        )?
        .get("entity")
        .and_then(Value::as_u64)
        .ok_or("spawn returned no entity")?;
    s.rpc(
        "world.reparent_entities",
        json!({"entities":[extra],"parent":tab2}),
    )?;
    s.wait("shared view painted", |s| {
        Ok(screen(s, f)?.contains("DEFAULT-SHELL"))
    })?;

    // Save the first workspace's layout to a file for later replay.
    let saved_path = "race-layout.scn.ron";
    s.control(
        v,
        json!({"kind":"save_layout","workspace":ws1,"path":saved_path}),
    )?;
    let saved = settled(s, v, "save completed")?;
    ensure(
        saved.get("error") != Some(&json!(true)),
        &format!("save failed: {saved}"),
    )?;
    let ron = fs::read_to_string(s.directory.join(saved_path))?;

    // Case A: the requesting workspace is closed while its load is in flight.
    // The load named a workspace that no longer exists by the time it
    // completes, so it must fail like any command naming a missing entity,
    // and must not add a workspace nobody asked for.
    let pipe_a = fifo(s, "load-a.fifo")?;
    s.control(
        v,
        json!({"kind":"load_layout","workspace":ws1,"path":pipe_a,"mapping":[]}),
    )?;
    s.wait(
        "load in flight",
        |s| Ok(notice_text(s, v)?.ends_with("...")),
    )?;
    s.control(v, json!({"kind":"close","subject":{"workspace":ws1}}))?;
    s.wait("first workspace closed", |s| Ok(count(s, WORKSPACE)? == 1))?;
    ensure(
        alive(shell_pid),
        "closing the first workspace killed a shared process",
    )?;
    let before = count(s, WORKSPACE)?;
    release(&pipe_a, &ron)?;
    let done = settled(s, v, "in-flight load completed after its workspace closed")?;
    let after = count(s, WORKSPACE)?;
    s.journal.record(
        "load_after_workspace_closed",
        json!({"notice":done,"workspaces_before":before,"workspaces_after":after}),
    )?;
    ensure(
        after == before,
        &format!(
            "application: a layout load whose workspace was closed mid-flight added a workspace ({before} became {after}); notice {done}"
        ),
    )?;
    ensure(
        done.get("error") == Some(&json!(true)),
        &format!(
            "application: a layout load whose workspace was closed mid-flight reported success: {done}"
        ),
    )?;

    // Case B: the requesting viewer detaches while its load is in flight. The
    // task is dropped with the viewer; nothing may be applied afterwards.
    let w = api_attach(s, None)?;
    let pipe_b = fifo(s, "load-b.fifo")?;
    s.control(
        w,
        json!({"kind":"load_layout","workspace":ws2,"path":pipe_b,"mapping":[]}),
    )?;
    s.wait("second load in flight", |s| {
        Ok(notice_text(s, w)?.ends_with("..."))
    })?;
    s.control(w, json!({"kind":"detach"}))?;
    s.wait("api viewer detached", |s| {
        Ok(!s.query(VIEWER)?.iter().any(|r| id(r).ok() == Some(w)))
    })?;
    let before = count(s, WORKSPACE)?;
    let views_before = count(s, PANE_VIEW)?;
    // The dropped task may never read; write from a thread so a blocked open
    // cannot stall the harness, then give the server time to misbehave.
    let pipe_b_thread = pipe_b.clone();
    let ron_b = ron.clone();
    let writer = std::thread::spawn(move || {
        let _ = fs::write(pipe_b_thread, ron_b);
    });
    std::thread::sleep(std::time::Duration::from_millis(400));
    s.healthy()?;
    let after = count(s, WORKSPACE)?;
    let views_after = count(s, PANE_VIEW)?;
    s.journal.record(
        "load_after_viewer_detached",
        json!({"workspaces_before":before,"workspaces_after":after,"views_before":views_before,"views_after":views_after}),
    )?;
    ensure(
        after == before && views_after == views_before,
        &format!(
            "application: a load whose viewer detached mid-flight still changed the layout ({before}->{after} workspaces, {views_before}->{views_after} views)"
        ),
    )?;
    // Unblock the writer if the loader never opened the pipe.
    if !writer.is_finished() {
        let _ = fs::OpenOptions::new().read(true).open(&pipe_b);
    }
    let _ = writer.join();

    // Case C: a rename prompt is open in one viewer when a second viewer
    // closes the prompt's target. Confirming must cancel, never rename the
    // pane that took over focus.
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HRACE-B'; exec cat > /dev/null"}))?;
    let target = s.relation(v, "fux::model::Focused")?;
    s.wait("second pane painted", |s| {
        Ok(screen(s, f)?.contains("RACE-B"))
    })?;
    s.send(f, b"\x02")?;
    s.wait("prefix open", |s| Ok(screen(s, f)?.contains("Panes")))?;
    s.send(f, b"r")?;
    s.wait("rename prompt open", |s| rename_prompt_open(s, f))?;
    let other = api_attach(s, None)?;
    s.control(other, json!({"kind":"close","subject":{"pane":target}}))?;
    s.wait("target closed by the other viewer", |s| {
        Ok(!s
            .query(PANE_VIEW)?
            .iter()
            .any(|r| id(r).ok() == Some(target)))
    })?;
    // The first key cancels the stale overlay and sets the notice; any key
    // after it is ordinary input, which clears the notice. Send one key,
    // observe the cancellation, then type the rest.
    s.send(f, b"H")?;
    let mut text = String::new();
    s.wait("stale prompt cancelled", |s| {
        text = notice_text(s, v)?;
        Ok(text.contains("target changed") || text.contains("no longer exists"))
    })
    .map_err(|e| format!("application: confirming a prompt whose target was closed elsewhere: notice {text:?}: {e}"))?;
    s.send(f, b"IJACKED")?;
    s.send(f, b"\r")?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    s.pump()?;
    ensure(
        !screen(s, f)?.contains("HIJACKED"),
        "application: a stale rename prompt renamed the pane that took over focus",
    )?;
    ensure(
        alive(shell_pid),
        "the shared shell died during the prompt race",
    )?;
    s.control(other, json!({"kind":"detach"}))?;

    // Case D: detaching a viewer that is in copy mode restores its terminal
    // and leaves the process alone.
    s.send(f, b"\x02")?;
    s.wait("prefix open again", |s| Ok(screen(s, f)?.contains("Panes")))?;
    s.send(f, b"c")?;
    s.wait("copy mode", |s| Ok(notice_text(s, v)?.starts_with("Copy:")))?;
    s.send(f, b" ")?;
    s.frontend(f)?.expected_exit = true;
    s.control(v, json!({"kind":"detach"}))?;
    s.wait("frontend exits on detach during copy mode", |s| {
        Ok(s.frontend(f)?.exited)
    })?;
    ensure(
        s.frontend(f)?.exit_success,
        "application: detaching during copy mode exited the frontend abnormally",
    )?;
    ensure(
        s.frontend(f)?.terminal_restored()?,
        "application: detaching during copy mode did not restore the terminal",
    )?;
    ensure(
        alive(shell_pid),
        "application: detaching during copy mode killed the process",
    )?;
    Ok(())
}
