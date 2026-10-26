//! What a script or an agent needs of the server itself: naming the one it
//! means, not stopping or closing the wrong thing, hearing what happens as
//! it happens, a place's panes and tabs as handles, sending keys to every
//! pane of a tab, and making a dump's panes again.
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
    let sh = server.ok(&[
        "opts", "80", "24", "size", "str", "sh", "name", "str", "sh", "program", "run",
    ])?;
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
    assert!(exited.ends_with(r#""exited":7}"#), "{exited}");
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
    let a = server.ok(&[
        &["opts", "80", "24", "size", "str", "a", "name"][..],
        &bash,
        &["run"],
    ]
    .concat())?;
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

/// `panes` and `tabs` give a place's members as a list of handles, in
/// layout order, each one a handle the next word takes; a place gone fails
/// as it would for any word.
#[test]
fn panes_and_tabs_are_lists_of_handles() -> Outcome {
    let server = Server::start("")?;
    let run = ["opts", "80", "24", "size", "str", "cat", "program", "run"];
    let a = server.ok(&run)?;
    let a = first_word(&a);
    let b = server.ok(&["str", a, "pane", "str", "cat", "program", "split"])?;
    let c = server.ok(&run)?;
    let (b, c) = (first_word(&b), first_word(&c));
    let agent = ["str", "agent", "workspace"];
    let panes = server.ok(&[&agent[..], &["panes"]].concat())?;
    assert_eq!(panes.trim(), format!("{a} {b} {c}"));
    let tabs = server.ok(&[&agent[..], &["tabs"]].concat())?;
    let Some((t1, t2)) = tabs.trim().split_once(' ') else {
        return Err(format!("two tabs: {tabs}"));
    };
    let each = ["panes", "each", "tab", "print", "end"];
    let of = server.ok(&[&agent[..], &each].concat())?;
    assert_eq!(of, format!("{t1}\n{t1}\n{t2}\n"));
    let each = ["tabs", "each", "panes", "len", "print", "end"];
    assert_eq!(server.ok(&[&agent[..], &each].concat())?, "2\n1\n");
    let gone = server.fux(&["str", b, "pane", "dup", "kill", "panes"])?;
    assert!(gone.stderr.contains("no_pane"), "{}", gone.stderr);
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

/// `dump-of` writes a workspace as a program, and running that program
/// makes it again.
#[test]
fn a_dump_makes_its_workspace_again() -> Outcome {
    let server = Server::start("")?;
    let pane = server.ok(&[
        "opts",
        "80",
        "24",
        "size",
        "str",
        "build",
        "name",
        "new-workspace",
    ])?;
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
    // A script: made runnable, it runs as it is.
    let file = server.dir.join("build.fux");
    std::fs::write(&file, &dump).map_err(e)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    let ran = server.piped(&file, &[], "")?;
    assert_eq!(ran.status, 0, "{}", ran.stderr);
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
        "opts", "80", "24", "size", "str", "jobs", "in", "str", "job", "name", "str", "sleep",
        "program", "str", "60", "arg", "run",
    ])?;
    let dump = server.ok(&["str", "jobs", "workspace", "dump-of"])?;
    server.ok(&["str", "jobs", "workspace", "kill"])?;
    std::fs::write(&file, &dump).map_err(e)?;
    server.ok(&["--file", &file.to_string_lossy()])?;
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
        "opts", "80", "24", "size", "str", "main", "in", "str", "tab-2", "name", "str", "sleep",
        "program", "str", "60", "arg", "run",
    ])?;
    let dump = source.ok(&["dump"])?;
    let server = Server::start("")?;
    let file = server.dir.join("source.fux");
    std::fs::write(&file, &dump).map_err(e)?;
    server.ok(&["--file", &file.to_string_lossy()])?;
    let named = server.ok(&["str", "tab-2", "pane"])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.contains(&format!("{} sleep ", first_word(&named))),
        "{named}\n{ls}\nfrom\n{dump}"
    );
    Ok(())
}

/// A workspace whose first tab closed, its pane moved to a new tab, is
/// made again from its dump: the first tab written opens it.
#[test]
fn a_dump_opens_its_workspace_with_the_first_tab_written() -> Outcome {
    let server = Server::start("")?;
    let pane = server.ok(&[
        "opts", "80", "24", "size", "str", "moved", "in", "str", "sleep", "program", "str", "60",
        "arg", "run",
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
    server.ok(&["--file", &file.to_string_lossy()])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.contains(" moved\n") && ls.contains(" sleep "),
        "{ls}\nfrom\n{dump}"
    );
    Ok(())
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
