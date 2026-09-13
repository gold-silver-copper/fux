//! Bounded local fux pane census for owned-worktree removal.
use super::model::{Journal, LaunchPhase, Target};
use anyhow::{Context, Result};
use std::{collections::BTreeSet, fs, path::Path, time::Instant};

pub(super) fn check(target: &Target, path: &Path, deadline: Instant) -> Result<()> {
    let pane = super::submit::target_pane(target, deadline)
        .context("session use is uncertain; removal refused")?;
    let cwd = pane.cwd;
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
    let endpoint = crate::fux::endpoint::Endpoint::new(runtime);
    let manager = endpoint.manager();
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
    let names = crate::fux::manager::names(runtime, deadline)
        .context("fux pane discovery unavailable; removal refused")?;
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    let mut instance: Option<String> = None;
    for name in names {
        let name = name.as_str();
        anyhow::ensure!(
            seen.insert(name.to_owned()),
            "duplicate workspace discovery"
        );
        let listing =
            crate::fux::snapshot::list(&endpoint.workspace(name)?, instance.as_deref(), deadline)?;
        let current = listing.instance.as_str();
        instance = Some(current.to_owned());
        let space = listing.workspaces.first().context("missing workspace")?;
        anyhow::ensure!(space.name == name, "workspace changed during discovery");
        let stream = space.event_cursor.stream;
        for tab in &space.tabs {
            for pane in &tab.panes {
                anyhow::ensure!(targets.len() < 256, "pane inspection limit exceeded");
                let pane_id = pane.id;
                let pid = pane.pid.context("missing pane process")?;
                targets.push(Target {
                    origin: None,
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
