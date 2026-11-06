//! Each pane in a process of its own, and each client connection (D-120,
//! D-123), and a run that outlives a crashed server (D-124): what one
//! process's end does to the rest.
// They script `/bin/sh` over a Unix socket (WINDOWS.md, "What PR 8 found").
#![cfg(unix)]
mod support;
use fuxix::process::Signal;
use std::path::PathBuf;
use support::*;

/// A new pane, through one grift program with each executable argument quoted.
fn program(builder: &str, argv: &[&str]) -> String {
    let Some((first, rest)) = argv.split_first() else {
        return builder.to_owned();
    };
    let mut expression = format!("(program {builder} {})", fux::script::quoted(first));
    for argument in rest {
        expression = format!("(argument {expression} {})", fux::script::quoted(argument));
    }
    expression
}
fn pane(server: &Server, ws: &str, argv: &[&str]) -> Result<String, String> {
    let builder = format!("(size (in (options) {}) 80 24)", fux::script::quoted(ws));
    Ok(id(
        &server.ok(&[format!("(new-tab {})", program(&builder, argv))])?
    ))
}
/// The pane an action's answer names.
fn id(answer: &str) -> String {
    let first = answer.split_whitespace().next().unwrap_or_default();
    first.split(':').next().unwrap_or_default().to_owned()
}

/// The journal's end now: a cursor.
fn cursor(server: &Server) -> Result<String, String> {
    let next = ["(get (next-events (all) \"\" 0) \"next\")"];
    Ok(server.ok(&next)?.trim().to_owned())
}

/// The run a server serves, as its pane's version ends.
fn run(server: &Server, pane: &str) -> Result<String, String> {
    let version = server.version(pane)?;
    Ok(version.rsplit(':').next().unwrap_or_default().to_owned())
}

/// Every process's pid, its parent's and its words, as `ps` says.
fn processes() -> Vec<(i32, i32, String)> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,args="])
        .output()
    else {
        return Vec::new();
    };
    (String::from_utf8_lossy(&out.stdout).lines())
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let parent = words.next()?.parse().ok()?;
            Some((pid, parent, words.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}

/// A process's parent, as `ps` says.
fn parent(pid: i32) -> Result<i32, String> {
    (processes().into_iter())
        .find(|(p, ..)| *p == pid)
        .map(|(_, parent, _)| parent)
        .ok_or_else(|| format!("{pid}'s parent"))
}

/// The pid of `pane`'s program.
fn program_pid(server: &Server, pane: &str) -> Result<i32, String> {
    let pid = server.ok(&[&format!("(sequence! (define! value-1242 (pane {})) \"program\" (define! value-1244 (get value-1242 \"program\")) \"pid\" (get value-1244 \"process-id\"))", fux::script::quoted(pane))])?;
    pid.trim()
        .parse()
        .map_err(|_| format!("{pane}'s program: {pid}"))
}

/// The pid of `pane`'s own process: its program's parent.
fn own_process(server: &Server, pane: &str) -> Result<i32, String> {
    parent(program_pid(server, pane)?)
}

/// The processes whose parent is `pid` and whose words hold `word`.
fn children(pid: i32, word: &str) -> Vec<i32> {
    (processes().into_iter())
        .filter(|(_, parent, args)| *parent == pid && args.split(' ').any(|w| w == word))
        .map(|(child, ..)| child)
        .collect()
}

/// The processes of the server `pid`'s connections that have a client:
/// those it took itself (`__conn taken`), and those that took one at its
/// socket, whose stdout then points elsewhere; not the one waiting for the
/// next, which holds the socket on its stdout (`Server::spare`).
#[cfg(target_os = "linux")]
fn connections(pid: i32) -> Vec<i32> {
    let taken = children(pid, "taken");
    (children(pid, "__conn").into_iter())
        .filter(|conn| {
            taken.contains(conn)
                || std::fs::read_link(format!("/proc/{conn}/fd/1"))
                    .is_ok_and(|to| !to.to_string_lossy().starts_with("socket:"))
        })
        .collect()
}

/// The server's process.
fn server_pid(server: &Server) -> Result<i32, String> {
    i32::try_from(server.pid().ok_or("the server's pid")?).map_err(e)
}

/// The most memory a process held at once, in KiB (Linux's `VmHWM`).
#[cfg(target_os = "linux")]
fn peak_kib(pid: i32) -> Result<u64, String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).map_err(e)?;
    (status.lines())
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .ok_or_else(|| "its peak memory".to_owned())
}

/// The journal's entries from `cursor` on, as JSON lines.
fn told(server: &Server, cursor: &str) -> Result<String, String> {
    server.ok(&[&format!(
        "(next-events (all) {} 0)",
        fux::script::quoted(cursor)
    )])
}

/// Where the run on `server`'s socket keeps what outlives its server.
fn kept(server: &Server) -> PathBuf {
    fux_server::socket::panes(&server.socket)
}

/// Caps `pid`'s address space at `more` bytes past what it has now
/// (`prlimit`, util-linux): an allocation past it fails.
#[cfg(target_os = "linux")]
fn cap(pid: i32, more: u64) -> Outcome {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).map_err(e)?;
    let kib: u64 = (status.lines())
        .find_map(|line| line.strip_prefix("VmSize:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .ok_or("its size")?;
    let limit = kib.saturating_mul(1024).saturating_add(more).to_string();
    let out = std::process::Command::new("prlimit")
        .args(["--pid", &pid.to_string(), &format!("--as={limit}:{limit}")])
        .output()
        .map_err(e)?;
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr).into_owned()),
    }
}

