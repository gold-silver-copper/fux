//! What a script or an agent needs of the server itself: naming the one it
//! means, not stopping or closing the wrong thing, hearing what happens as
//! it happens, sending keys to every pane of a tab, reaching only what
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

/// The pane an answer names: an action answers its version,
/// `%N:COUNTS:RUN`.
fn first_word(answer: &str) -> &str {
    answer.split(':').next().unwrap_or_default().trim()
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
        status.stdout.contains(r#""compatible":true"#) && status.stdout.contains("/fux/t.sock"),
        "{}",
        status.stdout
    );
    // With no server, status fails, so a shell's `if fux status` reads it.
    let none = named(&rt, &["str", "none", "server", "status"], &[])?;
    assert_eq!(none.status, 1, "{}", none.stderr);
    assert!(
        none.stderr.starts_with("fux: no_server: ") && none.stderr.contains("no fux server at"),
        "{}",
        none.stderr
    );
    // The test server's pane, as an agent in it would carry it, reaches
    // the private server: it is not that server's, and nothing is done to
    // what has its number there.
    let pane = server.ok(&["str", "%1", "pane", "str", "id", "get"])?;
    let theirs = [("FUX_PANE", pane.trim()), ("FUX_SERVER", "another-run")];
    let stray = named(&rt, &["str", "t", "server", "here", "capture"], &theirs)?;
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
    let sh = server.ok(&["opts", "str", "sh", "name", "str", "sh", "program", "run"])?;
    let sh = first_word(&sh);
    let line = format!("'{FUX}' kill-server; echo status=$?");
    let sent = server.ok(&[
        "str",
        sh,
        "pane",
        "str",
        &line,
        "text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    let since = sent.split_whitespace().next().unwrap_or_default();
    server.until(sh, since, |c| c.contains("status=1"))?;
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

/// A server a client starts is a daemon (daemon(7)): it holds none of the
/// client's descriptors beyond stdio, and keeps no directory of its busy.
#[cfg(target_os = "linux")]
#[test]
fn a_started_server_keeps_nothing_of_its_client() -> Outcome {
    let server = Server::start("")?;
    let rt = private(&server.dir)?;
    let here = server.dir.join("here");
    std::fs::create_dir_all(&here).map_err(e)?;
    let held = here.join("held");
    let started = {
        let _guard = spawning()?;
        let file = std::fs::File::create(&held).map_err(e)?;
        let _leaked = fuxix::io::duplicate_inheritable(&file).map_err(e)?;
        Command::new(FUX)
            .args(["str", "d", "server", "start"])
            .current_dir(&here)
            .env("XDG_RUNTIME_DIR", &rt)
            .env("XDG_STATE_HOME", rt.join("state"))
            .env_remove("FUX_SOCKET")
            .env_remove("FUX_PANE")
            .env_remove("FUX_SERVER")
            .stdin(Stdio::null())
            .output()
            .map_err(e)?
    };
    assert!(started.status.success(), "{started:?}");
    let socket = rt.join("fux").join("d.sock");
    let pid = serving(&socket).ok_or("no server serves d")?;
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).map_err(e)?;
    let fds = std::fs::read_dir(format!("/proc/{pid}/fd")).map_err(e)?;
    let holds = (fds.filter_map(Result::ok))
        .filter_map(|fd| std::fs::read_link(fd.path()).ok())
        .any(|target| target == held);
    let _ = named(&rt, &["str", "d", "server", "force-kill-server"], &[]);
    assert_eq!(cwd, std::path::Path::new("/"), "the server's directory");
    assert!(!holds, "the server holds its client's descriptor");
    Ok(())
}

/// `fux events` tells what happens, a line each, from `ready` on, in
/// order: a pane made, its bell, its exit, its close.
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
    let pane = first_word(&pane);
    let created = next(r#""event":"created""#)?;
    assert!(created.contains(r#""name":"agent""#), "{created}");
    next(&format!(r#""event":"created","pane":"{pane}""#))?;
    // The bell, then the exit, in the order they happened, one round or
    // two: an exit told first is passed over, and never found after.
    next(&format!(r#""event":"bell","pane":"{pane}""#))?;
    let exited = next(&format!(r#""event":"exited","pane":"{pane}""#))?;
    assert!(exited.ends_with(r#""status":7}"#), "{exited}");
    server.ok(&["str", pane, "pane", "kill"])?;
    next(&format!(r#""event":"closed","pane":"{pane}""#))?;
    let _ = events.kill();
    let _ = events.wait();
    Ok(())
}

/// Keys sent to a tab reach each of its panes, and a wait on them waits in
/// each.
#[test]
fn keys_to_a_tab_reach_each_pane() -> Outcome {
    let server = Server::start("")?;
    let bash = [
        "str", "bash", "program", "str", "--norc", "arg", "str", "-i", "arg",
    ];
    let a = server.ok(&[&["opts", "str", "a", "name"][..], &bash, &["run"]].concat())?;
    let a = first_word(&a);
    let b = server.ok(&[&["str", a, "pane", "horizontal"][..], &bash, &["split"]].concat())?;
    let b = first_word(&b);
    // Each pane takes them, answering its version before them, and writes
    // the command's output.
    let both = server.ok(&[
        "str",
        a,
        "pane",
        "tab",
        "str",
        "echo both-$((40+2))",
        "text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    for (pane, line) in [a, b].into_iter().zip(both.lines()) {
        assert!(line.starts_with(&format!("{pane}:")), "{both}");
        server.until(pane, line, |c| c.contains("\nboth-42"))?;
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

/// A revoke ends every connection through the grant, idle ones too: else
/// one kept open would reach whatever the same path is granted next.
#[test]
fn a_revoke_ends_an_idle_connection_through_it() -> Outcome {
    use fux::protocol::{Frame, Hello, PROTOCOL, Role};
    use std::io::{Read, Write};
    let server = Server::start("")?;
    let granted = server.dir.join("s").join("granted.sock");
    let socket = granted.to_string_lossy().into_owned();
    server.ok(&["str", "main", "str", &socket, "grant"])?;
    let mut stream = std::os::unix::net::UnixStream::connect(&granted).map_err(e)?;
    // Before the revoke: macOS refuses a timeout on a socket its peer closed.
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .map_err(e)?;
    let hello = Hello {
        protocol: PROTOCOL,
        version: "0".into(),
        role: Role::Command,
    };
    stream.write_all(&hello.encode().map_err(e)?).map_err(e)?;
    server.ok(&["str", &socket, "revoke"])?;
    // Told to go, then closed; not left waiting for its next request.
    let mut told = Vec::new();
    stream.read_to_end(&mut told).map_err(e)?;
    assert!(!told.is_empty(), "the connection was closed unsaid");
    Ok(())
}

/// A command client that sends request after request without reading the
/// answers is read no more once they back up, the requests it already sent
/// included, so it holds no more of the server's memory than its socket
/// does; once it reads, every request is answered.
#[test]
fn a_client_that_reads_no_answers_is_read_no_more() -> Outcome {
    use fux::command::{Command as Cmd, Request};
    use fux::protocol::{
        Asked, Context, Decoder, Frame, Greeting, Hello, PROTOCOL, Role, ToCommand,
    };
    use fuxix::poll::{Events, PollFd, poll};
    use std::io::{ErrorKind, Read, Write};
    /// Requests sent at most: a server that reads on takes them all.
    const MOST: usize = 100_000;
    let server = Server::start("")?;
    let mut stream = std::os::unix::net::UnixStream::connect(&server.socket).map_err(e)?;
    let hello = Hello {
        protocol: PROTOCOL,
        version: "0".into(),
        role: Role::Command,
    };
    stream.write_all(&hello.encode().map_err(e)?).map_err(e)?;
    let ls = Asked {
        request: Request::Do(Cmd::Ls {
            target: None,
            get: None,
        }),
        context: Context::default(),
    };
    let ls = ls.encode().map_err(e)?;
    stream.set_nonblocking(true).map_err(e)?;
    let (mut sent, mut at) = (0, 0);
    while sent < MOST {
        match stream.write(ls.get(at..).unwrap_or_default()) {
            Ok(n) => {
                at += n;
                if at == ls.len() {
                    (sent, at) = (sent + 1, 0);
                }
            }
            // Full: stalled for good if no room comes in a second.
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                let mut fds = [PollFd::new(&stream, Events::OUT)];
                let ready = poll(&mut fds, Some(std::time::Duration::from_secs(1))).map_err(e)?;
                if ready == 0 {
                    break;
                }
            }
            Err(error) => return Err(e(error)),
        }
    }
    assert!(
        sent < MOST,
        "the server read every request though no answer was read"
    );
    // Its answers read, the rest of what it sent is taken, and answered.
    stream.set_nonblocking(false).map_err(e)?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .map_err(e)?;
    let mut reader = stream.try_clone().map_err(e)?;
    let answers = std::thread::spawn(move || -> Result<usize, String> {
        let (mut decoder, mut greeted, mut answered) = (Decoder::default(), false, 0);
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let n = reader.read(&mut buffer).map_err(e)?;
            if n == 0 {
                return Ok(answered);
            }
            decoder.push(buffer.get(..n).unwrap_or_default());
            if !greeted {
                match decoder.frame::<Greeting>().map_err(e)? {
                    Some(Greeting::Hello(_)) => greeted = true,
                    Some(_) => return Err("not greeted".into()),
                    None => continue,
                }
            }
            while let Some(frame) = decoder.frame::<ToCommand>().map_err(e)? {
                if let ToCommand::PartsEnd = frame {
                    answered += 1;
                }
            }
            if answered == sent + usize::from(at > 0) {
                return Ok(answered);
            }
        }
    });
    if at > 0 {
        stream
            .write_all(ls.get(at..).unwrap_or_default())
            .map_err(e)?;
    }
    let answered = answers.join().map_err(|_| "the reader panicked")??;
    assert_eq!(
        answered,
        sent + usize::from(at > 0),
        "every request answered"
    );
    Ok(())
}

/// A server killed outright comes back on its socket with the panes it
/// had, their ids, splits and sizes: a program waits for `respawn`, which
/// starts it again; a `run` taking its tab's name is told where that came
/// from. `force-kill-server` forgets it.
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
    let app = first_word(&app);
    server.shows(app, "up")?;
    let shell = server.ok(&["str", app, "pane", "horizontal", "split"])?;
    let shell = first_word(&shell);
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
    // Paused, it takes no keys and no signal: nothing runs there.
    for refused in [
        &["str", "x", "text", "send-keys"][..],
        &["str", "TERM", "signal"],
    ] {
        let refused = server.fux(&[&["str", app, "pane"][..], refused].concat())?;
        let said = refused.stderr.starts_with("fux: not_running: ");
        assert!(said, "{}", refused.stderr);
    }
    // Its name is taken by what came back, which the refusal says.
    let taken = server.fux(&["opts", "str", "app", "name", "str", "cat", "command", "run"])?;
    assert!(
        taken.stderr.starts_with("fux: name_taken: ") && taken.stderr.contains("brought back"),
        "{}",
        taken.stderr
    );
    server.ok(&["str", app, "pane", "respawn"])?;
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

/// A pane made just before a crash comes back: a pane, tab or workspace
/// made or closed is saved at once, not within the second the rest waits.
#[test]
fn a_pane_made_just_before_a_crash_comes_back() -> Outcome {
    let server = Server::start("")?;
    let run = [
        "opts", "str", "late", "name", "str", "sleep 60", "command", "run",
    ];
    let late = server.ok(&run)?;
    let late = first_word(&late);
    let pid = server.pid().ok_or("the server's pid")?;
    let pid = fuxix::process::Pid::from_raw(i32::try_from(pid).map_err(e)?).ok_or("a pid")?;
    fuxix::process::kill(pid, fuxix::process::Signal::Kill).map_err(e)?;
    eventually("the server gone", || Ok(!alive(pid.as_raw())))?;
    let socket = server.socket.to_string_lossy().into_owned();
    let again = Command::new(FUX)
        .args(["str", &socket, "socket", "start"])
        .env("XDG_STATE_HOME", server.dir.join("state"))
        .env("HOME", &server.dir)
        .env("SHELL", "/bin/sh")
        .env_remove("FUX_PANE")
        .env_remove("FUX_SOCKET")
        .output()
        .map_err(e)?;
    assert!(again.status.success());
    eventually("the late pane back", || {
        let ls = server.ok(&["ls"])?;
        Ok(ls.contains(&format!("{late} sleep")))
    })?;
    server.ok(&["force-kill-server"])?;
    Ok(())
}

/// `dump-of` writes a workspace as a program, and running that program
/// makes it again.
#[test]
fn a_dump_makes_its_workspace_again() -> Outcome {
    let server = Server::start("")?;
    let pane = server.ok(&["opts", "str", "build", "name", "new-workspace"])?;
    let worker = server.ok(&[
        "str",
        first_word(&pane),
        "pane",
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
    server.ok(&[
        "str",
        first_word(&worker),
        "pane",
        "str",
        "worker",
        "rename",
    ])?;
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
    eventually("the worker's output", || {
        Ok(server
            .ok(&["str", &made, "pane", "capture"])?
            .contains("built"))
    })?;
    // A workspace `run` made: its dump takes the workspace from the answer
    // `run` leaves.
    server.ok(&[
        "opts", "str", "jobs", "in", "str", "job", "name", "str", "sleep 60", "command", "run",
    ])?;
    let dump = server.ok(&["str", "jobs", "workspace", "dump-of"])?;
    server.ok(&["str", "jobs", "workspace", "kill"])?;
    std::fs::write(&file, &dump).map_err(e)?;
    server.ok(&[&file.to_string_lossy()])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.contains(" jobs\n") && ls.contains(" job\n"),
        "{ls}\nfrom\n{dump}"
    );
    Ok(())
}

/// A dump run on a new server, whose own `main` tab makes the dump's first
/// tab `tab-2`, gives a tab it names `tab-2` that name all the same: a
/// name given takes the one fux gave, which it names anew.
#[test]
fn a_dump_runs_on_a_new_server_whatever_its_tabs_are_named() -> Outcome {
    let source = Server::start("")?;
    source.ok(&[
        "opts", "str", "main", "in", "str", "tab-2", "name", "str", "sleep 60", "command", "run",
    ])?;
    let dump = source.ok(&["dump"])?;
    let server = Server::start("")?;
    let file = server.dir.join("source.fux");
    std::fs::write(&file, &dump).map_err(e)?;
    server.ok(&[&file.to_string_lossy()])?;
    let named = server.ok(&["str", "tab-2", "pane"])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.contains(&format!("{} sleep ", first_word(&named))),
        "{named}\n{ls}\nfrom\n{dump}"
    );
    Ok(())
}

/// A workspace whose first tab is empty, its pane moved to a new tab, is
/// made again from its dump: the first tab written opens it.
#[test]
fn a_dump_opens_its_workspace_with_the_first_tab_written() -> Outcome {
    let server = Server::start("")?;
    let pane = server.ok(&[
        "opts", "str", "moved", "in", "str", "sleep 60", "command", "run",
    ])?;
    server.ok(&[
        "str",
        first_word(&pane),
        "pane",
        "str",
        "new-tab",
        "move-to",
    ])?;
    let dump = server.ok(&["str", "moved", "workspace", "dump-of"])?;
    server.ok(&["str", "moved", "workspace", "kill"])?;
    let file = server.dir.join("moved.fux");
    std::fs::write(&file, &dump).map_err(e)?;
    server.ok(&[&file.to_string_lossy()])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.contains(" moved\n") && ls.contains(" sleep "),
        "{ls}\nfrom\n{dump}"
    );
    Ok(())
}

/// A server opens a door beside its socket (`NAME.door`): a client that
/// cannot use the socket (a sandbox that denies sockets; here, the socket
/// file gone) reaches the server through it, FIFOs of its own relayed to a
/// connection as the socket's.
#[test]
fn a_client_reaches_the_server_through_its_door() -> Outcome {
    let server = Server::start("")?;
    let mut door = server.socket.as_os_str().to_owned();
    door.push(".door");
    let door = std::path::PathBuf::from(door);
    assert!(door.exists(), "a door beside {}", server.socket.display());
    std::fs::remove_file(&server.socket).map_err(|e| e.to_string())?;
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("+1 main"), "{ls}");
    let answer = server.ok(&[
        "opts",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "echo through-the-door; read x",
        "arg",
        "run",
    ])?;
    server.shows(first_word(&answer), "through-the-door")?;
    Ok(())
}

/// A server is found by its door alone, as one that serves through it
/// alone (a sandbox that denies sockets) is: `start` starts no second one,
/// `servers` lists it, and a grant has a door too. A door no server holds
/// open, left by one that stopped, is no server: `status` says so, and the
/// next server takes it.
#[test]
fn a_door_alone_is_a_server() -> Outcome {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::start("")?;
    let rt = private(&server.dir)?;
    let dir = rt.join("fux");
    std::fs::create_dir_all(&dir).map_err(e)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    let made = Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(dir.join("t.sock.door"))
        .status()
        .map_err(e)?;
    assert!(made.success());
    let t = |args: &[&str]| {
        let args: Vec<&str> = ["str", "t", "server"].iter().chain(args).copied().collect();
        named(&rt, &args, &[])
    };
    let left = t(&["status"])?;
    assert_eq!(left.status, 1, "{}", left.stderr);
    assert!(left.stderr.contains("no fux server"), "{}", left.stderr);
    assert_eq!(t(&["start"])?.status, 0);
    std::fs::remove_file(dir.join("t.sock")).map_err(e)?;
    assert_eq!(t(&["start"])?.status, 0);
    let log = std::fs::read_to_string(dir.join("fux.log")).unwrap_or_default();
    assert!(!log.contains("already"), "a second server started: {log}");
    let servers = named(&rt, &["servers"], &[])?;
    assert!(servers.stdout.starts_with("t  "), "{}", servers.stdout);
    let granted = rt.join("g").join("s.sock");
    let socket = granted.to_string_lossy().into_owned();
    assert_eq!(t(&["str", "agent", "str", &socket, "grant"])?.status, 0);
    std::fs::remove_file(&granted).map_err(e)?;
    let through = named(
        &rt,
        &[
            "str", &socket, "socket", "opts", "str", "true", "program", "run",
        ],
        &[],
    )?;
    assert_eq!(through.status, 0, "{}", through.stderr);
    assert!(t(&["ls"])?.stdout.contains("agent"));
    assert_eq!(t(&["force-kill-server"])?.status, 0);
    Ok(())
}

/// Whether `result` is the `EPERM` a sandbox answers with.
#[cfg(target_os = "linux")]
fn denied<T>(what: &str, result: std::io::Result<T>) -> Outcome {
    match result {
        Err(error) if error.raw_os_error() == Some(libc::EPERM) => Ok(()),
        Err(error) => Err(format!("{what}: {error}, not EPERM")),
        Ok(_) => Err(format!("{what} was allowed")),
    }
}

/// The pid of the server that serves `socket`, found by its command line.
#[cfg(target_os = "linux")]
fn serving(socket: &std::path::Path) -> Option<i32> {
    let line = [
        FUX.as_bytes(),
        fux::SERVE.as_bytes(),
        socket.as_os_str().as_encoded_bytes(),
    ]
    .join(&0u8);
    std::fs::read_dir("/proc")
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            let pid = entry.file_name().to_str()?.parse().ok()?;
            (cmdline.strip_suffix(&[0u8]) == Some(line.as_slice()) && alive(pid)).then_some(pid)
        })
}

/// A client in a sandbox that refuses sockets (Codex's: `connect`, `bind`,
/// `send` and `setsockopt` denied) reaches a server outside it through the
/// door: its requests, an action's answer, a capture, `status`, a
/// grant's door, and `kill-server`. A socket with no door beside it, such
/// as an older server's, is a clear error, not a wait.
#[cfg(target_os = "linux")]
#[test]
fn a_sandboxed_client_reaches_its_server_through_the_door() -> Outcome {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use support::sandbox::sandboxed;
    let mut server = Server::start("")?;
    let granted = server.dir.join("g").join("s.sock");
    let grant = granted.to_string_lossy().into_owned();
    server.ok(&["str", "agent", "str", &grant, "grant"])?;
    sandboxed(|| {
        // The sandbox is Codex's: sockets refused, a pair allowed, and on
        // it write(2) and read(2) but not setsockopt(2) or send(2), with
        // which std writes a socket.
        denied("connect", UnixStream::connect(&server.socket))?;
        denied("bind", UnixListener::bind(server.dir.join("bound.sock")))?;
        let (a, mut b) = UnixStream::pair().map_err(e)?;
        denied("setsockopt", a.set_read_timeout(Some(PATIENCE)))?;
        denied("send", (&a).write(b"x"))?;
        std::fs::File::from(std::os::fd::OwnedFd::from(a))
            .write_all(b"x")
            .map_err(e)?;
        let mut byte = [0u8; 1];
        b.read_exact(&mut byte).map_err(e)?;
        assert!(server.ok(&["ls"])?.contains("+1 main"));
        let sh = server.ok(&["opts", "str", "sh", "name", "str", "sh", "program", "run"])?;
        let sh = first_word(&sh);
        let keys = server.ok(&[
            "str",
            sh,
            "pane",
            "str",
            "echo door-$((40+2))",
            "text",
            "str",
            "Enter",
            "key",
            "send-keys",
        ])?;
        let since = keys.split_whitespace().next().unwrap_or_default();
        server.until(sh, since, |c| c.contains("\ndoor-42"))?;
        let capture = server.ok(&["str", sh, "pane", "capture", "json"])?;
        assert!(
            capture.starts_with('{') && capture.contains("door-42"),
            "{capture}"
        );
        let status = server.ok(&["status"])?;
        assert!(status.starts_with("fux "), "{status}");
        let through = server.fux_env(
            &[
                "opts", "str", "inside", "name", "str", "true", "program", "run",
            ],
            &[("FUX_SOCKET", &grant)],
        )?;
        assert_eq!(through.status, 0, "{}", through.stderr);
        let agent = server.ok(&["str", "agent", "workspace", "info", "json"])?;
        assert!(agent.contains(r#""name":"inside""#), "{agent}");
        // Each knock's FIFOs are gone once the server has opened them.
        let dir = server.socket.parent().ok_or("the socket's directory")?;
        for entry in std::fs::read_dir(dir).map_err(e)? {
            let name = entry.map_err(e)?.file_name();
            assert!(!name.to_string_lossy().starts_with("c-"), "{name:?} left");
        }
        server.ok(&["kill-server"])
    })?;
    assert!(server.wait_exit()?.success());
    let mut door = server.socket.clone().into_os_string();
    door.push(".door");
    assert!(!std::path::Path::new(&door).exists(), "the door left");
    // A socket left with no door: refused at once, and said why.
    let _left = UnixListener::bind(&server.socket).map_err(e)?;
    let started = std::time::Instant::now();
    for words in [["ls"], ["status"]] {
        let out = sandboxed(|| server.fux(&words))?;
        assert_eq!(out.status, 1, "{words:?}: {}", out.stdout);
        assert!(
            out.stderr.contains("connecting to") && out.stderr.contains("not permitted"),
            "{words:?}: {}",
            out.stderr
        );
    }
    assert!(started.elapsed() < PATIENCE, "it waited");
    Ok(())
}

/// A server started in the sandbox cannot bind its socket: it serves
/// through its door alone, holding its lock all the same, so no second
/// server starts. Killed outright, it leaves its door, which is then no
/// server: `status` says so, and `start` serves again.
#[cfg(target_os = "linux")]
#[test]
fn a_sandboxed_server_serves_through_its_door_alone() -> Outcome {
    use support::sandbox::sandboxed;
    // A server's, for a private directory of the test's own.
    let server = Server::start("")?;
    let rt = private(&server.dir)?;
    let socket = rt.join("fux").join("t.sock");
    let t = |args: &[&str]| {
        let args: Vec<&str> = ["str", "t", "server"].iter().chain(args).copied().collect();
        named(&rt, &args, &[])
    };
    let checked = sandboxed(|| {
        let started = t(&["start"])?;
        assert_eq!(started.status, 0, "{}", started.stderr);
        assert!(!socket.exists(), "a socket was bound");
        let pid = serving(&socket).ok_or("the server's pid")?;
        assert!(support::sandbox::filtered(pid.unsigned_abs()));
        let second = Command::new(FUX)
            .arg(fux::SERVE)
            .arg(&socket)
            .env("XDG_RUNTIME_DIR", &rt)
            .env("XDG_STATE_HOME", rt.join("state"))
            .output()
            .map_err(e)?;
        let refused = String::from_utf8_lossy(&second.stderr);
        assert!(
            !second.status.success() && refused.contains("already using"),
            "{refused}"
        );
        assert_eq!(t(&["start"])?.status, 0);
        assert_eq!(serving(&socket), Some(pid), "another server started");
        let servers = named(&rt, &["servers"], &[])?;
        assert!(servers.stdout.starts_with("t  "), "{}", servers.stdout);
        let answer = t(&[
            "opts",
            "str",
            "sh",
            "program",
            "str",
            "-c",
            "arg",
            "str",
            "echo door-only; read x",
            "arg",
            "run",
        ])?;
        let pane = answer
            .stdout
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        eventually("its output", || {
            Ok(t(&["str", &pane, "pane", "capture"])?
                .stdout
                .contains("door-only"))
        })?;
        signal(pid, fuxix::process::Signal::Kill);
        eventually("the server gone", || Ok(!alive(pid)))?;
        let status = t(&["status"])?;
        assert!(status.stderr.contains("no fux server"), "{}", status.stderr);
        assert_eq!(t(&["start"])?.status, 0);
        assert!(t(&["ls"])?.stdout.contains("main"));
        let killed = t(&["force-kill-server"])?;
        assert_eq!(killed.status, 0, "{}", killed.stderr);
        eventually("the server gone", || Ok(serving(&socket).is_none()))
    });
    // Started outside the test's care: never left running.
    if let Some(pid) = serving(&socket) {
        signal(pid, fuxix::process::Signal::Kill);
    }
    checked
}

/// A request through the door ends with its server: one killed while a
/// sandboxed client waits for its answer (a program that kills it) fails
/// at once, said so, though the sandbox refuses shutdown(2). The door it
/// left is no server, and the hint starts the server chosen.
#[cfg(target_os = "linux")]
#[test]
fn a_sandboxed_request_ends_with_its_server() -> Outcome {
    use support::sandbox::sandboxed;
    let server = Server::start("")?;
    let rt = private(&server.dir)?;
    let socket = rt.join("fux").join("t.sock");
    let t = |args: &[&str]| {
        let args: Vec<&str> = ["str", "t", "server"].iter().chain(args).copied().collect();
        named(&rt, &args, &[])
    };
    assert_eq!(t(&["start"])?.status, 0);
    let pid = serving(&socket).ok_or("the server's pid")?;
    let crash = "import os, signal, time\ntime.sleep(1)\nos.kill(os.getppid(), signal.SIGKILL)\ntime.sleep(30)\n";
    let checked = sandboxed(|| {
        let mut client = Command::new(FUX)
            .args(["str", "t", "server", "opts", "str", "python3", "program"])
            .args([
                "str", "-c", "arg", "str", crash, "arg", "run", "30", "timeout", "wait",
            ])
            .env("XDG_RUNTIME_DIR", &rt)
            .env("XDG_STATE_HOME", rt.join("state"))
            .env_remove("FUX_SOCKET")
            .env_remove("FUX_PANE")
            .env_remove("FUX_SERVER")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(e)?;
        let deadline = after(PATIENCE);
        let ended = loop {
            if let Some(status) = client.try_wait().map_err(e)? {
                break Some(status);
            }
            if std::time::Instant::now() >= deadline {
                let _ = client.kill();
                let _ = client.wait();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let mut said = String::new();
        if let Some(stderr) = client.stderr.take() {
            std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut said).map_err(e)?;
        }
        let status = ended.ok_or("the client waited on past its server's end")?;
        assert_eq!(status.code(), Some(1), "{said}");
        assert!(said.contains("closed the connection"), "{said}");
        let left = t(&["ls"])?;
        assert!(
            left.stderr.contains("`fux str t server start`"),
            "{}",
            left.stderr
        );
        Ok(())
    });
    if alive(pid) {
        signal(pid, fuxix::process::Signal::Kill);
    }
    checked
}

/// A socket that is the user's, in a directory of the user's, but listened
/// on by another uid is refused: the client checks who serves it. The other
/// uid is a user namespace's (`unshare --map-auto`: the user's own uid as
/// its root, and the user's subordinate ids), whose root binds the socket
/// and listens as uid 1. Where that cannot be had (no `newuidmap` or
/// subordinate ids, or user namespaces restricted, as AppArmor may on
/// Ubuntu), the test says so and passes.
#[cfg(target_os = "linux")]
#[test]
fn a_socket_served_by_another_uid_is_refused() -> Outcome {
    // A server's, for a private directory of the test's own.
    let server = Server::start("")?;
    let socket = private(&server.dir)?.join("other.sock");
    let listen = "import os, socket, sys, time\n\
                  s = socket.socket(socket.AF_UNIX)\n\
                  s.bind(sys.argv[1])\n\
                  os.setgid(1)\n\
                  os.setuid(1)\n\
                  s.listen()\n\
                  print('listening', flush=True)\n\
                  time.sleep(60)\n";
    let spawned = Command::new("unshare")
        .args(["--user", "--map-auto", "--map-root-user", "python3", "-c"])
        .arg(listen)
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut listener) = spawned else {
        eprintln!("skipped: no unshare");
        return Ok(());
    };
    let _reap = Reap(vec![i32::try_from(listener.id()).map_err(e)?]);
    let mut line = String::new();
    let stdout = listener.stdout.take().ok_or("no stdout")?;
    BufReader::new(stdout).read_line(&mut line).map_err(e)?;
    if line.trim() != "listening" {
        let mut why = String::new();
        if let Some(stderr) = listener.stderr.take() {
            std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut why).map_err(e)?;
        }
        eprintln!("skipped: no user namespace with another uid to be had: {why}");
        return Ok(());
    }
    let socket = socket.to_string_lossy();
    let out = server.fux_env(&["ls"], &[("FUX_SOCKET", &socket)])?;
    let euid = fuxix::process::geteuid();
    assert_eq!(out.status, 1);
    assert!(
        out.stderr.contains("its server runs as uid")
            && out.stderr.contains(&format!("not this user's ({euid})")),
        "{}",
        out.stderr
    );
    let _ = listener.kill();
    let _ = listener.wait();
    Ok(())
}

/// The real client attaches from a terminal in the sandbox: no descriptor
/// can go through the door, so the server relays its paints, and keys, a
/// resize, stopping a key's program (the prefix, then Esc) and detaching
/// work as through the socket.
#[cfg(target_os = "linux")]
#[test]
fn a_sandboxed_client_attaches_through_the_door() -> Outcome {
    let server = Server::start("str j str 'me focused 600 timeout wait drop' bind")?;
    let mut terminal = support::sandbox::sandboxed(|| Terminal::attach(&server, 12, 50, &[]))?;
    assert!(support::sandbox::filtered(terminal.child.id()));
    terminal.wait_for("%1 sh")?;
    terminal.type_bytes(b"echo through''-the-door\r")?;
    terminal.wait_for("through-the-door")?;
    terminal.resize(15, 70)?;
    terminal.type_bytes(b"stty size\r")?;
    terminal.wait_for("14 70")?;
    // A wait on a pane that does not change, stopped.
    terminal.type_bytes(b"echo sleep''ing; sleep 100\r")?;
    terminal.wait_for("sleeping")?;
    terminal.type_bytes(b"\x02j")?;
    // The wait under way at the server, not stopped between its steps.
    std::thread::sleep(std::time::Duration::from_millis(500));
    terminal.type_bytes(b"\x02\x1b")?;
    terminal.wait_for("stopped")?;
    terminal.type_bytes(b"\x02d")?;
    let status = terminal.wait_exit()?;
    assert!(
        status.success(),
        "{status}: {:?}",
        String::from_utf8_lossy(&terminal.output)
    );
    Ok(())
}
