//! What a script or an agent needs of the server itself: naming the one it
//! means, not stopping or closing the wrong thing, hearing what happens as
//! it happens, sending input only to the screen it read, reaching only what
//! it was granted, and finding its panes again after a crash.
mod support;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use support::*;

/// A command line run with only its words naming the server (`str NAME
/// server`): no FUX_SOCKET, FUX_PANE or FUX_SERVER, and fux's directory
/// under `rt`.
fn named(rt: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> Result<Output, String> {
    let mut command = Command::new(FUX);
    command
        .args(args)
        .env("XDG_RUNTIME_DIR", rt)
        .env("XDG_STATE_HOME", rt.join("state"))
        .env_remove("FUX_SOCKET")
        .env_remove("FUX_PANE")
        .env_remove("FUX_SERVER")
        .stdin(Stdio::null());
    for (k, v) in env {
        command.env(k, v);
    }
    let out = command.output().map_err(e)?;
    Ok(Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn private(dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let rt = dir.join("rt");
    std::fs::create_dir_all(&rt).map_err(e)?;
    std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    Ok(rt)
}

/// `str NAME server` names a server for what follows, `start` starts one
/// and returns once it answers, and `status` says what answers. A FUX_PANE
/// of another server's pane is not acted on; `kill-server` from a server's
/// own pane, and a kill with no target, are refused.
#[test]
fn servers_are_named_and_kills_are_guarded() -> Outcome {
    let server = Server::start("")?;
    let rt = private(&server.dir)?;
    let started = named(&rt, &["str", "t", "server", "start"], &[])?;
    assert_eq!(started.status, 0, "{}", started.stderr);
    let status = named(&rt, &["str", "t", "server", "status", "json"], &[])?;
    assert_eq!(status.status, 0);
    assert!(
        status.stdout.contains(r#""running":true"#) && status.stdout.contains("/fux/t.sock"),
        "{}",
        status.stdout
    );
    let none = named(&rt, &["str", "none", "server", "status"], &[])?;
    assert!(none.stdout.starts_with("no fux server"), "{}", none.stdout);
    // The test server's pane, as an agent in it would carry it, reaches
    // the private server: it is not that server's, and nothing is done to
    // what has its number there.
    let pane = server.ok(&["str", "%1", "pane", "str", "id", "get"])?;
    let theirs = [("FUX_PANE", pane.trim()), ("FUX_SERVER", "another-run")];
    let stray = named(
        &rt,
        &["str", "t", "server", "here", "opts", "capture"],
        &theirs,
    )?;
    assert_eq!(stray.status, 1);
    assert!(
        stray.stderr.contains("another fux server"),
        "{}",
        stray.stderr
    );
    let untargeted = named(&rt, &["str", "t", "server", "here", "kill"], &[])?;
    assert_eq!(untargeted.status, 1);
    assert!(
        untargeted.stderr.starts_with("fux: no_target: "),
        "{}",
        untargeted.stderr
    );
    // From one of its own panes, kill-server is refused; force-kill-server
    // is not.
    let sh = server.ok(&[
        "opts",
        "str",
        "sh",
        "name",
        "str",
        "sh",
        "program",
        "300",
        "wait-idle",
        "run",
    ])?;
    let line = format!("'{FUX}' kill-server; echo status=$?");
    let refused = server.ok(&[
        "str",
        sh.trim(),
        "pane",
        "keys",
        "str",
        &line,
        "text",
        "str",
        "Enter",
        "key",
        "str",
        "status=",
        "wait-text",
        "with-screen",
        "send-keys",
    ])?;
    assert!(refused.contains("status=1"), "{refused}");
    assert!(
        server.ok(&["ls"])?.contains("+1 main"),
        "the server still runs"
    );
    assert_eq!(
        named(&rt, &["str", "t", "server", "kill-server"], &[])?.status,
        0
    );
    Ok(())
}

/// `fux events` tells what happens, a line each, from `ready` on: a pane
/// made, a command's end, its bell, its exit, its close.
#[test]
fn events_tell_what_happens() -> Outcome {
    let server = Server::start("")?;
    let mut events = Command::new(FUX)
        .args(["events-json"])
        .env("FUX_SOCKET", &server.socket)
        .env_remove("FUX_PANE")
        .stdout(Stdio::piped())
        .spawn()
        .map_err(e)?;
    let stdout = events.stdout.take().ok_or("no stdout")?;
    let (lines, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let next = |what: &str| -> Result<String, String> {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let line = read
                .recv_timeout(left)
                .map_err(|_| format!("no {what} event"))?;
            if line.contains(what) {
                return Ok(line);
            }
        }
    };
    next(r#""event":"ready""#)?;
    let pane = server.ok(&[
        "opts",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "printf '\\a'; exit 7",
        "arg",
        "run",
    ])?;
    let pane = pane.trim();
    let created = next(r#""event":"created""#)?;
    assert!(created.contains(r#""name":"agent""#), "{created}");
    next(&format!(r#""event":"created","pane":"{pane}""#))?;
    // The bell and the exit may come in one round, which tells them by
    // kind rather than by time: either first.
    let (mut bell, mut exited) = (false, None);
    while !bell || exited.is_none() {
        let line = next(&format!(r#""pane":"{pane}""#))?;
        bell |= line.contains(r#""event":"bell""#);
        if line.contains(r#""event":"exited""#) {
            exited = Some(line);
        }
    }
    let exited = exited.unwrap_or_default();
    assert!(exited.ends_with(r#""status":7}"#), "{exited}");
    server.ok(&["str", pane, "pane", "kill"])?;
    next(&format!(r#""event":"closed","pane":"{pane}""#))?;
    let _ = events.kill();
    let _ = events.wait();
    Ok(())
}

/// Input meant for one screen goes only to that screen: `if-revision`
/// sends nothing once it changed (status 6). Keys sent to a tab reach each
/// of its panes, and their wait waits for each.
#[test]
fn input_goes_only_where_it_was_meant() -> Outcome {
    let server = Server::start("")?;
    let bash = [
        "str", "bash", "program", "str", "--norc", "arg", "str", "-i", "arg",
    ];
    let a = server.ok(&[
        &["opts", "str", "a", "name"][..],
        &bash,
        &["300", "wait-idle", "run"],
    ]
    .concat())?;
    let a = a.trim();
    let b = server.ok(&[
        &["str", a, "pane", "opts", "horizontal"][..],
        &bash,
        &["split"],
    ]
    .concat())?;
    let b = b.trim();
    server.ok(&["str", b, "pane", "opts", "300", "wait-idle", "wait"])?;
    let revision = server.ok(&["str", a, "pane", "str", "revision", "get"])?;
    let revision = revision.trim();
    server.ok(&["str", a, "pane", "str", "echo moved", "line"])?;
    let stale = server.fux(&[
        "str",
        a,
        "pane",
        "keys",
        "str",
        "y",
        "text",
        "str",
        "Enter",
        "key",
        revision,
        "if-revision",
        "send-keys",
    ])?;
    assert_eq!(stale.status, 6, "{}", stale.stderr);
    assert!(
        stale.stderr.contains("nothing was sent"),
        "{}",
        stale.stderr
    );
    let both = server.ok(&[
        "str",
        a,
        "pane",
        "tab",
        "keys",
        "str",
        "echo both-$((40+2))",
        "text",
        "str",
        "Enter",
        "key",
        "wait-done",
        "send-keys",
    ])?;
    assert_eq!(both, format!("{a} done 0\n{b} done 0\n"));
    for pane in [a, b] {
        let screen = server.ok(&["str", pane, "pane", "screen"])?;
        assert!(screen.contains("both-42"), "{pane}: {screen}");
    }
    Ok(())
}

/// A granted socket reaches its workspace alone: a program run through it
/// lands there, and everything outside it, or everyone's, is refused.
#[test]
fn a_grant_reaches_one_workspace() -> Outcome {
    let server = Server::start("")?;
    let granted = server.dir.join("s").join("granted.sock");
    let socket = granted.to_string_lossy().into_owned();
    server.ok(&["str", "sandbox", "str", &socket, "grant"])?;
    let through = |args: &[&str]| server.fux_env(args, &[("FUX_SOCKET", &socket)]);
    let job = through(&[
        "opts",
        "str",
        "job",
        "name",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "echo inside; sleep 60",
        "arg",
        "run",
    ])?;
    assert_eq!(job.status, 0, "{}", job.stderr);
    let sandbox = server.ok(&["str", "sandbox", "workspace", "info", "json"])?;
    assert!(sandbox.contains(r#""name":"job""#), "{sandbox}");
    for refused in [
        vec!["str", "%1", "pane", "screen"],
        vec!["force-kill-server"],
        vec!["str", "bell", "str", "off", "set"],
        vec!["opts", "new-workspace"],
    ] {
        let out = through(&refused)?;
        assert_eq!(out.status, 1, "{refused:?} went through");
        // A stop asks no workspace: it is refused by the grant itself.
        assert!(
            out.stderr.contains("sandbox") || out.stderr.contains("fux grant"),
            "{refused:?}: {}",
            out.stderr
        );
    }
    let ls = through(&["ls"])?.stdout;
    assert!(
        !ls.contains("main"),
        "only the granted workspace is listed: {ls}"
    );
    server.ok(&["str", &socket, "revoke"])?;
    assert!(!granted.exists(), "a revoked grant's socket is gone");
    Ok(())
}

/// A server killed outright comes back on its socket with the panes it
/// had, their ids, splits and sizes: a program waits for `respawn`, which
/// starts it again. `force-kill-server` forgets it.
#[test]
fn a_crashed_server_comes_back() -> Outcome {
    let server = Server::start("")?;
    let app = server.ok(&[
        "opts",
        "str",
        "app",
        "name",
        "100",
        "30",
        "size",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "echo up; sleep 60",
        "arg",
        "run",
    ])?;
    let app = app.trim();
    let shell = server.ok(&["str", app, "pane", "opts", "horizontal", "split"])?;
    let shell = shell.trim();
    server.ok(&["str", app, "pane", "70", "resize-x"])?;
    let before = server.ok(&["ls"])?;
    // Saved within a second of the last change.
    let layout = server.dir.join("state").join("fux");
    eventually("the layout saved", || {
        let saved = std::fs::read_dir(&layout)
            .map_err(e)?
            .filter_map(Result::ok);
        // The file itself, not the one written beside it and moved in.
        Ok(saved
            .filter(|f| f.path().extension().is_some_and(|x| x == "layout"))
            .filter_map(|f| std::fs::read_to_string(f.path()).ok())
            .any(|text| text.contains("70 resize-x")))
    })?;
    let pid = server.pid().ok_or("the server's pid")?;
    let pid = fuxix::process::Pid::from_raw(i32::try_from(pid).map_err(e)?).ok_or("a pid")?;
    fuxix::process::kill(pid, fuxix::process::Signal::Kill).map_err(e)?;
    // Gone before another starts: else `start` finds it still answering.
    eventually("the server gone", || Ok(!alive(pid.as_raw())))?;
    let socket = server.socket.to_string_lossy().into_owned();
    let again = Command::new(FUX)
        .args(["str", &socket, "socket", "start"])
        .env("XDG_STATE_HOME", server.dir.join("state"))
        .env("XDG_CONFIG_HOME", server.dir.join("config"))
        .env("HOME", &server.dir)
        .env("SHELL", "/bin/sh")
        .env_remove("FUX_PANE")
        .env_remove("FUX_SOCKET")
        .output()
        .map_err(e)?;
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let mut after = String::new();
    // The saved session is run line by line: its sizes come last.
    eventually("the panes back, at their sizes", || {
        after = server.ok(&["ls"])?;
        Ok(after.contains(&format!("{shell} ")) && after.contains(&format!("{app} sh 70x30")))
    })
    .map_err(|e| {
        // The server `start` started logs beside its socket.
        let log = server.socket.parent().map(|d| d.join("fux.log"));
        let log = log
            .and_then(|l| std::fs::read_to_string(l).ok())
            .unwrap_or_default();
        format!("{e}: {after}\nas before:\n{before}\nits log:\n{log}")
    })?;
    assert!(
        after.contains(&format!("{app} sh 70x30")),
        "{app} in\n{after}\nas before:\n{before}"
    );
    let paused = ["str", app, "pane", "str", "paused", "get"];
    assert_eq!(server.ok(&paused)?, "true\n");
    server.ok(&[
        "str",
        app,
        "pane",
        "opts",
        "str",
        "up",
        "wait-text",
        "respawn",
    ])?;
    assert_eq!(server.ok(&paused)?, "false\n");
    server.ok(&["force-kill-server"])?;
    eventually("the layout forgotten", || {
        let saved = std::fs::read_dir(&layout)
            .map_err(e)?
            .filter_map(Result::ok);
        Ok(saved
            .filter(|f| f.path().extension().is_some_and(|x| x == "layout"))
            .count()
            == 0)
    })
}

/// `dump-of` writes a workspace as a program, and running that program
/// makes it again.
#[test]
fn a_dump_makes_its_workspace_again() -> Outcome {
    let server = Server::start("")?;
    let pane = server.ok(&["opts", "str", "build", "name", "new-workspace"])?;
    let worker = server.ok(&[
        "str",
        pane.trim(),
        "pane",
        "opts",
        "vertical",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "echo built; sleep 60",
        "arg",
        "split",
    ])?;
    server.ok(&["str", worker.trim(), "pane", "str", "worker", "rename"])?;
    let dump = server.ok(&["str", "build", "workspace", "dump-of"])?;
    server.ok(&["str", "build", "workspace", "kill"])?;
    assert!(!server.ok(&["ls"])?.contains("build"));
    let file = server.dir.join("build.fux");
    std::fs::write(&file, &dump).map_err(e)?;
    server.ok(&[&file.to_string_lossy()])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.contains(" build\n") && ls.contains(" worker "),
        "{ls}\nfrom\n{dump}"
    );
    let (made, _) = server.newest_pane()?;
    server.ok(&["str", &made, "pane", "str", "built", "await"])?;
    Ok(())
}