/// 1. A pane's own process killed: the server and another pane go on,
///    taking input and captures, and the journal says how it ended, then
///    its program's end, its real status.
#[cfg(target_os = "linux")]
#[test]
fn a_pane_process_killed_ends_its_pane_alone() -> Outcome {
    let server = Server::start("")?;
    let sleeper = pane(&server, "a", &["sleep", "1000"])?;
    let shell = pane(&server, "b", &["sh"])?;
    let cursor = cursor(&server)?;
    let own = own_process(&server, &sleeper)?;
    signal(own, Signal::Kill);
    eventually("its read_failed and exited", || {
        let lines = told(&server, &cursor)?;
        Ok(lines.contains(r#"(event "read-failed")"#) && lines.contains(r#"(event "exited")"#))
    })?;
    let lines = told(&server, &cursor)?;
    let failed = lines.find(r#"(event "read-failed")"#).unwrap_or(usize::MAX);
    let exited = lines.find(r#"(event "exited")"#).unwrap_or(0);
    assert!(failed < exited, "{lines}");
    assert!(
        lines.contains("its pane's own process was killed by signal 9"),
        "{lines}"
    );
    // Its program hung up as its terminal closed: its real status.
    assert!(lines.contains("(signal 1)"), "{lines}");
    assert!(
        server
            .pid()
            .is_some_and(|p| alive(i32::try_from(p).unwrap_or(0)))
    );
    server.type_line(&shell, "echo still-here")?;
    server.shows(&shell, "still-here")?;
    // A program's pane stays, its program ended.
    let state = server.ok(&[&format!("(sequence! (define! value-1253 (pane {})) \"program\" (define! value-1255 (get value-1253 \"program\")) \"state\" (get value-1255 \"state\"))", fux::script::quoted(&sleeper))])?;
    assert_eq!(state.trim(), "ended");
    Ok(())
}

/// 2. A pane flooding what it answers its program, under a cap of its
///    own process's memory: that process ends alone.
#[cfg(target_os = "linux")]
#[test]
fn a_flood_under_a_cap_ends_its_pane_process_alone() -> Outcome {
    let server = Server::start("")?;
    let flood = pane(&server, "a", &["sleep", "1000"])?;
    let shell = pane(&server, "b", &["sh"])?;
    let own = own_process(&server, &flood)?;
    cap(own, 64 << 20)?;
    // It asks its terminal what it is, and never reads the answers.
    let asks = r#"q=$(printf '\033[c%.0s' $(seq 200)); yes "$q""#;
    let respawn = vec![format!(
        "(respawn (kill-first {}))",
        program(
            &format!("(pane {})", fux::script::quoted(&flood)),
            &["sh", "-c", asks]
        )
    )];
    server.ok(&respawn)?;
    eventually("its process to end", || Ok(!alive(own)))?;
    assert!(
        server
            .pid()
            .is_some_and(|p| alive(i32::try_from(p).unwrap_or(0)))
    );
    server.type_line(&shell, "echo still-here")?;
    server.shows(&shell, "still-here")?;
    Ok(())
}

/// 3. A client that stops reading while a pane floods it with copies: only
///    its connection ends, its process capped; the server and others go
///    on.
#[cfg(target_os = "linux")]
#[test]
fn a_client_that_stops_reading_ends_only_its_connection() -> Outcome {
    let server = Server::start("")?;
    let pid = i32::try_from(server.pid().ok_or("the server's pid")?).map_err(e)?;
    let mut stuck = server.attach(10, 40)?;
    stuck.wait_for("$")?;
    let conns = connections(pid);
    let [conn] = conns.as_slice() else {
        return Err(format!("its connection's process: {conns:?}"));
    };
    let conn = *conn;
    // What it holds for its client grows by doubling, so it ends once
    // about half its cap is held. The copies come no faster than its
    // pane's process reads them, a few MiB a second in a debug build, less
    // on CI: a cap of 16 MiB, 8 of them to hold, missed `PATIENCE` there.
    cap(conn, 4 << 20)?;
    let shell = support::focused(&server)?;
    // It no longer reads (`pump`) from here on.
    let copies = r#"while :; do printf '\033]52;c;aGk=\a'; done"#;
    server.type_line(&shell, copies)?;
    eventually("its connection's process to end", || Ok(!alive(conn)))?;
    eventually("its client gone", || {
        Ok(!server
            .ok(&["(listing (list-session))"])?
            .contains("client "))
    })?;
    assert!(alive(pid), "the server");
    let mut other = server.attach(10, 40)?;
    other.wait_for("$")?;
    other.detach()?;
    drop(stuck);
    Ok(())
}

/// 4. Keys at a version their pane is no longer at are refused by the pane
///    and queued nowhere.
#[test]
fn a_stale_expect_is_refused_by_its_pane_and_queues_nothing() -> Outcome {
    let server = Server::start("")?;
    let shell = pane(&server, "a", &["sh"])?;
    server.shows(&shell, "$")?;
    let seen = server.version(&shell)?;
    server.type_line(&shell, "echo moved")?;
    server.shows(&shell, "moved")?;
    let cursor = cursor(&server)?;
    let stale = server.fux(&[&format!(
        "(send-keys (text (expect {}) \"NOT-SENT\"))",
        fux::script::quoted(&seen)
    )])?;
    assert!(stale.stderr.contains("stale: "), "{}", stale.stderr);
    let lines = told(&server, &cursor)?;
    assert!(!lines.contains(r#"(event "input")"#), "{lines}");
    let capture = server.ok(&[&format!("(capture (pane {}))", fux::script::quoted(&shell))])?;
    assert!(!capture.contains("NOT-SENT"), "{capture}");
    Ok(())
}

/// 5. Keys for a tab go to each pane that takes them: one that refuses
///    fails the whole, saying which took them (D-120 (4)).
#[test]
fn keys_for_a_tab_go_to_each_pane_that_takes_them() -> Outcome {
    let server = Server::start("")?;
    let shell = pane(&server, "a", &["sh"])?;
    server.shows(&shell, "$")?;
    let ended = id(&server.ok(&[format!(
        "(split {})",
        program(
            &format!(
                "(axis (pane {}) \"horizontal\")",
                fux::script::quoted(&shell)
            ),
            &["sh", "-c", "exit 3"]
        )
    )])?);
    assert_eq!(server.exits(&ended)?, "3");
    let tab = server.ok(&[&format!(
        "(sequence! (define! value-1273 (pane {})) \"tab\" (get value-1273 \"tab\"))",
        fux::script::quoted(&shell)
    )])?;
    let sent = server.fux(&[&format!(
        "(send-keys (text (tab {}) \"TOOK\"))",
        fux::script::quoted(tab.trim())
    )])?;
    assert_ne!(sent.status, 0);
    assert!(sent.stderr.contains("not-running: "), "{}", sent.stderr);
    assert!(
        sent.stderr.contains(&format!("already sent to {shell}")),
        "{}",
        sent.stderr
    );
    server.shows(&shell, "TOOK")?;
    Ok(())
}

/// 6. The server killed: its panes' programs go on; `start` takes the run
///    back, with its ids, layout, names and screens; a cursor from before is
///    told what it lost; the attached client had said why it ended. On
///    macOS, which names no socket's peer, too (D-124).
#[test]
fn a_crashed_servers_run_is_taken_back() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let shell = pane(server, "work", &["sh"])?;
    server.ok(&[&format!(
        "(sequence! (rename (pane {}) \"editor\") #inert)",
        fux::script::quoted(&shell)
    )])?;
    let sleeper = id(&server.ok(&[format!(
        "(split {})",
        program(
            &format!("(axis (pane {}) \"vertical\")", fux::script::quoted(&shell)),
            &["sleep", "1000"]
        )
    )])?);
    server.type_line(&shell, "echo MARK-BEFORE")?;
    server.shows(&shell, "MARK-BEFORE")?;
    let was = run(server, &shell)?;
    let cursor = cursor(server)?;
    let programs = [program_pid(server, &shell)?, program_pid(server, &sleeper)?];
    let mut attached = Terminal::attach(server, 24, 80, &[])?;
    attached.wait_for("$")?;
    let before = layout(server)?;
    let pid = i32::try_from(server.pid().ok_or("the server's pid")?).map_err(e)?;
    signal(pid, Signal::Kill);
    attached.wait_exit()?;
    let said = String::from_utf8_lossy(&attached.output).into_owned();
    assert!(said.contains("ended without a word"), "{said}");
    for program in programs {
        assert!(alive(program), "{program} ended with the server");
    }
    server.ok(&["(sequence! (start) #inert)"])?;
    assert_eq!(layout(server)?, before);
    assert_eq!(run(server, &shell)?, was);
    assert!(
        server
            .ok(&[&format!("(capture (pane {}))", fux::script::quoted(&shell))])?
            .contains("MARK-BEFORE")
    );
    let lost = server.fux(&[&format!(
        "(next-events (all) {} 0)",
        fux::script::quoted(&cursor)
    )])?;
    assert!(
        lost.stderr.contains("is not this server's journal"),
        "{}",
        lost.stderr
    );
    assert!(
        lost.stderr.contains("what came between is lost"),
        "{}",
        lost.stderr
    );
    // Its programs' FUX_PANE and FUX_SERVER still name it.
    let here = server.dir.join("here");
    let line = format!(
        r#""$FUX" '(get (list-session) "run")' > '{}'"#,
        here.display()
    );
    server.type_line(&shell, &line)?;
    eventually("its shell's answer", || {
        Ok(std::fs::read_to_string(&here).is_ok_and(|said| said.trim() == was))
    })
}

/// Stops a server however its test ends: one that took a run back is no
/// child of the test's.
/// 6b. A crashed server's run is taken back though the connection's
///     process it started for its next client, which holds its socket till
///     it sees its server go, is stopped meanwhile: a socket whose server
///     is gone is no server's (`socket::owned`), with room in its backlog
///     or none.
#[cfg(target_os = "linux")]
#[test]
fn a_crashed_servers_run_is_taken_back_while_its_waiting_connection_is_stopped() -> Outcome {
    taken_back_with_its_spare_stopped(false)
}

#[cfg(target_os = "linux")]
#[test]
fn a_crashed_servers_run_is_taken_back_with_its_stopped_connections_backlog_full() -> Outcome {
    taken_back_with_its_spare_stopped(true)
}

#[cfg(target_os = "linux")]
fn taken_back_with_its_spare_stopped(full: bool) -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let shell = pane(server, "work", &["sh"])?;
    server.type_line(&shell, "echo MARK-BEFORE")?;
    server.shows(&shell, "MARK-BEFORE")?;
    let before = layout(server)?;
    let pid = server_pid(server)?;
    let waiting: Vec<i32> = (children(pid, "__conn").into_iter())
        .filter(|conn| !connections(pid).contains(conn))
        .collect();
    let [spare] = waiting[..] else {
        return Err(format!("the connection waiting for a client: {waiting:?}"));
    };
    signal(spare, Signal::Stop);
    signal(pid, Signal::Kill);
    // Clients come while no server serves, till the socket takes no more.
    let mut queued = Vec::new();
    while full && queued.len() < 100_000 {
        let fd = fuxix::socket::stream().map_err(e)?;
        fuxix::io::set_nonblocking(&fd, true).map_err(e)?;
        match fuxix::socket::connect(&fd, &server.socket) {
            Ok(()) => queued.push(fd),
            Err(fuxix::Errno::AGAIN) => break,
            Err(error) => return Err(format!("queueing a client: {error}")),
        }
    }
    // Asked with a bound: a client of the old socket would wait for good.
    let taken = server.ok(&["(sequence! (start) #inert)"]).and_then(|_| {
        let status = within(server, 30, &["status"])?;
        match status.status {
            0 => layout(server),
            _ => Err(format!("no server took the run back: {}", status.stderr)),
        }
    });
    let still = processes().into_iter().any(|(p, ..)| p == spare)
        && std::fs::read_to_string(format!("/proc/{spare}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, r)| r.starts_with('T'))
        });
    signal(spare, Signal::Cont);
    drop(queued);
    assert_eq!(taken?, before);
    assert!(still, "the spare ran meanwhile");
    assert!(
        server
            .ok(&[&format!("(capture (pane {}))", fux::script::quoted(&shell))])?
            .contains("MARK-BEFORE")
    );
    eventually("the old connection's process gone", || Ok(!alive(spare)))
}

struct Stop<'a>(&'a Server);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        let _ = self.0.fux(&["(sequence! (kill-server) #inert)"]);
    }
}

/// What `ls listing` says of a server's workspaces, tabs and panes.
fn layout(server: &Server) -> Result<String, String> {
    let listing = server.ok(&["(listing (list-session))"])?;
    let kept = (listing.lines()).filter(|line| {
        !["client ", "cursor "]
            .iter()
            .any(|w| line.trim_start().starts_with(w))
    });
    Ok(kept.collect::<Vec<_>>().join("\n"))
}

/// 7. `kill-server` and SIGTERM end the run: no pane's process is left,
///    nor its record.
#[test]
fn stopping_ends_the_run_and_its_pane_processes() -> Outcome {
    for stop in ["kill-server", "SIGTERM"] {
        let server = Server::start("")?;
        let sleeper = pane(&server, "a", &["sleep", "1000"])?;
        let own = own_process(&server, &sleeper)?;
        let program = program_pid(&server, &sleeper)?;
        match stop {
            "kill-server" => drop(server.ok(&["(sequence! (kill-server) #inert)"])?),
            _ => signal(
                i32::try_from(server.pid().ok_or("pid")?).map_err(e)?,
                Signal::Term,
            ),
        }
        eventually("its pane's process and program gone", || {
            Ok(!alive(own) && !alive(program))
        })?;
        eventually("its record gone", || Ok(!kept(&server).exists()))?;
    }
    Ok(())
}

/// 8. A crashed server's run whose record, or whose fux, is gone: `start`
///    refuses, naming the panes that wait.
#[test]
fn a_run_that_cannot_be_taken_back_is_refused_naming_its_panes() -> Outcome {
    for gone in ["run", "exe"] {
        let server = Server::start("")?;
        let sleeper = pane(&server, "a", &["sleep", "1000"])?;
        let own = own_process(&server, &sleeper)?;
        // Each pane's process and program waits after the crash, till
        // killed.
        let mut reap = Reap::default();
        for (_, program) in server.panes()? {
            reap.0.extend([program, parent(program)?]);
        }
        let pid = i32::try_from(server.pid().ok_or("the server's pid")?).map_err(e)?;
        signal(pid, Signal::Kill);
        eventually("the server gone", || Ok(!alive(pid)))?;
        std::fs::remove_file(kept(&server).join(gone)).map_err(e)?;
        let refused = server.fux(&["(sequence! (start) #inert)"])?;
        assert_ne!(refused.status, 0, "{gone}");
        let waits = format!("{sleeper} (pid {own})");
        let said = &refused.stderr;
        assert!(
            said.contains("its panes wait: ") && said.contains(&waits),
            "{gone}: {said}"
        );
        assert!(alive(own), "{gone}: the pane's process was ended");
    }
    Ok(())
}

/// `fux ARGS…` against `server`, started: its stdin is a pipe.
#[cfg(target_os = "linux")]
fn started(server: &Server, args: &[&str]) -> Result<std::process::Child, String> {
    std::process::Command::new(fux())
        .args(args)
        .env("FUX_SOCKET", &server.socket)
        .env("XDG_CONFIG_HOME", server.dir.join("config"))
        .env_remove("FUX_PANE")
        .env_remove("FUX_SERVER")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(e)
}

/// `fux ARGS…` against `server`, given `seconds` at most: its status, 124
/// if it took longer (killed then), and what it said.
fn within(server: &Server, seconds: u64, args: &[&str]) -> Result<Output, String> {
    let mut child = std::process::Command::new(fux())
        .args(args)
        .env("FUX_SOCKET", &server.socket)
        .env("XDG_CONFIG_HOME", server.dir.join("config"))
        .env_remove("FUX_PANE")
        .env_remove("FUX_SERVER")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(e)?;
    let deadline = after(std::time::Duration::from_secs(seconds));
    let status = loop {
        if let Some(status) = child.try_wait().map_err(e)? {
            break status.code().unwrap_or(-1);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break 124;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let out = child.wait_with_output().map_err(e)?;
    Ok(Output {
        status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Every process of a crashed server's run, killed as its test ends: the
/// panes' processes and programs, which wait for no one now.
fn reap_run(server: &Server) -> Result<Reap, String> {
    let mut reap = Reap::default();
    for (_, program) in server.panes()? {
        reap.0.extend([program, parent(program)?]);
    }
    Ok(reap)
}

/// 9. A paste's body goes from its client's connection to its pane, never
///    through the server (D-123): sent to a pane whose process reads
///    nothing (stopped), it grows no memory of the server's.
#[cfg(target_os = "linux")]
#[test]
fn a_paste_body_never_passes_through_the_server() -> Outcome {
    use std::io::Write;
    let server = Server::start("")?;
    let sleeper = pane(&server, "a", &["sleep", "1000"])?;
    let own = own_process(&server, &sleeper)?;
    let pid = server_pid(&server)?;
    let before = peak_kib(pid)?;
    signal(own, Signal::Stop);
    let mut sending = started(
        &server,
        &[&format!(
            "(send-keys (paste {} (input)))",
            support::reference(&sleeper)
        )],
    )?;
    let body = vec![b'x'; 32 << 20];
    let mut stdin = sending.stdin.take().ok_or("its stdin")?;
    stdin.write_all(&body).map_err(e)?;
    drop(stdin);
    eventually("its connection to hold it", || {
        Ok((connections(pid).into_iter()).any(|conn| peak_kib(conn).is_ok_and(|k| k > 32 << 10)))
    })?;
    let grew = peak_kib(pid)?.saturating_sub(before);
    signal(own, Signal::Cont);
    let _ = sending.wait();
    assert!(grew < 8 << 10, "the server grew {grew} KiB");
    Ok(())
}

/// 10. A capture goes from its pane to the asking connection, never
///     through the server (D-123): its whole history grows no memory of the
///     server's.
#[cfg(target_os = "linux")]
#[test]
fn a_capture_never_passes_through_the_server() -> Outcome {
    let server = Server::start("")?;
    let words = vec![format!(
        "(new-tab {})",
        program(
            "(history-bytes (size (in (options) \"a\") 80 24) 67108864)",
            &["sh", "-c", "seq 600000; echo all-out; sleep 1000"]
        )
    )];
    let lines = id(&server.ok(&words)?);
    // Its screen shows the end once all is read: waited for by its version,
    // as long as a slow machine takes to read 600,000 lines, with no whole
    // history captured on the way.
    let deadline = after(std::time::Duration::from_secs(120));
    let mut since = server.version(&lines)?;
    while !server
        .ok(&[&format!("(capture (pane {}))", fux::script::quoted(&lines))])?
        .contains("all-out")
    {
        since = server.changed(&lines, &since, deadline)?;
    }
    let pid = server_pid(&server)?;
    let before = peak_kib(pid)?;
    let captured = server.ok(&[&format!(
        "(capture (all-history (pane {})))",
        fux::script::quoted(&lines)
    )])?;
    assert!(captured.len() > 3 << 20, "{} bytes", captured.len());
    let grew = peak_kib(pid)?.saturating_sub(before);
    assert!(grew < 2 << 10, "the server grew {grew} KiB");
    Ok(())
}

/// 11. A client's screen is composed by its connection, never by the
///     server (D-123): one that watches at a size far past its tab's grows
///     no memory of the server's.
#[cfg(target_os = "linux")]
#[test]
fn a_clients_screen_is_never_composed_in_the_server() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(24, 80)?;
    client.ready()?;
    let pid = server_pid(&server)?;
    let before = peak_kib(pid)?;
    let mut watcher = server.watch(1500, 1500)?;
    eventually("its connection to compose it", || {
        watcher.pump()?;
        Ok((connections(pid).into_iter()).any(|conn| peak_kib(conn).is_ok_and(|k| k > 24 << 10)))
    })?;
    let grew = peak_kib(pid)?.saturating_sub(before);
    assert!(grew < 8 << 10, "the server grew {grew} KiB");
    Ok(())
}

/// 12. What a client types at a pane whose process reads nothing (stopped)
///     waits with the client's connection: the server grows no queue for
///     the pane.
#[cfg(target_os = "linux")]
#[test]
fn typing_at_a_stopped_pane_grows_no_queue_in_the_server() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(24, 80)?;
    client.wait_for("$")?;
    let own = own_process(&server, "%1")?;
    let pid = server_pid(&server)?;
    signal(own, Signal::Stop);
    let before = peak_kib(pid)?;
    for _ in 0..40 {
        client.send(&[b'k'; 10_000])?;
    }
    std::thread::sleep(std::time::Duration::from_secs(1));
    let grew = peak_kib(pid)?.saturating_sub(before);
    signal(own, Signal::Cont);
    assert!(grew < 2 << 10, "the server grew {grew} KiB");
    Ok(())
}

/// 13. A client's connection ends with its server: one whose client
///     stopped reading, with what it holds for it unsent, is gone at once
///     once its server is (D-124 (6)).
#[cfg(target_os = "linux")]
#[test]
fn a_connection_ends_with_its_server_though_its_client_reads_nothing() -> Outcome {
    let server = Server::start("")?;
    let pid = server_pid(&server)?;
    let mut stuck = server.attach(10, 40)?;
    stuck.wait_for("$")?;
    let conn = (connections(pid).first().copied()).ok_or("its connection")?;
    server.type_line("%1", r#"while :; do printf '\033]52;c;aGk=\a'; done"#)?;
    eventually("its connection to hold copies", || {
        Ok(peak_kib(conn)? > 8 << 10)
    })?;
    let _reap = reap_run(&server)?;
    signal(pid, Signal::Kill);
    eventually("its connection gone", || Ok(!alive(conn)))?;
    drop(stuck);
    Ok(())
}

/// 14. A close sent to a pane is a close though its server is gone by the
///     time it reads it: a run stopped while a pane's process was stopped
///     leaves no pane once that process goes on (D-124 (1)).
#[test]
fn a_close_read_after_its_server_went_ends_the_pane() -> Outcome {
    let mut server = Server::start("")?;
    let sleeper = pane(&server, "a", &["sleep", "1000"])?;
    let own = own_process(&server, &sleeper)?;
    let program = program_pid(&server, &sleeper)?;
    let _reap = Reap(vec![own, program]);
    signal(own, Signal::Stop);
    signal(server_pid(&server)?, Signal::Term);
    server.wait_exit()?;
    signal(own, Signal::Cont);
    eventually("its pane's process and program gone", || {
        Ok(!alive(own) && !alive(program))
    })
}

/// 15. A change whose record can't be written fails, and is not made: the
///     tree a crash takes back is the one anyone saw (D-124 (3)).
#[test]
fn a_change_whose_record_cannot_be_written_is_not_made() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let shell = pane(server, "a", &["sh"])?;
    server.ok(&[&format!(
        "(sequence! (rename (pane {}) \"one\") #inert)",
        fux::script::quoted(&shell)
    )])?;
    let blocked = kept(server).join("run.new");
    std::fs::create_dir(&blocked).map_err(e)?;
    let refused = server.fux(&[&format!(
        "(sequence! (rename (pane {}) \"two\") #inert)",
        fux::script::quoted(&shell)
    )])?;
    assert_ne!(refused.status, 0);
    assert!(
        refused.stderr.contains("could not be written"),
        "{}",
        refused.stderr
    );
    let listing = server.ok(&["(listing (list-session))"])?;
    assert!(
        listing.contains(" one ") && !listing.contains(" two "),
        "{listing}"
    );
    // A close is undone whole: its pane stays, and takes input.
    let refused = server.fux(&[&format!(
        "(sequence! (kill (pane {})) #inert)",
        fux::script::quoted(&shell)
    )])?;
    assert_ne!(refused.status, 0, "{}", refused.stdout);
    assert!(
        server
            .ok(&["(listing (list-session))"])?
            .contains(&shell.to_string())
    );
    std::fs::remove_dir(&blocked).map_err(e)?;
    server.type_line(&shell, "echo still-here")?;
    server.shows(&shell, "still-here")?;
    signal(server_pid(server)?, Signal::Kill);
    server.ok(&["(sequence! (start) #inert)"])?;
    let listing = server.ok(&["(listing (list-session))"])?;
    assert!(listing.contains(" one "), "{listing}");
    Ok(())
}

/// 16. A pane's id and place are recorded before its program starts: one
///     whose record can't be written starts nothing, and a socket a process
///     still listens at is never taken for debris (D-124 (3)).
#[test]
fn a_pane_is_recorded_before_its_program_starts() -> Outcome {
    let server = Server::start("")?;
    let pid = server_pid(&server)?;
    let panes = children(pid, "__pane").len();
    let blocked = kept(&server).join("run.new");
    std::fs::create_dir(&blocked).map_err(e)?;
    let refused = server.fux(&[format!(
        "(new-tab {})",
        program("(size (in (options) \"a\") 80 24)", &["sleep", "1001"])
    )])?;
    assert_ne!(refused.status, 0, "{}", refused.stdout);
    assert_eq!(
        children(pid, "__pane").len(),
        panes,
        "a pane's process started"
    );
    std::fs::remove_dir(&blocked).map_err(e)?;
    // The next pane's socket, a process listening at it.
    let next = kept(&server).join("2");
    let listener = std::os::unix::net::UnixListener::bind(&next).map_err(e)?;
    let refused = server.fux(&[format!(
        "(new-tab {})",
        program("(size (in (options) \"a\") 80 24)", &["sleep", "1002"])
    )])?;
    assert!(refused.stderr.contains("listens at"), "{}", refused.stderr);
    assert!(next.exists(), "a live socket let go");
    drop(listener);
    Ok(())
}

/// 17. A takeover owes the pane nothing its last server did: a screen sent
///     and never taken, then a frame part way through as the next server
///     reaches it, still gives that one its first screen once the frame is
///     whole (D-124).
#[test]
fn a_takeover_mid_frame_gets_its_first_screen() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let script = r#"sleep 2; printf hello; sleep 2; printf '\033[?2026hframe'; sleep 1000"#;
    let framed = pane(server, "a", &["sh", "-c", script])?;
    let _reap = reap_run(server)?;
    let pid = server_pid(server)?;
    // Its screen with hello is sent, and never taken; then its frame begins,
    // which it holds a second (`FRAME_TIMEOUT`): the next server reaches it
    // in that second.
    signal(pid, Signal::Stop);
    std::thread::sleep(std::time::Duration::from_secs(3));
    signal(pid, Signal::Kill);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let taken = within(server, 20, &["(start)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    assert!(server.ok(&["(listing (list-session))"])?.contains(&framed));
    Ok(())
}

/// 18. A run takes back the fux it started with before its record is
///     read: a record that fux alone can read is its to read (D-124 (7)).
#[test]
fn a_run_chooses_its_fux_before_reading_its_record() -> Outcome {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::start("")?;
    let _reap = reap_run(&server)?;
    let pid = server_pid(&server)?;
    signal(pid, Signal::Kill);
    eventually("the server gone", || Ok(!alive(pid)))?;
    let kept = kept(&server);
    // A record no fux of this build reads, and a run's fux that says it ran.
    std::fs::write(kept.join("run"), b"a record of another fux's").map_err(e)?;
    let ran = server.dir.join("ran");
    let exe = kept.join("exe");
    std::fs::remove_file(&exe).map_err(e)?;
    let script = format!("#!/bin/sh\necho \"$@\" > '{}'\n", ran.display());
    std::fs::write(&exe, script).map_err(e)?;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    let _ = within(&server, 10, &["(start)"])?;
    eventually("its own fux run", || Ok(ran.exists()))?;
    let said = std::fs::read_to_string(&ran).map_err(e)?;
    assert!(said.contains("__serve"), "{said}");
    Ok(())
}

/// 19. A takeover keeps an ended program pane, by its role's rule: one
///     that ended before the crash, and one whose process died while no
///     server ran, both stay, ended; a shell closes (D-120 (6), D-124).
#[test]
fn a_takeover_keeps_ended_program_panes_by_their_role() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let ended = pane(server, "w", &["sleep", "1003"])?;
    let lost = pane(server, "w", &["sleep", "1004"])?;
    // A shell's pane, as no program is given.
    let shell = id(&server.ok(&["(new-tab (size (in (options) \"w\") 80 24))"])?);
    let _reap = reap_run(server)?;
    signal(own_process(server, &ended)?, Signal::Kill);
    // Ended, or on macOS, which gives the server no orphan to reap, its
    // terminal's read failed: either way no longer running.
    eventually("its program no longer running", || {
        let state = server.ok(&[&format!("(sequence! (define! value-1357 (pane {})) \"program\" (define! value-1359 (get value-1357 \"program\")) \"state\" (get value-1359 \"state\"))", fux::script::quoted(&ended))])?;
        Ok(state.trim() != "running")
    })?;
    let (lost_own, shell_own) = (own_process(server, &lost)?, own_process(server, &shell)?);
    let pid = server_pid(server)?;
    signal(pid, Signal::Kill);
    eventually("the server gone", || Ok(!alive(pid)))?;
    signal(lost_own, Signal::Kill);
    signal(shell_own, Signal::Kill);
    eventually("their processes gone", || {
        Ok(!alive(lost_own) && !alive(shell_own))
    })?;
    server.ok(&["(sequence! (start) #inert)"])?;
    let listing = server.ok(&["(listing (list-session))"])?;
    for kept in [&ended, &lost] {
        assert!(listing.contains(&kept.to_string()), "{kept}: {listing}");
        let state = server.ok(&[&format!("(sequence! (define! value-1369 (pane {})) \"program\" (define! value-1371 (get value-1369 \"program\")) \"state\" (get value-1371 \"state\"))", fux::script::quoted(kept))])?;
        assert_eq!(state.trim(), "ended", "{kept}");
    }
    assert!(!listing.contains(&shell.to_string()), "{listing}");
    Ok(())
}

/// 20. A connection ends with its server though a pane it sends a body to
///     reads nothing (stopped): what it does at panes' sockets is part of
///     its loop, which its server's end ends (D-124 (6)).
#[cfg(target_os = "linux")]
#[test]
fn a_connection_sending_a_stopped_pane_a_body_ends_with_its_server() -> Outcome {
    use std::io::Write;
    let server = Server::start("")?;
    let sleeper = pane(&server, "a", &["sleep", "1000"])?;
    let own = own_process(&server, &sleeper)?;
    let pid = server_pid(&server)?;
    let _reap = reap_run(&server)?;
    signal(own, Signal::Stop);
    let mut sending = started(
        &server,
        &[&format!(
            "(send-keys (paste {} (input)))",
            support::reference(&sleeper)
        )],
    )?;
    let mut stdin = sending.stdin.take().ok_or("its stdin")?;
    stdin.write_all(&vec![b'x'; 32 << 20]).map_err(e)?;
    drop(stdin);
    let mut conn = None;
    eventually("its connection to hold it", || {
        conn =
            (connections(pid).into_iter()).find(|conn| peak_kib(*conn).is_ok_and(|k| k > 32 << 10));
        Ok(conn.is_some())
    })?;
    let conn = conn.ok_or("its connection")?;
    signal(pid, Signal::Kill);
    eventually("its connection gone", || Ok(!alive(conn)))?;
    signal(own, Signal::Cont);
    let _ = sending.wait();
    Ok(())
}

/// 21. Keys for a client go from the asking connection to the client's,
///     never through the server (D-123): a paste of any size for one grows
///     no memory of the server's, its client takes all of it as typed, and
///     what is sent it after comes too; keys that can't reach it fail,
///     saying so.
#[cfg(target_os = "linux")]
#[test]
fn keys_for_a_client_never_pass_through_the_server() -> Outcome {
    let server = Server::start("")?;
    let (mut client, id, got) = keeping(&server)?;
    let pid = server_pid(&server)?;
    let before = peak_kib(pid)?;
    let mut sent = 0u64;
    for size in [1usize << 20, 32 << 20, 5] {
        let sending = paste(&server, &id, &vec![b'y'; size])?;
        let (typed, said) = finish(sending, &mut client)?;
        assert!(typed, "{said}");
        sent = sent.saturating_add(u64::try_from(size).map_err(e)?);
        eventually(&format!("{sent} bytes kept"), || {
            client.pump()?;
            Ok(std::fs::metadata(&got).map_err(e)?.len() >= sent)
        })?;
    }
    assert_eq!(std::fs::metadata(&got).map_err(e)?.len(), sent);
    let grew = peak_kib(pid)?.saturating_sub(before);
    assert!(grew < 8 << 10, "the server grew {grew} KiB");
    let socket = kept(&server).join(format!("c{}", id.trim_start_matches('c')));
    std::fs::remove_file(&socket).map_err(e)?;
    let (typed, said) = finish(paste(&server, &id, b"lost")?, &mut client)?;
    assert!(!typed && said.contains("did not reach"), "{said}");
    Ok(())
}

/// A client that keeps what it is typed, attached to `server`, and where.
#[cfg(target_os = "linux")]
fn keeping(server: &Server) -> Result<(Client, String, PathBuf), String> {
    let mut client = server.attach(24, 80)?;
    client.wait_for("$")?;
    let id = client.id.clone().ok_or("its id")?;
    let got = server.dir.join("got");
    client.keys(&format!(
        "stty -icanon -echo; exec cat > {}\r",
        got.display()
    ))?;
    eventually("its program to keep what it is sent", || {
        client.pump()?;
        Ok(got.exists())
    })?;
    Ok((client, id, got))
}

/// `input` pasted at client `id`, under way.
#[cfg(target_os = "linux")]
fn paste(server: &Server, id: &str, input: &[u8]) -> Result<std::process::Child, String> {
    use std::io::Write;
    let mut sending = started(
        server,
        &[&format!(
            "(send-keys (paste (client {}) (input)))",
            fux::script::quoted(id)
        )],
    )?;
    let mut stdin = sending.stdin.take().ok_or("its stdin")?;
    stdin.write_all(input).map_err(e)?;
    Ok(sending)
}

/// What `sending` came to, `client` taking what it is sent the while
/// (it answers the question sent it after keys, as a terminal's does):
/// whether it succeeded, and what it said.
#[cfg(target_os = "linux")]
fn finish(mut sending: std::process::Child, client: &mut Client) -> Result<(bool, String), String> {
    let mut status = None;
    eventually("the keys sent", || {
        let _ = client.pump();
        status = sending.try_wait().map_err(e)?;
        Ok(status.is_some())
    })?;
    let mut said = String::new();
    if let Some(mut stderr) = sending.stderr.take() {
        std::io::Read::read_to_string(&mut stderr, &mut said).map_err(e)?;
    }
    Ok((status.is_some_and(|s| s.success()), said))
}

/// 22. Keys for a client that detaches before it takes them fail, saying
///     so: never status 0 for keys its program never got. The detach
///     always completes: its client is told and its connection ends. Its
///     connection held still while it detaches, then at real times.
#[cfg(target_os = "linux")]
#[test]
fn keys_for_a_client_that_detaches_first_fail() -> Outcome {
    const SIZE: usize = 32 << 20;
    let detached = |client: &mut Client, conn: i32, at: &str| {
        eventually(&format!("it detached, {at}"), || {
            let _ = client.pump();
            Ok(client.exit.as_deref() == Some("detached"))
        })?;
        eventually(&format!("its connection gone, {at}"), || Ok(!alive(conn)))
    };
    let the_clients = |server: &Server| -> Result<i32, String> {
        match connections(server_pid(server)?)[..] {
            [conn] => Ok(conn),
            _ => Err("one connection, the client's".into()),
        }
    };
    let server = Server::start("")?;
    let (mut client, id, got) = keeping(&server)?;
    let conn = the_clients(&server)?;
    signal(conn, Signal::Stop);
    let sending = paste(&server, &id, &vec![b'y'; SIZE])?;
    let pid = server_pid(&server)?;
    eventually("the keys on their way", || {
        Ok((connections(pid).into_iter())
            .any(|asker| asker != conn && peak_kib(asker).is_ok_and(|k| k > 32 << 10)))
    })?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    server.ok(&[&format!(
        "(sequence! (detach (client {})) #inert)",
        fux::script::quoted(&id)
    )])?;
    signal(conn, Signal::Cont);
    let (typed, said) = finish(sending, &mut client)?;
    assert!(!typed && said.contains("did not reach"), "{said}");
    detached(&mut client, conn, "held")?;
    assert_eq!(std::fs::metadata(&got).map_err(e)?.len(), 0);
    for delay in [0u64, 10, 20, 40, 60, 80] {
        let server = Server::start("")?;
        let (mut client, id, got) = keeping(&server)?;
        let conn = the_clients(&server)?;
        let sending = paste(&server, &id, &vec![b'y'; SIZE])?;
        std::thread::sleep(std::time::Duration::from_millis(delay));
        server.ok(&[&format!(
            "(sequence! (detach (client {})) #inert)",
            fux::script::quoted(&id)
        )])?;
        let (typed, said) = finish(sending, &mut client)?;
        if typed {
            let whole = u64::try_from(SIZE).map_err(e)?;
            eventually(&format!("all typed, at {delay} ms"), || {
                Ok(std::fs::metadata(&got).map_err(e)?.len() == whole)
            })?;
        } else {
            let lost = said.contains("did not reach") || said.contains("no client");
            assert!(lost, "{delay} ms: {said}");
        }
        detached(&mut client, conn, &format!("at {delay} ms"))?;
    }
    Ok(())
}
