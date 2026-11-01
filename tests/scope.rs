//! Identities stay within the server run and program that gave them.
mod support;
use support::*;

/// What carries another server run (one program talks to one server, so
/// it comes as text: a version, a pane's identity) is refused before it
/// changes anything here.
#[test]
fn another_runs_handles_are_refused_before_mutating() -> Outcome {
    let a = Server::start("")?;
    let b = Server::start("")?;
    let theirs = a.words("agent-opts 80 24 size str cat program new-tab")?;
    b.words("agent-opts 80 24 size str cat program new-tab")?;
    for action in [
        "expect str forbidden text send-keys",
        "pane kill",
        "pane tab kill",
        "changed 0 timeout wait",
    ] {
        let out = b.fux(&[&format!("str {} {action}", theirs.trim())])?;
        assert_eq!(out.status, 1, "{action}: {}", out.stderr);
        assert!(out.stderr.contains("server run"), "{}", out.stderr);
        assert_eq!(b.panes()?.len(), 2);
        assert!(!b.ok(&["%2", "capture"])?.contains("forbidden"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn a_background_descendant_cannot_be_here_after_respawn() -> Outcome {
    let server = Server::start("")?;
    let released = server.dir.join("released");
    let result = server.dir.join("result");
    let status = server.dir.join("status");
    let pid = server.dir.join("descendant");
    let descendant = format!(
        "while [ ! -f \"{}\" ]; do sleep 0.02; done; \"$FUX\" here kill > \"{}\" 2>&1; echo $? > \"{}\"",
        released.display(),
        result.display(),
        status.display()
    );
    let script = format!(
        "setsid sh -c '{}' & echo $! > '{}'; echo \"$FUX_PANE\"; exec cat",
        descendant,
        pid.display()
    );
    server.ok(&[
        "agent-opts",
        "80",
        "24",
        "size",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        &script,
        "arg",
        "new-tab",
    ])?;
    let descendant = pid_in(&pid)?;
    let _reap = Reap(vec![descendant]);
    eventually("the initial program identity", || {
        Ok(server.ok(&["%2", "capture"])?.contains("%2:0"))
    })?;
    server.ok(&[
        "%2",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "echo \"$FUX_PANE\"; exec cat",
        "arg",
        "kill-first",
        "respawn",
    ])?;
    eventually("the replacement's program identity", || {
        Ok(server.ok(&["%2", "capture"])?.contains("%2:1"))
    })?;
    std::fs::write(&released, "go").map_err(e)?;
    eventually("the old caller's refusal", || {
        Ok(std::fs::read_to_string(&status).is_ok_and(|s| s.trim() == "1"))
    })?;
    let refused = std::fs::read_to_string(&result).map_err(e)?;
    assert!(
        refused.contains("no longer identifies its program"),
        "{refused}"
    );
    assert!(server.panes()?.iter().any(|(p, _)| p == "%2"));
    // An explicitly named target, and the caller's own directory/environment,
    // remain usable even when its inherited implicit identity is stale.
    let version = server.version("%2")?;
    let run = version.rsplit(':').next().ok_or("run")?;
    let out = server.fux_env(
        &["%2", "capture"],
        &[("FUX_PANE", "%2:0"), ("FUX_SERVER", run)],
    )?;
    assert_eq!(out.status, 0, "{}", out.stderr);
    Ok(())
}

/// The terminal's foreground group is reported while a pipeline runs on
/// after its leader exited: no other member is guessed to replace it.
#[cfg(target_os = "linux")]
#[test]
fn a_pipeline_keeps_its_group_when_its_leader_is_gone() -> Outcome {
    let server = Server::start("")?;
    let (_, shell) = server.newest_pane()?;
    server.type_line("%1", "true | sleep 300")?;
    eventually("a foreground group whose leader exited", || {
        let group = server.ok(&["%1", "str", "foreground_group", "get"])?;
        let group = group.trim().parse::<i32>().ok();
        Ok(group.is_some_and(|g| {
            g != shell
                && fuxix::process::Pid::from_raw(g).is_some_and(|g| !fuxix::process::exists(g))
        }))
    })?;
    Ok(())
}

#[test]
fn a_keyboard_worker_cannot_act_as_a_client_of_a_replacement_run() -> Outcome {
    let a = Server::start("")?;
    let b = Server::start("str F5 str 'me focused kill' bind-root")?;
    let mut client = b.attach(24, 80)?;
    client.ready()?;
    let id = client.id.as_deref().ok_or("an attached client id")?;
    let old = a.version("%1")?;
    let run = old
        .rsplit(':')
        .next()
        .ok_or("run")?
        .parse()
        .map_err(|()| "run")?;
    let mut keyboard = fux::client::Keyboard::new(
        &b.socket_path()?,
        id.parse().map_err(e)?,
        run,
        b.state.clone(),
        false,
    )
    .map_err(e)?;
    let effects = keyboard.bytes(b"\x1b[15~", std::time::Instant::now());
    let [fux::screens::Dispatch::Run(job)] = effects.as_slice() else {
        return Err(format!("the admitted job: {effects:?}"));
    };
    keyboard.dispatch(job.clone(), 0).map_err(e)?;
    let mut notice = String::new();
    eventually("the old worker's refusal", || {
        for act in keyboard.ran() {
            if let fux::screens::Dispatch::Send(fux_wire::act::Act::Notice { text, error: true }) =
                act
            {
                notice = text;
            }
        }
        Ok(keyboard.idle())
    })?;
    assert!(notice.contains("server run"), "{notice}");
    assert_eq!(b.panes()?.len(), 1);
    Ok(())
}
