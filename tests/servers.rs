//! What a script or an agent needs of the server itself: choosing the one
//! it means, not stopping or closing the wrong thing, hearing what happens as
//! it happens, a place's panes and tabs as handles, sending keys to every
//! pane of a tab, and making a dump's panes again.
// They script `/bin/sh` over a Unix socket (WINDOWS.md, "What PR 8 found").
#![cfg(unix)]
mod support;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use support::*;

/// A command line on the server `NAME.sock` in fux's directory under `rt`,
/// chosen by FUX_SOCKET alone: no FUX_PANE or FUX_SERVER.
fn named(
    rt: &std::path::Path,
    name: &str,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<Output, String> {
    let mut command = Command::new(FUX);
    command
        .args(args)
        .env("XDG_RUNTIME_DIR", rt)
        .env("FUX_SOCKET", rt.join("fux").join(format!("{name}.sock")))
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

/// FUX_SOCKET chooses a server, `start` starts one and returns once it
/// answers, and `status` says what answers. A FUX_PANE
/// of another server's pane is not acted on; `kill-server` from a server's
/// own pane, and a kill with no target, are refused.
#[test]
fn servers_are_chosen_and_kills_are_guarded() -> Outcome {
    let server = Server::start("")?;
    // `status` names the server's process, where the system says it.
    let ours = server.fux(&["(status)"])?;
    let pid = format!("(server-process-id {})", server.pid().ok_or("no pid")?);
    assert_eq!(
        ours.stdout.contains(&pid),
        cfg!(not(target_os = "macos")),
        "{}",
        ours.stdout
    );
    let rt = private(&server.dir)?;
    let started = named(&rt, "t", &["(start)"], &[])?;
    assert_eq!(started.status, 0, "{}", started.stderr);
    let status = named(&rt, "t", &["(status)"], &[])?;
    assert_eq!(status.status, 0);
    assert!(
        status.stdout.contains("(compatible #true)") && status.stdout.contains("/fux/t.sock"),
        "{}",
        status.stdout
    );
    let release = env!("CARGO_PKG_VERSION");
    assert!(status.stdout.contains(&format!("(release \"{release}\")")));
    assert!(
        status
            .stdout
            .contains(&format!("(server-release \"{release}\")"))
    );
    assert!(!status.stdout.contains("\"version\""));
    // With no server, status fails, so a shell's `if fux status` reads it.
    let none = named(&rt, "none", &["(status)"], &[])?;
    assert_eq!(none.status, 1, "{}", none.stderr);
    assert!(
        none.stderr.starts_with("fux: no-server: ") && none.stderr.contains("no fux server"),
        "{}",
        none.stderr
    );
    // The test server's pane, as an agent in it would carry it, reaches
    // the private server: it is not that server's, and nothing is done to
    // what has its number there.
    let pane = server
        .ok(&["(sequence! (define! value-1588 (pane \"%1\")) \"id\" (get value-1588 \"id\"))"])?;
    let identity = format!("{}:0", pane.trim());
    let theirs = [
        ("FUX_PANE", identity.as_str()),
        ("FUX_SERVER", "another-run"),
    ];
    let stray = named(&rt, "t", &["(capture (here))"], &theirs)?;
    assert_eq!(stray.status, 1);
    assert!(
        stray.stderr.contains("another fux server"),
        "{}",
        stray.stderr
    );
    let untargeted = named(&rt, "t", &["(kill (here))"], &[])?;
    assert_eq!(untargeted.status, 1);
    assert!(
        untargeted.stderr.starts_with("fux: no-target: "),
        "{}",
        untargeted.stderr
    );
    // A terminal attached to the server also guards normal CLI shutdown.
    let mut client = server.attach(10, 40)?;
    let refused = server.fux(&["(sequence! (kill-server) #inert)"])?;
    assert_eq!(refused.status, 1);
    assert!(
        refused.stderr.contains("people-attached"),
        "{}",
        refused.stderr
    );
    // From one of its own panes, kill-server is refused as well.
    let sh =
        server.words("(sequence! (define! value-661 (new-tab (program (size (in (options) \"agent\") 80 24) \"sh\"))) (rename value-661 \"sh\") value-661)")?;
    let sh = first_word(&sh);
    let line = format!("'{FUX}' '(kill-server)'; echo status=$?");
    let sent = server.ok(&[&format!(
        "(send-keys (key (text (pane {}) {}) \"Enter\"))",
        fux::script::quoted(sh),
        fux::script::quoted(&line)
    )])?;
    let since = sent.split_whitespace().next().unwrap_or_default();
    server.until(sh, since, |c| {
        c.contains("own-server:") && c.contains("status=1")
    })?;
    assert!(
        server
            .ok(&["(listing (list-session))"])?
            .contains("+1 main"),
        "the server still runs"
    );
    // `kill` on its process stops it anyway.
    signal(
        i32::try_from(server.pid().ok_or("the server's pid")?).map_err(e)?,
        fuxix::process::Signal::Term,
    );
    assert!(client.wait_exit()?.contains("stopped by a signal"));
    assert_eq!(named(&rt, "t", &["(kill-server)"], &[])?.status, 0);
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
        // A shell that leaks a descriptor into its child.
        Command::new("/bin/sh")
            .arg("-c")
            .arg("exec 9>\"$0\" && exec \"$1\" '(start)'")
            .arg(&held)
            .arg(FUX)
            .current_dir(&here)
            .env("XDG_RUNTIME_DIR", &rt)
            .env("FUX_SOCKET", rt.join("fux").join("d.sock"))
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
    signal(pid, fuxix::process::Signal::Term);
    assert_eq!(cwd, std::path::Path::new("/"), "the server's directory");
    assert!(!holds, "the server holds its client's descriptor");
    Ok(())
}

/// The next line of a stream holding a text.
type Next = Box<dyn Fn(&str, Option<&str>) -> Result<String, String>>;

/// A `fux` with `args`, streaming events from `server`, and its next
/// line holding a text, each within `PATIENCE`, the lines before it
/// passed over (`""`: the next line).
fn streaming(server: &Server, args: &[&str]) -> Result<(std::process::Child, Next), String> {
    let mut events = Command::new(FUX)
        .args(args)
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
    let next: Next = Box::new(move |what: &str, pane: Option<&str>| {
        let deadline = after(PATIENCE);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let line = read
                .recv_timeout(left)
                .map_err(|_| format!("no {what} event"))?;
            let kind = read_data(&line, "(field data (quote! event))")?;
            let matches_pane = match pane {
                Some(pane) => {
                    read_data(
                        &line,
                        "(let! ((entry (find-association (quote! pane) data))) (if! (empty-list? entry) \"\" (second entry)))",
                    )? == pane
                }
                None => true,
            };
            if (what.is_empty() || kind == what) && matches_pane {
                return Ok(line);
            }
        }
    });
    Ok((events, next))
}

/// `fux all str CURSOR events` tells what happens after the cursor, a line
/// each, in order: a pane made, its bell, its exit, its close; a stream of
/// the pane's own ends once it closed.
#[test]
fn events_tell_what_happens() -> Outcome {
    let server = Server::start("")?;
    let cursor = server.ok(&["(sequence! (define! value-1607 (next-events (all) \"\" 0)) \"next\" (get value-1607 \"next\"))"])?;
    let (mut events, next) = streaming(
        &server,
        &[&format!(
            "(events (all) {})",
            fux::script::quoted(cursor.trim())
        )],
    )?;
    let told = (|| {
        let pane = server.ok(&["(new-tab (argument (argument (program (size (in (options) \"agent\") 80 24) \"sh\") \"-c\") \"printf '\\\\a'; exit 7\"))"])?;
        let pane = first_word(&pane);
        let created = next("created", None)?;
        assert!(
            read_data(&created, "(field data (quote! name))")? == "agent",
            "{created}"
        );
        next("created", Some(pane))?;
        // The bell, then the exit, in the order they happened, one round or
        // two: an exit told first is passed over, and never found after.
        next("bell", Some(pane))?;
        let exited = next("exited", Some(pane))?;
        assert!(
            read_data(&exited, "(field data (quote! exited))")? == "7",
            "{exited}"
        );
        let killed = [&format!(
            "(sequence! (define! value-169 (pane {})) (kill value-169) (events value-169 {}) #inert)",
            fux::script::quoted(pane),
            fux::script::quoted(cursor.trim())
        )];
        let own = server.fux(&killed)?;
        next("closed", Some(pane))?;
        assert_eq!(own.status, 0, "{}", own.stderr);
        assert!(own.stdout.ends_with("\n") && own.stdout.lines().all(|l| l.contains(pane)));
        assert!(
            own.stdout
                .lines()
                .last()
                .is_some_and(|l| l.contains("(event \"closed\")"))
        );
        Ok::<_, String>(())
    })();
    let _ = events.kill();
    let _ = events.wait();
    told.map(drop)
}

/// A stream from the cursor `ls` gives is told what changed after the
/// listing, though it changed before the stream began; and a cursor past
/// the journal's end is refused.
#[test]
fn a_stream_from_ls_misses_nothing() -> Outcome {
    let server = Server::start("")?;
    let listing = server.ok(&["(list-session)"])?;
    let cursor = read_data(&listing, "(field data (quote! cursor))")?;
    server.words("(new-tab (size (in (options) \"w\") 80 24))")?;
    let (mut events, next) = streaming(
        &server,
        &[&format!("(events (all) {})", fux::script::quoted(&cursor))],
    )?;
    let told = next("", None);
    let _ = events.kill();
    let _ = events.wait();
    let created = told?;
    assert!(
        read_data(&created, "(field data (quote! event))")? == "created",
        "{created}"
    );
    assert!(
        read_data(&created, "(field data (quote! name))")? == "w",
        "{created}"
    );
    let (seq, run) = cursor.split_once(':').ok_or("no run")?;
    let ahead = format!("{}:{run}", seq.parse::<u64>().map_err(e)? + 100);
    let refused = server.fux(&[&format!(
        "(sequence! (next-events (all) {} 0) #inert)",
        fux::script::quoted(&ahead)
    )])?;
    assert_ne!(refused.status, 0);
    assert!(
        refused.stderr.contains("past the journal's end"),
        "{}",
        refused.stderr
    );
    Ok(())
}

/// Keys sent to a tab reach each of its panes, and a wait on them waits in
/// each.
#[test]
fn keys_to_a_tab_reach_each_pane() -> Outcome {
    let server = Server::start("")?;
    let a = server.ok(&[r#"(new-tab (argument (argument (program (name (size (in (options) "agent") 80 24) "a") "bash") "--norc") "-i"))"#])?;
    let a = first_word(&a);
    let b=server.ok(&[&format!("(split (argument (argument (program (axis {} \"horizontal\") \"bash\") \"--norc\") \"-i\"))",support::reference(a))])?;
    let b = first_word(&b);
    // Each pane takes them, answering its version before them, and writes
    // the command's output.
    let both = server.ok(&[&format!(
        "(send-keys (key (text (tab (pane {})) \"echo both-$((40+2))\") \"Enter\"))",
        fux::script::quoted(a)
    )])?;
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
    let run = [r#"(new-tab (program (size (in (options) "agent") 80 24) "cat"))"#];
    let a = server.ok(&run)?;
    let a = first_word(&a);
    let b = server.ok(&[&format!(
        "(split (program (axis {} \"horizontal\") \"cat\"))",
        support::reference(a)
    )])?;
    let c = server.ok(&run)?;
    let (b, c) = (first_word(&b), first_word(&c));
    assert_eq!(
        server.ok(&[r#"(panes (workspace "agent"))"#])?,
        format!("{a}\n{b}\n{c}\n")
    );
    let tabs = server.ok(&[r#"(tabs (workspace "agent"))"#])?;
    let (t1, t2) = tabs.trim().split_once('\n').ok_or("two tabs")?;
    assert_eq!(
        server.ok(&[r#"(map tab (panes (workspace "agent")))"#])?,
        format!("{t1}\n{t1}\n{t2}\n")
    );
    assert_eq!(
        server.ok(&[r#"(map (lambda! (t) (length (panes t))) (tabs (workspace "agent")))"#])?,
        "2\n1\n"
    );
    server.ok(&[&format!(
        "(swap-with {} {}) (reorder (tab {}) \"previous\")",
        support::reference(a),
        support::reference(b),
        fux::script::quoted(t2)
    )])?;
    assert_eq!(
        server.ok(&[r#"(panes (workspace "agent"))"#])?,
        format!("{c}\n{b}\n{a}\n")
    );
    assert_eq!(
        server.ok(&[r#"(tabs (workspace "agent"))"#])?,
        format!("{t2}\n{t1}\n")
    );
    server.ok(&[r#"(new-tab (size (in (options) "keep") 80 24))"#])?;
    for (target, operation, kind) in [
        (support::reference(b), "panes", "no-pane"),
        (
            format!("(tab {})", fux::script::quoted(t2)),
            "tabs",
            "no-tab",
        ),
        (r#"(workspace "agent")"#.into(), "tabs", "no-workspace"),
    ] {
        let out = server.fux(&[&format!(
            "(define! target {target}) (kill target) ({operation} target)"
        )])?;
        assert_eq!(out.status, 1);
        assert!(
            out.stderr.starts_with(&format!("fux: {kind}: ")),
            "{}",
            out.stderr
        );
    }
    Ok(())
}

/// A command client that sends request after request without reading the
/// answers is read no more once they back up, the requests it already sent
/// included, so it holds no more of the server's memory than its socket
/// does; once it reads, every request is answered.
#[test]
fn a_client_that_reads_no_answers_is_read_no_more() -> Outcome {
    use fux_wire::command::{Command as Cmd, Request};
    use fux_wire::protocol::{
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
    stream.write_all(&hello.encode()).map_err(e)?;
    let ls = Asked {
        after: None,
        request: Request::Do(Cmd::Inspect {
            scope: fux_wire::inspect::Scope::All,
            projection: fux_wire::inspect::Projection::Facts,
        }),
        context: Context::default(),
    };
    let ls = ls.encode();
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

/// `dump` writes a workspace as a program, and running that program
/// makes it again.
#[test]
fn a_dump_makes_its_workspace_again() -> Outcome {
    let server = Server::start("")?;
    let pane = server.words("(new-tab (in (size (options) 80 24) \"build\"))")?;
    let worker = server.ok(&[&format!("(split (argument (argument (program (axis (pane {}) \"vertical\") \"sh\") \"-c\") \"echo built; sleep 60\"))", fux::script::quoted(first_word(&pane)))])?;
    server.ok(&[&format!(
        "(sequence! (rename (pane {}) \"worker\") #inert)",
        fux::script::quoted(first_word(&worker))
    )])?;
    let dump = server.ok(&["(dump (workspace \"build\"))"])?;
    server.ok(&["(sequence! (kill (workspace \"build\")) #inert)"])?;
    assert!(!server.ok(&["(listing (list-session))"])?.contains("build"));
    // A script: made runnable, it runs as it is.
    let file = server.dir.join("build.fux");
    std::fs::write(&file, &dump).map_err(e)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    let ran = server.fux(&["--file", &file.to_string_lossy()])?;
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(
        ls.contains(" build\n") && ls.contains(" worker "),
        "{ls}\nfrom\n{dump}"
    );
    let (made, _) = server.newest_pane()?;
    eventually("the worker's output", || {
        Ok(server
            .ok(&[&format!("(capture (pane {}))", fux::script::quoted(&made))])?
            .contains("built"))
    })?;
    // A workspace new-tab made: its dump takes the workspace from the answer
    // new-tab leaves.
    server
        .words("(sequence! (rename (new-tab (argument (program (in (size (options) 80 24) \"jobs\") \"sleep\") \"60\")) \"job\") #inert)")?;
    let dump = server.ok(&["(dump (workspace \"jobs\"))"])?;
    server.ok(&["(sequence! (kill (workspace \"jobs\")) #inert)"])?;
    std::fs::write(&file, &dump).map_err(e)?;
    server.ok(&["--file", &file.to_string_lossy()])?;
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(
        ls.contains(" jobs\n") && ls.contains(" job "),
        "{ls}\nfrom\n{dump}"
    );
    Ok(())
}

/// A dump of a workspace with workspaces in it replays under another
/// (`./proj.fux home`): the same names, nesting and layouts, in the
/// workspace its argument names (D-116 (2)).
#[test]
fn a_nested_dump_replays_under_another_workspace() -> Outcome {
    let server = Server::start("")?;
    let create = |builder: &str| {
        server.words(&format!(
            "(new-tab (argument (program {builder} \"sleep\") \"60\"))"
        ))
    };
    create(r#"(in (size (options) 80 24) "home")"#)?;
    create(r#"(name (in (size (options) 80 24) "proj") "top")"#)?;
    create(r#"(in (in (size (options) 80 24) "proj") "agent1")"#)?;
    let p = create(r#"(name (in (in (size (options) 80 24) "proj") "agent2") "a2")"#)?;
    server.words(&format!(
        "(split (argument (program (shares (axis {} \"vertical\") 2 1) \"sleep\") \"60\"))",
        support::reference(first_word(&p))
    ))?;
    create(r#"(in (in (size (options) 80 24) "agent1") "deep")"#)?;
    create(r#"(name (in (size (options) 80 24) "agent1") "after")"#)?;
    create(r#"(name (in (size (options) 80 24) "proj") "last")"#)?;
    let shape = || {
        server.ok(&[r#"(define! s (snapshot (workspace "proj") "topology"))
      (data-text (list
        (map (lambda! (w) (list (get w "name") (if! (same? (get w "parent") none) "top" (get (get w "parent") "name"))
           (map (lambda! (c) (list (kind c) (get c "name"))) (get w "children")))) (get s "workspaces"))
        (map (lambda! (t) (list (get t "name") (get (get t "workspace") "name")
           (map (lambda! (node) (if! (same? (kind node) "pane") "p" node)) (get t "nodes")))) (get s "tabs"))))"#])
    };
    let before = shape()?;
    assert_eq!(
        read_data(&before, "(map first (first data))")?,
        r#"("proj" "agent1" "deep" "agent2")"#
    );
    assert!(before.contains(r#"("vertical" 2 1 1 2)"#), "{before}");
    let file = server.dir.join("proj.grift");
    std::fs::write(&file, server.ok(&[r#"(dump (workspace "proj"))"#])?).map_err(e)?;
    server.ok(&[r#"(kill (workspace "proj"))"#])?;
    server.ok(&["--file", &file.to_string_lossy(), "home"])?;
    assert_eq!(shape()?, before.replacen(r#""top""#, r#""home""#, 1));
    Ok(())
}

/// A dump replayed beside what it was dumped from, its pane's name taken,
/// is refused as `name_taken` with nothing made, from any directory: one
/// that is gone too.
#[test]
fn a_dump_replayed_beside_itself_makes_nothing_from_a_deleted_directory() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["(sequence! (rename (pane \"%1\") \"runner\") #inert)"])?;
    let file = server.dir.join("runner.fux");
    std::fs::write(&file, server.ok(&["(dump (all))"])?).map_err(e)?;
    let gone = server.dir.join("gone");
    std::fs::create_dir(&gone).map_err(e)?;
    let before = server.ok(&["(list-session)"])?;
    let (gone, file) = (gone.to_str().ok_or("path")?, file.to_str().ok_or("path")?);
    let replay = r#"cd "$1" && rmdir "$1" && exec fux --file "$2""#;
    let ran = server.piped("/bin/sh".as_ref(), &["-c", replay, "sh", gone, file], "")?;
    let taken = ran.stderr.starts_with("fux: name-taken: ") && ran.stderr.contains("runner");
    assert!(taken, "{}", ran.stderr);
    assert_eq!(server.ok(&["(list-session)"])?, before);
    Ok(())
}

/// A given name matching an old automatic label survives dump replay;
/// its unnamed workspace stays separate from a workspace with an old label.
#[test]
fn a_dump_runs_on_a_new_server_whatever_its_tabs_are_named() -> Outcome {
    let source = Server::start("")?;
    source.words(
        "(sequence! (rename (new-tab (argument (program (in (size (options) 80 24) \"main\") \"sleep\") \"60\")) \"tab-2\") #inert)",
    )?;
    source.ok(&["(sequence! (move (pane \"tab-2\") \"new-workspace\") #inert)"])?;
    let dump = source.ok(&["(dump (all))"])?;
    let server = Server::start("")?;
    server.words("(new-tab (in (size (options) 80 24) \"workspace-2\"))")?;
    let file = server.dir.join("source.fux");
    std::fs::write(&file, &dump).map_err(e)?;
    server.ok(&["--file", &file.to_string_lossy()])?;
    let named = server.ok(&["(pane \"tab-2\")"])?;
    assert_eq!(
        server.ok(&["(sequence! (define! value-1780 (workspace (pane \"tab-2\"))) \"name\" (get value-1780 \"name\"))"])?,
        "none\n"
    );
    let held = server.ok(&["(length (panes (workspace \"workspace-2\")))"])?;
    assert_eq!(held, "1\n");
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(
        ls.contains(&format!("{} tab-2 ", first_word(&named))),
        "{named}\n{ls}\nfrom\n{dump}"
    );
    Ok(())
}

/// Replaying explicit programs in an unnamed workspace never starts the
/// replaying client's default shell.
#[test]
fn an_unnamed_workspace_replays_independently_of_the_default_shell() -> Outcome {
    let source = Server::start("")?;
    source.words(r#"(define! p (new-tab (program (size (in (options) "new-workspace") 80 24) "cat"))) (rename p "first") (define! p2 (split (program (axis (pane "first") "horizontal") "cat"))) (rename p2 "second") (rename (new-tab (program (size (in (options) (workspace (pane "first"))) 80 24) "cat")) "third")"#)?;
    let dump = source.ok(&[r#"(dump (workspace (pane "first")))"#])?;
    for shell in [
        r#"(list "/no/such/fux-shell")"#,
        r#"(list "sh" "-c" "exit 0")"#,
    ] {
        let server = Server::start("")?;
        let file = server.dir.join("unnamed.grift");
        std::fs::write(&file, format!("(set \"shell\" {shell})\n{dump}")).map_err(e)?;
        server.ok(&["--file", &file.to_string_lossy()])?;
        assert_eq!(
            server.ok(&[r#"(length (panes (workspace (pane "first"))))"#])?,
            "3\n"
        );
        assert_eq!(
            server.ok(&[r#"(length (tabs (workspace (pane "first"))))"#])?,
            "2\n"
        );
        for name in ["first", "second", "third"] {
            let pid = server.ok(&[&format!(
                "(get (get (pane {}) \"program\") \"process-id\")",
                fux::script::quoted(name)
            )])?;
            assert!(pid.trim().parse::<u32>().is_ok(), "{name}: {pid}");
        }
    }
    Ok(())
}

/// A workspace whose first tab closed, its pane moved to a new tab, is
/// made again from its dump: the first tab written opens it.
#[test]
fn a_dump_opens_its_workspace_with_the_first_tab_written() -> Outcome {
    let server = Server::start("")?;
    let pane = server.words(
        "(new-tab (argument (program (in (size (options) 80 24) \"moved\") \"sleep\") \"60\"))",
    )?;
    server.ok(&[&format!(
        "(sequence! (move (pane {}) \"new-tab\") #inert)",
        fux::script::quoted(first_word(&pane))
    )])?;
    let dump = server.ok(&["(dump (workspace \"moved\"))"])?;
    server.ok(&["(sequence! (kill (workspace \"moved\")) #inert)"])?;
    let file = server.dir.join("moved.fux");
    std::fs::write(&file, &dump).map_err(e)?;
    server.ok(&["--file", &file.to_string_lossy()])?;
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(
        ls.contains(" moved\n") && ls.contains(" sleep "),
        "{ls}\nfrom\n{dump}"
    );
    Ok(())
}

/// The pid of the server that serves `socket`, found by its command line.
#[cfg(target_os = "linux")]
fn serving(socket: &std::path::Path) -> Option<i32> {
    let journal = journal().ok()?;
    let line = [
        FUX.as_bytes(),
        fux::SERVE.as_bytes(),
        socket.as_os_str().as_encoded_bytes(),
        journal.as_bytes(),
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
    let out = server.fux_env(&["(list-session)"], &[("FUX_SOCKET", &socket)])?;
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

/// Argv and cwd are one saved launch, including relative arguments and executables.
#[test]
fn dump_replays_the_saved_launch_after_the_program_changes_directory() -> Outcome {
    use std::os::unix::fs::PermissionsExt;
    for command in [
        r#"(argument (program B "/bin/sh") "worker.sh")"#,
        r#"(program B "./worker.sh")"#,
    ] {
        let server = Server::start("")?;
        let directory = server.dir.join("launch");
        let child = directory.join("child");
        std::fs::create_dir_all(&child).map_err(e)?;
        let script = directory.join("worker.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncd child\nprintf 'saved-launch\n'\nexec /bin/cat\n",
        )
        .map_err(e)?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
        let cwd = fux::script::quoted(directory.to_str().ok_or("cwd text")?);
        let builder = format!("(directory (size (in (options) \"agent\") 80 24) {cwd})");
        let answer = server.ok(&[&format!("(new-tab {})", command.replace('B', &builder))])?;
        let pane = answer.trim().split(':').next().ok_or("pane")?;
        // It prints once it changed directory.
        eventually("the program changed directory", || {
            Ok(server
                .ok(&[&format!("(capture {})", support::reference(pane))])?
                .contains("saved-launch"))
        })?;
        let dump = server.ok(&[&format!("(dump (tab {}))", support::reference(pane))])?;
        assert!(dump.contains(&cwd), "{dump}");
        server.ok(&[&dump])?;
        let (replayed, _) = server.newest_pane()?;
        assert_ne!(replayed, pane);
        eventually("the saved launch replayed", || {
            Ok(server
                .ok(&[&format!("(capture {})", support::reference(&replayed))])?
                .contains("saved-launch"))
        })?;
    }
    Ok(())
}

/// Shell replay starts in the observed directory, retaining its saved argv.
#[test]
fn dump_replays_a_shell_in_its_current_directory() -> Outcome {
    let server = Server::start("")?;
    let directory = server.dir.join("launch");
    let child = directory.join("child");
    std::fs::create_dir_all(&child).map_err(e)?;
    let cwd = fux::script::quoted(directory.to_str().ok_or("cwd text")?);
    let observed = fux::script::quoted(child.to_str().ok_or("child text")?);
    let body = fux::script::quoted("cd \"$1\" && printf 'shell-directory\\n' && exec /bin/cat");
    let answer = server.ok(&[&format!("(new-tab (shell-program (directory (size (in (options) \"agent\") 80 24) {}) (list \"/bin/sh\" \"-c\" {body} \"shell\" {observed})))", cwd)])?;
    let pane = answer.trim().split(':').next().ok_or("pane")?;
    let in_child = fux::script::quoted(child.to_str().ok_or("child text")?);
    eventually("the shell changed directory", || {
        Ok(server
            .ok(&[&format!("(capture {})", support::reference(pane))])?
            .contains("shell-directory"))
    })?;
    let dump = server.ok(&[&format!("(dump (tab {}))", support::reference(pane))])?;
    assert!(dump.contains(&in_child), "{dump}");
    server.ok(&[&dump])?;
    let (replayed, _) = server.newest_pane()?;
    assert_ne!(replayed, pane);
    eventually("the shell replayed in its current directory", || {
        Ok(server
            .ok(&[&format!("(capture {})", support::reference(&replayed))])?
            .contains("shell-directory")
            && server
                .ok(&[&format!("(dump (tab {}))", support::reference(&replayed))])?
                .contains(&in_child))
    })?;
    Ok(())
}

#[test]
fn addressing_a_client_does_not_claim_its_origin() -> Outcome {
    let server = Server::start("")?;
    let mut view = server.attach(12, 80)?;
    view.ready()?;
    let client = view.id.as_deref().ok_or("client")?;
    server.ok(&["(sequence! (split (axis (pane 1) \"horizontal\")) #inert)"])?;
    let cursor = server.ok(&["(sequence! (define! value-1851 (next-events (all) \"\" 0)) \"next\" (get value-1851 \"next\"))"])?;
    server.ok(&[&format!(
        "(sequence! (show (client {}) (pane 1)) #inert)",
        fux::script::quoted(client)
    )])?;
    let origins = server.ok(&[&format!("(define! batch (next-events (all) {} 0)) (map (lambda! (e) (get (get e \"origin\") \"kind\")) (filter (lambda! (e) (same? (get e \"event\") \"shown\")) (get batch \"events\")))", fux::script::quoted(cursor.trim()))])?;
    assert!(!origins.trim().is_empty());
    assert!(origins.lines().all(|line| line == "outside"), "{origins}");
    Ok(())
}
