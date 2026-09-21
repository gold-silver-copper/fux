use super::*;
use crate::trace::FrontendSignal;
use nix::sys::signal::Signal;

/// The README promises that graceful viewer signals restore the terminal and
/// detach. Detaching is the frontend's own explicit request, so the viewer must
/// disappear while the server is otherwise idle: no paint, no mutation, no
/// second viewer. Only read-only queries observe the outcome.
pub(super) fn run(s: &mut Server, signal: FrontendSignal) -> Result<()> {
    let f = s.attach(24, 80)?;
    let viewer = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(viewer, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let pids = running(s)?;
    let signal = match signal {
        FrontendSignal::Interrupt => Signal::SIGINT,
        FrontendSignal::Terminate => Signal::SIGTERM,
        FrontendSignal::Hangup => Signal::SIGHUP,
    };
    s.signal_frontend(f, signal)?;
    s.wait("graceful frontend exit", |s| Ok(s.frontend(f)?.exited))?;
    ensure(
        s.frontend(f)?.exit_success,
        &format!("application: abnormal frontend exit after {signal}"),
    )?;
    ensure(
        s.frontend(f)?.terminal_restored()?,
        "signalled frontend did not restore terminal attributes",
    )?;
    ensure(
        s.frontend(f)?.capture.bytes().ends_with(b"\x1b[?1049l"),
        "signalled frontend did not restore alternate screen",
    )?;
    s.journal.record(
        "frontend_exited",
        json!({"signal":signal.as_str(),"viewer":viewer}),
    )?;
    let result = s.wait("viewer detached by the signalled frontend", |s| {
        Ok(!s
            .query(VIEWER)?
            .iter()
            .any(|row| id(row).ok() == Some(viewer)))
    });
    if let Err(error) = result {
        // Distinguish a lost detach request from a lost server: a paint proves
        // the closed stream is still detectable, so the viewer was recoverable.
        let workspaces = s.query("fux::model::Workspace")?;
        let workspace = id(workspaces.first().ok_or("no workspace")?)?;
        s.rpc(
            "world.insert_components",
            json!({"entity":workspace,"components":{"bevy_ecs::name::Name":"detach-probe"}}),
        )?;
        let removed = s
            .wait("viewer removed only after a paint stimulus", |s| {
                Ok(!s
                    .query(VIEWER)?
                    .iter()
                    .any(|row| id(row).ok() == Some(viewer)))
            })
            .is_ok();
        s.journal
            .record("stale_viewer", json!({"removed_after_paint":removed}))?;
        return Err(format!(
            "application: viewer {viewer} outlived its frontend after {signal}: {error}; removed after an unrelated paint: {removed}. The frontend's explicit detach must remove the viewer without waiting for another paint."
        )
        .into());
    }
    ensure(
        pids.iter().all(|p| alive(*p)),
        "signalled frontend killed the shared process",
    )
}
