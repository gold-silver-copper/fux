//! What a script or an agent needs of the server itself: naming the one it
//! means, not stopping or closing the wrong thing, hearing what happens as
//! it happens, sending input only to the screen it read, reaching only what
//! it was granted, and finding its panes again after a crash.
mod support;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use support::*;

/// A command line run with only `-L`/`-S` naming the server: no
/// FUX_SOCKET, FUX_PANE or FUX_SERVER, and fux's directory under `rt`.
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

/// `-L NAME` names a server on every command, `server --background` starts
/// one and returns once it answers, and `status` says what answers. A
/// FUX_PANE of another server's pane is not acted on; `kill-server` from a
/// server's own pane, and a kill with no target, are refused.
#[test]
fn servers_are_named_and_kills_are_guarded() -> Outcome {
    let server = Server::start("")?;
    let rt = private(&server.dir)?;
    let started = named(&rt, &["-L", "t", "server", "--background"], &[])?;
    assert_eq!(started.status, 0, "{}", started.stderr);
    assert!(
        started.stdout.trim_end().ends_with("/fux/t.sock"),
        "{}",
        started.stdout
    );
    let status = named(&rt, &["-L", "t", "status", "--json"], &[])?;
    assert_eq!(status.status, 0);
    assert!(
        status.stdout.contains(r#""running":true"#),
        "{}",
        status.stdout
    );
    let none = named(&rt, &["-L", "none", "status"], &[])?;
    assert_eq!(
        (none.status, none.stdout.starts_with("no fux server")),
        (1, true)
    );
    // The test server's pane, as an agent in it would carry it, reaches
    // the private server: it is not that server's, and nothing is done to
    // what has its number there.
    let pane = server.ok(&["ls", "-t", "%1", "--get", "id"])?;
    let theirs = [("FUX_PANE", pane.trim()), ("FUX_SERVER", "another-run")];
    let stray = named(&rt, &["-L", "t", "capture-pane"], &theirs)?;
    assert_eq!(stray.status, 1);
    assert!(
        stray.stderr.contains("another fux server"),
        "{}",
        stray.stderr
    );
    let untargeted = named(&rt, &["-L", "t", "kill-pane", "--json"], &[])?;
    assert_eq!(untargeted.status, 1);
    assert!(
        untargeted.stderr.contains(r#""error":"no_target""#),
        "{}",
        untargeted.stderr
    );
    // From one of its own panes, kill-server needs --force.
    let sh = server.ok(&["run", "-n", "sh", "--wait-idle", "300", "--", "sh"])?;
    let line = format!("'{FUX}' kill-server; echo status=$?");
    let refused = server.ok(&[
        "send-keys",
        "-t",
        sh.trim(),
        "--text",
        &line,
        "Enter",
        "--wait-text",
        "status=",
    ])?;
    assert!(refused.contains("status=1"), "{refused}");
    assert!(
        server.ok(&["ls"])?.contains("+1 main"),
        "the server still runs"
    );
    assert_eq!(named(&rt, &["-L", "t", "kill-server"], &[])?.status, 0);
    Ok(())
}

/// `fux events` tells what happens, a line each, from `ready` on: a pane
/// made, a command's end, its bell, its exit, its close.
#[test]
fn events_tell_what_happens() -> Outcome {
    let server = Server::start("")?;
    let mut events = Command::new(FUX)
        .args(["events", "--json"])
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
    let pane = server.ok(&["run", "--", "sh", "-c", "printf '\\a'; exit 7"])?;
    let pane = pane.trim();
    let created = next(r#""event":"created""#)?;
    assert!(created.contains(r#""name":"agent""#), "{created}");
    next(&format!(r#""event":"created","pane":"{pane}""#))?;
    next(&format!(r#""event":"bell","pane":"{pane}""#))?;
    let exited = next(&format!(r#""event":"exited","pane":"{pane}""#))?;
    assert!(exited.ends_with(r#""status":7}"#), "{exited}");
    server.ok(&["kill-pane", "-t", pane])?;
    next(&format!(r#""event":"closed","pane":"{pane}""#))?;
    let _ = events.kill();
    let _ = events.wait();
    Ok(())
}

/// Input meant for one screen goes only to that screen: `--if-revision`
/// sends nothing once it changed (status 6). Keys sent to several panes
/// reach each, and their wait waits for each.
#[test]
fn input_goes_only_where_it_was_meant() -> Outcome {
    let server = Server::start("")?;
    let a = server.ok(&[
        "run",
        "-n",
        "a",
        "--wait-idle",
        "300",
        "--",
        "bash",
        "--norc",
        "-i",
    ])?;
    let b = server.ok(&[
        "split",
        "-h",
        "-t",
        a.trim(),
        "--run",
        "--",
        "bash",
        "--norc",
        "-i",
    ])?;
    let (a, b) = (a.trim(), b.trim());
    server.ok(&["wait", "-t", b, "--idle", "300"])?;
    let revision = server.ok(&["ls", "-t", a, "--get", "revision"])?;
    let revision = revision.trim();
    server.ok(&[
        "send-keys",
        "-t",
        a,
        "--text",
        "echo moved",
        "Enter",
        "--wait-done",
    ])?;
    let stale = server.fux(&[
        "send-keys",
        "-t",
        a,
        "--if-revision",
        revision,
        "y",
        "Enter",
    ])?;
    assert_eq!(stale.status, 6, "{}", stale.stderr);
    assert!(
        stale.stderr.contains("nothing was sent"),
        "{}",
        stale.stderr
    );
    let both = server.ok(&[
        "send-keys",
        "-t",
        a,
        "-t",
        b,
        "--text",
        "echo both-$((40+2))",
        "Enter",
        "--wait-done",
    ])?;
    assert_eq!(both, format!("{a} done 0\n{b} done 0\n"));
    for pane in [a, b] {
        let screen = server.ok(&["capture-pane", "-t", pane])?;
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
    server.ok(&["grant", "-t", "sandbox", "--socket", &socket])?;
    let through = |args: &[&str]| server.fux_env(args, &[("FUX_SOCKET", &socket)]);
    let job = through(&[
        "run",
        "-n",
        "job",
        "--",
        "sh",
        "-c",
        "echo inside; sleep 60",
    ])?;
    assert_eq!(job.status, 0, "{}", job.stderr);
    assert!(server.ok(&["ls", "-t", "sandbox"])?.contains("job"));
    for refused in [
        vec!["capture-pane", "-t", "%1"],
        vec!["kill-server", "--force"],
        vec!["set", "bell", "off"],
        vec!["new-workspace"],
    ] {
        let out = through(&refused)?;
        assert_eq!(out.status, 1, "{refused:?} went through");
        assert!(
            out.stderr.contains("sandbox"),
            "{refused:?}: {}",
            out.stderr
        );
    }
    let ls = through(&["ls"])?.stdout;
    assert!(
        !ls.contains("main"),
        "only the granted workspace is listed: {ls}"
    );
    server.ok(&["grant", "--revoke", "--socket", &socket])?;
    assert!(!granted.exists(), "a revoked grant's socket is gone");
    Ok(())
}

/// A server killed outright comes back on its socket with the panes it
/// had, their ids, splits and sizes: a program waits for `respawn`, which
/// starts it again. `kill-server` forgets it.
#[test]
fn a_crashed_server_comes_back() -> Outcome {
    let server = Server::start("")?;
    let app = server.ok(&[
        "run",
        "-n",
        "app",
        "--size",
        "100x30",
        "--",
        "sh",
        "-c",
        "echo up; sleep 60",
    ])?;
    let app = app.trim();
    let shell = server.ok(&["split", "-h", "-t", app])?;
    let shell = shell.trim();
    server.ok(&["resize-pane", "-t", app, "-x", "70"])?;
    let before = server.ok(&["ls"])?;
    // Saved within a second of the last change.
    let layout = server.dir.join("state").join("fux");
    eventually("the layout saved", || {
        let saved = std::fs::read_dir(&layout)
            .map_err(e)?
            .filter_map(Result::ok);
        Ok(saved
            .filter_map(|f| std::fs::read_to_string(f.path()).ok())
            .any(|text| text.contains("-x 70")))
    })?;
    let pid = server.pid().ok_or("the server's pid")?;
    let pid = fuxix::process::Pid::from_raw(i32::try_from(pid).map_err(e)?).ok_or("a pid")?;
    fuxix::process::kill(pid, fuxix::process::Signal::Kill).map_err(e)?;
    let socket = server.socket.to_string_lossy().into_owned();
    let again = Command::new(FUX)
        .args(["server", "--socket", &socket, "--background", "--config"])
        .arg(server.dir.join("fux.conf"))
        .env("XDG_STATE_HOME", server.dir.join("state"))
        .env("HOME", &server.dir)
        .env("SHELL", "/bin/sh")
        .env_remove("FUX_PANE")
        .output()
        .map_err(e)?;
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let after = server.ok(&["ls"])?;
    for line in [format!("{app} sh 70x30"), format!("{shell} ")] {
        assert!(
            after.contains(&line),
            "{line:?} in\n{after}\nas before:\n{before}"
        );
    }
    assert_eq!(server.ok(&["ls", "-t", app, "--get", "paused"])?, "true\n");
    assert!(
        server
            .ok(&["capture-pane", "-t", app])?
            .contains("respawn -t")
    );
    server.ok(&["respawn", "-t", app, "--wait-text", "up"])?;
    assert_eq!(server.ok(&["ls", "-t", app, "--get", "paused"])?, "false\n");
    server.ok(&["kill-server", "--force"])?;
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

/// A layout file is made whole or not at all, with fresh ids, and
/// `dump-layout` writes what it made as one again.
#[test]
fn layouts_are_made_whole_or_not_at_all() -> Outcome {
    let server = Server::start("")?;
    let good = "new-tab -n build --id @1 --id %1\n\
                split -v -t %1 --run --id %2 -- sh -c 'echo built; sleep 60'\n\
                rename -t %2 worker\n";
    let file = server.dir.join("good.fux");
    std::fs::write(&file, format!("{good}split -h -t %9\n")).map_err(e)?;
    let path = file.to_string_lossy().into_owned();
    let failed = server.fux(&["layout", &path])?;
    assert_eq!(failed.status, 1);
    assert!(failed.stderr.contains("line 4"), "{}", failed.stderr);
    assert!(!server.ok(&["ls"])?.contains("build"), "nothing was made");
    std::fs::write(&file, good).map_err(e)?;
    let made = server.ok(&["layout", &path])?;
    // The failed try's ids are not given again.
    let mut ids = made.split_whitespace();
    assert_eq!(ids.next(), Some("+3"), "{made}");
    let pane = ids.nth(1).ok_or("the tab's pane")?;
    let worker = ids.next().ok_or("the split's pane")?;
    server.ok(&["wait", "-t", worker, "--text", "built"])?;
    let dump = server.ok(&["dump-layout", "-t", "build"])?;
    assert!(
        dump.contains(&format!("split -v -t {pane} --run")),
        "{dump}"
    );
    assert!(
        dump.contains(&format!("rename -t {worker} worker")),
        "{dump}"
    );
    Ok(())
}
