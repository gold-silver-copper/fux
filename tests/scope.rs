//! Identities stay within the server run and program that gave them.
mod support;
use support::*;

fn code(server: &Server, source: &str) -> Result<Output, String> {
    let path = server.dir.join("scope.fux");
    std::fs::write(&path, source).map_err(e)?;
    server.fux(&["--file", &path.to_string_lossy()])
}

#[test]
fn held_handles_refuse_another_server_before_mutating_it() -> Outcome {
    let a = Server::start("")?;
    let b = Server::start("")?;
    for server in [&a, &b] {
        server.ok(&[
            "agent-opts",
            "80",
            "24",
            "size",
            "str",
            "cat",
            "program",
            "run",
        ])?;
    }
    // A scoped attach must refuse the foreign run without changing b.
    std::fs::write(
        a.dir.join("config/fux/config.fux"),
        "str shell list str /no/such/scope-shell end set",
    )
    .map_err(e)?;
    let socket = fux::words::quote(&b.socket.to_string_lossy());
    for (held, action) in [
        ("%2", "kill"),
        ("%2 str forbidden text", "send-keys"),
        ("%2 str harmless text send-keys", "kill"),
        ("%2 tab", "kill"),
        ("%2 workspace", "kill"),
        ("%2 tab panes 0 at", "kill"),
        ("list list %2 end end let saved", "saved 0 at 0 at kill"),
        ("%2 capture expect str forbidden text", "send-keys"),
        ("%2 capture changed", "0 timeout wait"),
        ("%2", "%1 swap-with"),
        ("opts %2 workspace in", "str cat program run"),
        ("opts %2 workspace in 20 10 size", "attach"),
        (
            "%2 str version get let v %2 kill str closed try v changed wait catch drop end",
            "kill",
        ),
    ] {
        let out = code(&a, &format!("{held} str {socket} socket {action}"))?;
        assert_eq!(out.status, 1, "{held} {action}: {}", out.stderr);
        assert!(out.stderr.contains("server run"), "{}", out.stderr);
        assert_eq!(b.panes()?.len(), 2);
        assert!(!b.ok(&["%2", "capture"])?.contains("forbidden"));
    }
    a.type_line("%1", "printf '\\033]52;c;b2xk\\007'")?;
    let mut buffer = String::new();
    eventually("a run-qualified buffer id", || {
        let metadata = a.ok(&["buffers", "json"])?;
        let id = metadata
            .split("\"id\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next());
        if let Some(id) = id {
            buffer = id.to_owned();
        }
        Ok(!buffer.is_empty())
    })?;
    for action in ["buffer", "%2 swap paste-buffer"] {
        let out = code(&a, &format!("str {buffer} str {socket} socket {action}"))?;
        assert_eq!(out.status, 1, "{}", out.stderr);
        assert!(
            out.stderr.contains("bad_value:") && out.stderr.contains("server run"),
            "{}",
            out.stderr
        );
    }
    // Written text is explicitly interpreted by the server selected now.
    let out = code(
        &a,
        &format!("str {socket} socket %2 str local text send-keys"),
    )?;
    assert_eq!(out.status, 0, "{}", out.stderr);
    b.ok(&["%1", "str", "horizontal", "axis", "split"])?;
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
        "run",
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

#[test]
fn reserved_environment_names_are_refused_before_spawning() -> Outcome {
    let server = Server::start("")?;
    for name in ["FUX", "FUX_PANE", "FUX_SOCKET", "FUX_SERVER"] {
        let inherited =
            server.fux(&["str", "copy-environment", "list", "str", name, "end", "set"])?;
        let explicit = server.fux(&[
            "agent-opts",
            "str",
            name,
            "str",
            "forged",
            "env",
            "80",
            "24",
            "size",
            "str",
            "cat",
            "program",
            "run",
        ])?;
        for (out, code) in [(inherited, "config:"), (explicit, "bad_value:")] {
            assert_eq!(out.status, 1, "{name}: {}", out.stderr);
            assert!(
                out.stderr.contains(code)
                    && out.stderr.contains(name)
                    && out.stderr.contains("reserved"),
                "{}",
                out.stderr
            );
        }
        assert_eq!(server.panes()?.len(), 1);
    }
    // The reservation is the exact identity set, not a prefix rule.
    server.ok(&[
        "str",
        "copy-environment",
        "list",
        "str",
        "FUX_EXTRA",
        "end",
        "set",
    ])?;
    server.ok(&[
        "agent-opts",
        "str",
        "FUX_EXTRA",
        "str",
        "allowed",
        "env",
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
        "echo \"$FUX_PANE $FUX_EXTRA\"",
        "arg",
        "run",
    ])?;
    server.exits("%2")?;
    assert_eq!(server.ok(&["%2", "capture"])?, "%2:0 allowed\n");
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn foreground_metadata_observes_exec_and_keeps_launch_arguments_separate() -> Outcome {
    let server = Server::start("")?;
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
        "cd /; exec sleep 300",
        "arg",
        "run",
    ])?;
    eventually("exec sleep", || {
        Ok(server
            .ok(&["%2", "capture", "json"])?
            .contains("\"program\":\"sleep\""))
    })?;
    let json = server.ok(&["%2", "capture", "json"])?;
    let (_, pid) = server.newest_pane()?;
    for field in [
        format!("\"foreground_group\":{pid}"),
        format!("\"foreground_leader\":{pid}"),
        "\"argv\":[\"sleep\",\"300\"]".into(),
        "\"launch_argv\":[\"sh\",\"-c\",\"cd /; exec sleep 300\"]".into(),
        "\"cwd\":\"/\"".into(),
    ] {
        assert!(json.contains(&field), "{field}: {json}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn a_pipeline_keeps_its_group_when_the_observed_leader_is_gone() -> Outcome {
    let server = Server::start("")?;
    server.type_line("%1", "true | sleep 300")?;
    eventually("a foreground group whose leader exited", || {
        let json = server.ok(&["%1", "capture", "json"])?;
        Ok(!json.contains("\"foreground_group\":null")
            && json.contains("\"foreground_leader\":null"))
    })?;
    let json = server.ok(&["%1", "capture", "json"])?;
    for field in ["\"program\":null", "\"argv\":null", "\"cwd\":null"] {
        assert!(json.contains(field), "{field}: {json}");
    }
    Ok(())
}

#[test]
fn a_keyboard_worker_cannot_act_as_a_client_of_a_replacement_run() -> Outcome {
    let a = Server::start("")?;
    let b = Server::start("")?;
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
    let effects = keyboard.program("me focused kill".into());
    let [fux::screens::Dispatch::Run(job)] = effects.as_slice() else {
        return Err(format!("the admitted job: {effects:?}"));
    };
    keyboard.dispatch(job.clone(), 0).map_err(e)?;
    let mut notice = String::new();
    eventually("the old worker's refusal", || {
        for act in keyboard.ran() {
            if let fux::screens::Dispatch::Send(fux::act::Act::Notice { text, error: true }) = act {
                notice = text;
            }
        }
        Ok(keyboard.idle())
    })?;
    assert!(notice.contains("server run"), "{notice}");
    assert_eq!(b.panes()?.len(), 1);
    Ok(())
}
