//! Bounded local fux pane census for owned-worktree removal.
use super::model::{Journal, LaunchPhase, Target};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

pub(super) fn check(target: &Target, path: &Path, deadline: Instant) -> Result<()> {
    let pane = super::submit::target_pane(target, deadline)
        .context("session use is uncertain; removal refused")?;
    let cwd = PathBuf::from(
        pane.get("cwd")
            .and_then(Value::as_str)
            .context("session cwd unavailable; removal refused")?,
    );
    anyhow::ensure!(cwd.is_absolute(), "session cwd is not absolute");
    let canonical = fs::canonicalize(&cwd).context("session cwd uncertain; removal refused")?;
    anyhow::ensure!(
        !cwd.starts_with(path) && !canonical.starts_with(path),
        "worktree has an active session; removal refused"
    );
    let pid = target
        .pid
        .and_then(|pid| i32::try_from(pid).ok())
        .context("session process ID unavailable; removal refused")?;
    let paths = crate::platform::usage::process_tree_cwds(pid, deadline)
        .context("current session cwd uncertain; removal refused")?;
    for current in paths {
        anyhow::ensure!(current.is_absolute(), "current session cwd is not absolute");
        let canonical = fs::canonicalize(&current)
            .context("current session cwd cannot be resolved; removal refused")?;
        anyhow::ensure!(
            !current.starts_with(path) && !canonical.starts_with(path),
            "worktree is the current directory of an active session; removal refused"
        );
    }
    // Bind the OS sample to the still-live fux target; a process exit or
    // server/workspace replacement during sampling must not authorize deletion.
    super::submit::verify_target(target, deadline)
        .context("session changed during cwd sampling; removal refused")?;
    Ok(())
}

pub(super) fn unadopted(journal: &Journal, path: &Path, deadline: Instant) -> Result<()> {
    let mut runtimes = BTreeSet::new();
    if let Ok(runtime) = crate::fux::runtime() {
        runtimes.insert(runtime);
    }
    for session in journal.sessions.values() {
        let closed = journal.sessions.values().any(|other| {
            other.target.identity() == session.target.identity()
                && other
                    .launch
                    .as_ref()
                    .and_then(|id| journal.launches.get(id))
                    .is_some_and(|launch| launch.phase == LaunchPhase::Closed)
        });
        if !closed {
            runtimes.insert(session.target.runtime.clone());
        }
    }
    let mut count = 0;
    for runtime in runtimes {
        for target in discover(&runtime, deadline)? {
            count += 1;
            anyhow::ensure!(
                count <= 256,
                "pane use inspection limit exceeded; removal refused"
            );
            check(&target, path, deadline)?;
        }
    }
    Ok(())
}

fn discover(runtime: &Path, deadline: Instant) -> Result<Vec<Target>> {
    let manager = runtime.join("manager.sock");
    if !manager.try_exists()? {
        // Standalone worktrees remain usable with no fux runtime. A partial
        // runtime containing sockets is uncertain, not proof that no pane exists.
        match fs::read_dir(runtime) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
            Ok(entries) => {
                for (index, entry) in entries.enumerate() {
                    anyhow::ensure!(index < 256, "runtime inspection limit exceeded");
                    anyhow::ensure!(
                        entry?.path().extension().is_none_or(|ext| ext != "sock"),
                        "fux manager unavailable with retained sockets; removal refused"
                    );
                }
                return Ok(Vec::new());
            }
        }
    }
    let response = crate::fux::request_until(&manager, json!({"request":"list"}), deadline)
        .context("fux pane discovery unavailable; removal refused")?;
    let names = response
        .get("names")
        .and_then(Value::as_array)
        .context("invalid workspace discovery")?;
    anyhow::ensure!(names.len() <= 64, "workspace inspection limit exceeded");
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    let mut instance: Option<String> = None;
    for name in names {
        let name = name
            .as_str()
            .filter(|name| super::model::workspace(name))
            .context("invalid workspace name")?;
        anyhow::ensure!(seen.insert(name), "duplicate workspace discovery");
        let response = crate::fux::completed_until(
            &runtime.join(format!("{name}.sock")),
            json!({"id":1,"command":"list","instance":instance}),
            deadline,
        )?;
        let listing = response
            .pointer("/result/value")
            .context("missing pane listing")?;
        let current = listing
            .get("instance")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 128)
            .context("invalid server instance")?;
        anyhow::ensure!(
            instance.as_deref().is_none_or(|value| value == current),
            "fux server changed during discovery"
        );
        instance = Some(current.to_owned());
        let spaces = listing
            .get("workspaces")
            .and_then(Value::as_array)
            .context("invalid workspaces")?;
        anyhow::ensure!(spaces.len() == 1, "unexpected workspace count");
        let space = spaces.first().context("missing workspace")?;
        anyhow::ensure!(
            space.get("name").and_then(Value::as_str) == Some(name),
            "workspace changed during discovery"
        );
        let stream = space
            .pointer("/event_cursor/stream")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .context("missing workspace lifetime")?;
        let tabs = space
            .get("tabs")
            .and_then(Value::as_array)
            .context("invalid tabs")?;
        anyhow::ensure!(tabs.len() <= 32, "tab inspection limit exceeded");
        for tab in tabs {
            for pane in tab
                .get("panes")
                .and_then(Value::as_array)
                .context("invalid panes")?
            {
                anyhow::ensure!(targets.len() < 256, "pane inspection limit exceeded");
                let pane_id = pane
                    .get("id")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .context("invalid pane ID")?;
                let pid = pane
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .context("missing pane process")?;
                targets.push(Target {
                    runtime: runtime.to_owned(),
                    instance: current.to_owned(),
                    workspace: name.into(),
                    stream,
                    pane: pane_id,
                    pid: Some(pid),
                });
            }
        }
    }
    Ok(targets)
}
