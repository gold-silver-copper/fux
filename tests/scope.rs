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
    let theirs = a.words("(new-tab (program (size (in (options) \"agent\") 80 24) \"cat\"))")?;
    b.words("(new-tab (program (size (in (options) \"agent\") 80 24) \"cat\"))")?;
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
        assert!(!b.ok(&["(capture (pane 2))"])?.contains("forbidden"));
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
    // It gives its pid once in a session of its own: till then the
    // respawn's signals to the pane's session reach it too.
    let descendant = format!(
        "echo $$ > \"{}\"; while [ ! -f \"{}\" ]; do sleep 0.02; done; \"$FUX\" here kill > \"{}\" 2>&1; echo $? > \"{}\"",
        pid.display(),
        released.display(),
        result.display(),
        status.display()
    );
    let script = format!("setsid sh -c '{descendant}' & echo \"$FUX_PANE\"; exec cat");
    server.ok(&[&format!("(new-tab (argument (argument (program (size (in (options) \"agent\") 80 24) \"sh\") \"-c\") {}))", fux::script::quoted(&script))])?;
    let descendant = pid_in(&pid)?;
    let _reap = Reap(vec![descendant]);
    eventually("the initial program identity", || {
        Ok(server.ok(&["(capture (pane 2))"])?.contains("%2:0"))
    })?;
    server.ok(&["(respawn (kill-first (argument (argument (program (pane 2) \"sh\") \"-c\") \"echo \\\"$FUX_PANE\\\"; exec cat\")))"])?;
    eventually("the replacement's program identity", || {
        Ok(server.ok(&["(capture (pane 2))"])?.contains("%2:1"))
    })?;
    std::fs::write(&released, "go").map_err(e)?;
    eventually("the old caller's status", || {
        Ok(std::fs::read_to_string(&status).is_ok_and(|s| s.ends_with('\n')))
    })?;
    let refused = std::fs::read_to_string(&result).map_err(e)?;
    let status = std::fs::read_to_string(&status).map_err(e)?;
    assert_eq!(status, "1\n", "{refused}");
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
        &["(capture (pane 2))"],
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
        let program = ["%1", "str", "program", "get"];
        let group = server.ok(&[&program[..], &["str", "foreground_group", "get"]].concat())?;
        let group = group.trim().parse::<i32>().ok();
        Ok(group.is_some_and(|g| g != shell && !exists(g)))
    })?;
    Ok(())
}

#[test]
fn a_keyboard_worker_cannot_act_as_a_client_of_a_replacement_run() -> Outcome {
    let a = Server::start("")?;
    let b = Server::start(
        "(sequence! (bind-key \"F5\" (code! (lambda! () (sequence! (kill (focused (me))) #inert)))) #inert)",
    )?;
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
