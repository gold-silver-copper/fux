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

/// How many sockets the system has at `path`: the one listening there and
/// each connection that reached it, taken by its listener or not yet (a
/// process stopped takes none), till both its ends are closed
/// (`/proc/net/unix`).
#[cfg(target_os = "linux")]
fn reached(path: &std::path::Path) -> Result<usize, String> {
    let path = path.to_str().ok_or("its path")?;
    let sockets = std::fs::read_to_string("/proc/net/unix").map_err(e)?;
    Ok(sockets.lines().filter(|line| line.ends_with(path)).count())
}

/// Where pane `pane`'s own process listens.
#[cfg(target_os = "linux")]
fn pane_socket(server: &Server, pane: &str) -> PathBuf {
    kept(server).join(pane.trim_start_matches('%'))
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

/// 2. A program that asks its terminal what it is, far more than its
///    pane's process may hold the answers to (a cap of its memory), and
///    reads none till it has asked it all, having written megabytes
///    before it reads: its pane holds a frame of answers, reads it no
///    more till it reads, answers its server meanwhile, and its process
///    lives. That process grew with every answer, and ended at its cap.
#[cfg(target_os = "linux")]
#[test]
fn a_flood_of_queries_none_reads_leaves_its_pane_process_flat_and_answering() -> Outcome {
    let server = Server::start("")?;
    let flood = pane(&server, "a", &["sleep", "1000"])?;
    let own = own_process(&server, &flood)?;
    let cap_bytes = 16 << 20;
    cap(own, cap_bytes)?;
    let before = peak_kib(own)?;
    let go = kept(&server).join("read-now");
    // 4 MB of queries, each answered with more than six times its
    // length: more than the cap; cut where a query ends, so that what is
    // printed after is text. A reader takes the answers once told to.
    let asks = format!(
        r#"stty raw -echo; (until [ -e {go} ]; do sleep 0.05; done; cat </dev/tty >/dev/null) & q=$(printf '\033[c%.0s' $(seq 200)); yes "$q" | head -c 4000000; printf 'ASKED-%s' ALL; sleep 1000"#,
        go = go.display()
    );
    let respawn = vec![format!(
        "(respawn (kill-first {}))",
        program(
            &format!("(pane {})", fux::script::quoted(&flood)),
            &["sh", "-c", &asks]
        )
    )];
    server.ok(&respawn)?;
    // Its pane answers its server while its program floods it.
    for (rows, cols) in [(10, 40), (12, 50)] {
        let resize = format!(
            "(resize (size (pane {}) {cols} {rows}))",
            fux::script::quoted(&flood)
        );
        let _ = server.fux(&[&resize])?;
        server.ok(&[&format!("(capture (pane {}))", fux::script::quoted(&flood))])?;
    }
    std::fs::write(&go, b"").map_err(e)?;
    let capture = format!("(capture (pane {}))", fux::script::quoted(&flood));
    eventually("all it asked to be read", || {
        Ok(server.ok(&[&capture])?.contains("ASKED-ALL"))
    })?;
    assert!(alive(own), "its pane's process ended");
    assert_eq!(own_process(&server, &flood)?, own);
    let grew = peak_kib(own)?.saturating_sub(before);
    assert!(grew < 8 << 10, "its pane's process grew {grew} KiB");
    Ok(())
}

/// 3. A client that stops reading while a pane floods it with copies costs
///    neither its connection's process nor the server: what it is owed is
///    the latest copy, kept as that in both (D-184), so neither grows, the
///    server and another client go on, and once it reads again it gets
///    the last copy, and not each of those before it. (Its connection
///    queued every copy, and ended at its cap: this test said so.)
#[cfg(target_os = "linux")]
#[test]
fn a_client_that_stops_reading_costs_its_connection_and_the_server_nothing() -> Outcome {
    const COPIES: usize = 200_000;
    let server = Server::start("")?;
    let pid = i32::try_from(server.pid().ok_or("the server's pid")?).map_err(e)?;
    let mut stuck = server.attach(10, 40)?;
    stuck.wait_for("$")?;
    let conns = connections(pid);
    let [conn] = conns.as_slice() else {
        return Err(format!("its connection's process: {conns:?}"));
    };
    let conn = *conn;
    let shell = support::focused(&server)?;
    let before = (peak_kib(conn)?, peak_kib(pid)?);
    // It no longer reads (`pump`) from here on. Its pane copies "hi" over
    // and over, then "last", and says it is done.
    let done = server.dir.join("copied");
    let copies = format!(
        r#"i=0; while [ $i -lt {COPIES} ]; do printf '\033]52;c;aGk=\a'; i=$((i+1)); done; printf '\033]52;c;bGFzdA==\a'; : > '{}'"#,
        done.display()
    );
    server.type_line(&shell, &copies)?;
    let mut copied = false;
    for _ in 0..12 {
        copied = eventually("its pane to have copied them all", || Ok(done.exists())).is_ok();
        if copied {
            break;
        }
    }
    assert!(copied, "its pane did not copy them all");
    assert!(alive(pid), "the server");
    assert!(alive(conn), "its connection's process ended");
    let mut other = server.attach(10, 40)?;
    other.wait_for("$")?;
    other.detach()?;
    // It reads again: the last copy comes, the server having taken every
    // copy its pane made by then.
    let (last, each): (&[u8], &[u8]) = (b"\x1b]52;c;bGFzdA==\x07", b"\x1b]52;c;aGk=\x07");
    let has = |painted: &[u8], copy: &[u8]| {
        (0..painted.len())
            .filter(|at| {
                painted
                    .get(*at..)
                    .is_some_and(|rest| rest.starts_with(copy))
            })
            .count()
    };
    eventually("the last copy", || {
        stuck.pump()?;
        Ok(has(&stuck.painted, last) > 0)
    })?;
    let got = has(&stuck.painted, each);
    assert!(got < COPIES / 2, "it was sent {got} of {COPIES} copies");
    let grew = (
        peak_kib(conn)?.saturating_sub(before.0),
        peak_kib(pid)?.saturating_sub(before.1),
    );
    assert!(
        grew.0 < 8 << 10,
        "its connection's process grew {} KiB",
        grew.0
    );
    // Measured in a debug build, whether its pane made 60,000 copies or
    // 400,000: its connection +0 KiB, the server +3 to +5 MiB.
    assert!(grew.1 < 16 << 10, "the server grew {} KiB", grew.1);
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
    // A listing reads each pane as the server has it, from its record
    // till its own process answers; what is asked of a pane waits for it.
    for pane in [&shell, &sleeper] {
        server.version(pane)?;
    }
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
        let status = within(server, 30, &["(status)"])?;
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
    let at = pane_socket(&server, &sleeper);
    let links = reached(&at)?;
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
    // It stays with its sender, which waits: a piece at a time goes on
    // (D-181), so no connection's process holds it either. Its
    // connection has reached the pane's socket with the first piece, which
    // the pane, stopped, does not take: it reads no more of it from here.
    eventually("its connection to reach the pane", || {
        Ok(reached(&at)? > links)
    })?;
    let grew = peak_kib(pid)?.saturating_sub(before);
    let held = (connections(pid).into_iter())
        .filter_map(|conn| peak_kib(conn).ok())
        .max();
    signal(own, Signal::Cont);
    let _ = sending.wait();
    assert!(grew < 8 << 10, "the server grew {grew} KiB");
    assert!(
        held.is_some_and(|k| k < 16 << 10),
        "a connection held {held:?} KiB"
    );
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
    // A frame of what was typed is the most it keeps for a pane that
    // takes none (D-193): its queue, and what its allocator keeps of it.
    assert!(grew < 8 << 10, "the server grew {grew} KiB");
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

/// 13a. A client gone while its request waits (killed, Ctrl-C, a timeout)
///      ends its connection's process, and so the wait. The request asks
///      an attached client what only it knows, which that client, the
///      test's own, never answers: the question come to it, the request is
///      known to wait.
#[cfg(target_os = "linux")]
#[test]
fn a_client_gone_while_its_request_waits_ends_its_connection() -> Outcome {
    use fux_wire::command::{ClientRead, Request};
    use fux_wire::protocol::{
        Asked, Attach, Context, Decoder, Frame, Greeting, Hello, PROTOCOL, Role, ToAttached,
    };
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let server = Server::start("")?;
    let pid = server_pid(&server)?;
    // A client of `role`, greeted: its connection's process has it.
    let greeted = |role| -> Result<(UnixStream, Decoder), String> {
        let mut client = UnixStream::connect(&server.socket).map_err(e)?;
        client.set_read_timeout(Some(PATIENCE)).map_err(e)?;
        let hello = Hello {
            protocol: PROTOCOL,
            version: "0".into(),
            role,
        };
        client.write_all(&hello.encode()).map_err(e)?;
        let mut decoder = Decoder::default();
        while decoder.frame::<Greeting>().map_err(e)?.is_none() {
            more(&mut client, &mut decoder)?;
        }
        Ok((client, decoder))
    };
    let (mut asked, mut frames) = greeted(Role::Attach)?;
    let attach = Attach::of(sized(10, 40)?, server.state.preferences.presentation());
    asked.write_all(&attach.encode()).map_err(e)?;
    let me = loop {
        match frames.frame::<ToAttached>().map_err(e)? {
            Some(ToAttached::Welcome(me, _)) => break me,
            Some(_) => {}
            None => more(&mut asked, &mut frames)?,
        }
    };
    let before = connections(pid);
    let (mut client, _) = greeted(Role::Command)?;
    let mut conn = None;
    eventually("its connection", || {
        conn = (connections(pid).into_iter()).find(|conn| !before.contains(conn));
        Ok(conn.is_some())
    })?;
    let conn = conn.ok_or("its connection")?;
    let request = Asked {
        after: None,
        request: Request::Read {
            client: me,
            read: ClientRead::Keys,
        },
        context: Context {
            of: None,
            cwd: PathBuf::new(),
            pane: None,
            server: None,
            client: None,
        },
    };
    client.write_all(&request.encode()).map_err(e)?;
    loop {
        match frames.frame::<ToAttached>().map_err(e)? {
            Some(ToAttached::Read(..)) => break,
            Some(_) => {}
            None => more(&mut asked, &mut frames)?,
        }
    }
    // Gone as a killed client is: its socket closed.
    drop(client);
    eventually("its connection gone", || Ok(!alive(conn)))?;
    assert!(alive(pid), "the server");
    Ok(())
}

/// More of what `stream` was sent, into `decoder`.
#[cfg(target_os = "linux")]
fn more(
    stream: &mut std::os::unix::net::UnixStream,
    decoder: &mut fux_wire::protocol::Decoder,
) -> Outcome {
    use std::io::Read;
    let mut buffer = [0u8; 4096];
    let n = stream.read(&mut buffer).map_err(e)?;
    if n == 0 {
        return Err("the server closed its socket".into());
    }
    decoder.push(buffer.get(..n).unwrap_or_default());
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
    // Where its server writes a record first, before it renames it.
    let blocked = kept(server).join(format!("run.new.{}", server_pid(server)?));
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

/// The numbers of the name files a run keeps (`record::name_file`): each
/// its server's process and which of its names it is.
fn name_files(server: &Server) -> Vec<String> {
    let entries = std::fs::read_dir(kept(server)).into_iter().flatten();
    let mut names: Vec<String> = (entries.flatten())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.strip_prefix("name.").map(str::to_owned))
        .filter(|number| !number.ends_with(".new"))
        .collect();
    names.sort();
    names
}

/// 15b. A server killed between writing a name and writing the structure
///     that names it leaves a run the next start takes back, with the
///     names anyone saw; the name's file, which no structure names, goes
///     with the first record the new server writes (D-175). The moment is held, not raced: a structure is written to
///     `run.new.PID` first, and a FIFO put there holds the server at its
///     opening, the name's file written, till it is killed.
#[cfg(unix)]
#[test]
fn a_server_killed_between_a_name_and_the_structure_is_taken_back() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let pane = pane(server, "a", &["sleep", "1016"])?;
    let _reap = reap_run(server)?;
    let rename = |name: &str| {
        let pane = fux::script::quoted(&pane);
        format!("(sequence! (rename (pane {pane}) \"{name}\") #inert)")
    };
    server.ok(&[&rename("one")])?;
    let recorded = name_files(server);
    // Found while it answers: held at the FIFO, it answers no one.
    let serving = serving(server)?;
    let held = kept(server).join(format!("run.new.{serving}"));
    let made = std::process::Command::new("mkfifo").arg(&held).status();
    assert!(
        made.map_err(|error| format!("mkfifo: {error}"))?.success(),
        "mkfifo {}",
        held.display()
    );
    let mut renaming = server.spawn(&[&rename("two")])?;
    let written = eventually("the new name's file written", || {
        Ok(name_files(server).len() > recorded.len())
    });
    signal(serving, Signal::Kill);
    let crashed = written.and(eventually("the server gone", || Ok(!alive(serving))));
    let _ = end(&mut renaming);
    std::fs::remove_file(&held).map_err(e)?;
    crashed?;
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    let listing = server.ok(&["(listing (list-session))"])?;
    assert!(
        listing.contains(" one ") && !listing.contains(" two "),
        "{listing}"
    );
    // And it is the run still, and this server's: its pane takes a name,
    // and with that record the file no structure names goes.
    server.ok(&[&rename("three")])?;
    assert!(
        server
            .ok(&["(listing (list-session))"])?
            .contains(" three ")
    );
    assert_eq!(name_files(server).len(), recorded.len(), "a name, a file");
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
    let blocked = kept(&server).join(format!("run.new.{pid}"));
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
    let at = pane_socket(&server, &sleeper);
    let links = reached(&at)?;
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
    // The body stays with its sender (D-181): its connection, which has
    // reached the stopped pane's socket with the first piece, is one of
    // those the server has.
    eventually("its connection to reach the pane", || {
        Ok(reached(&at)? > links)
    })?;
    let conns = connections(pid);
    signal(pid, Signal::Kill);
    eventually("its connections gone", || {
        Ok(conns.iter().all(|conn| !alive(*conn)))
    })?;
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
    // They stay with their sender (D-181): its connection reaches the
    // client's, stopped, with the first piece of them, none of it taken.
    let at = kept(&server).join(format!("c{}", id.trim_start_matches('c')));
    let links = reached(&at)?;
    let sending = paste(&server, &id, &vec![b'y'; SIZE])?;
    eventually("the keys on their way", || Ok(reached(&at)? > links))?;
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

/// A client's allocation refused aborts it, which no panic hook sees: its
/// terminal comes back all the same, off the alternate screen and out of raw
/// mode. Printing a value that shares its parts takes more text than any
/// memory holds, in a string of fux's own, which refuses by aborting.
#[cfg(target_os = "linux")]
#[test]
fn a_client_that_aborts_puts_its_terminal_back() -> Outcome {
    use std::os::unix::process::ExitStatusExt;
    let server = Server::start(
        r#"(define! shared (lambda! (n d) (if! (same? n 0) d (shared (subtract n 1) (list d d)))))
        (bind-key "C-b g" (code! (lambda! () (data-text (shared 40 ())))))"#,
    )?;
    let mut terminal = Terminal::attach(&server, 12, 50, &[])?;
    terminal.wait_for("$")?;
    assert!(!terminal.modes()?.echoes(), "attached in raw mode");
    cap(i32::try_from(terminal.child.id()).map_err(e)?, 32 << 20)?;
    terminal.type_bytes(b"\x02g")?;
    let status = terminal.wait_exit()?;
    assert_eq!(status.signal(), Some(Signal::Abrt.raw()), "{status}");
    let modes = terminal.modes()?;
    assert!(modes.echoes() && modes.edits_lines(), "still raw");
    let output = String::from_utf8_lossy(&terminal.output);
    assert!(
        output.contains("\x1b[?1049l"),
        "still on the alternate screen"
    );
    Ok(())
}

/// Sets `pid`'s soft limit `which` to `soft` (`prlimit`, util-linux), its
/// hard one kept.
#[cfg(target_os = "linux")]
fn limit(pid: i32, which: &str, soft: u64) -> Outcome {
    let out = std::process::Command::new("prlimit")
        .args(["--pid", &pid.to_string(), &format!("--{which}={soft}:")])
        .output()
        .map_err(e)?;
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr).into_owned()),
    }
}

/// The connection's process that waits at `pid`'s socket for the next
/// client (`Server::spare`).
#[cfg(target_os = "linux")]
fn spare(pid: i32) -> Result<i32, String> {
    let mut found = None;
    eventually("a connection waiting for a client", || {
        let taken = connections(pid);
        found = (children(pid, "__conn").into_iter()).find(|conn| !taken.contains(conn));
        Ok(found.is_some())
    })?;
    found.ok_or_else(|| "a connection waiting for a client".into())
}

/// Three clients, one after another, of a server that has met a limit
/// (`ulimit`): the one its waiting connection's process takes may be
/// served; each one refused is told `cause`, and two are, the second by a
/// server that was ready to tell again.
#[cfg(target_os = "linux")]
fn refused_naming(server: &Server, cause: &str) -> Outcome {
    let mut refused = 0u32;
    for _ in 0..3 {
        let asked = within(server, 30, &["(length (panes (all)))"])?;
        if asked.status != 0 {
            assert!(asked.stderr.contains(cause), "{}", asked.stderr);
            refused = refused.saturating_add(1);
        }
    }
    assert!(refused >= 2, "{refused} refused");
    Ok(())
}

/// A server with every descriptor its limit allows takes the client that
/// waits with the one it kept back, and tells it so: the client fails at
/// once naming the cause, not after waiting for an answer that never
/// comes; and the next is told too.
#[cfg(target_os = "linux")]
#[test]
fn a_server_out_of_descriptors_tells_a_client_so() -> Outcome {
    let server = Server::start("")?;
    let pid = server_pid(&server)?;
    spare(pid)?;
    no_descriptor_free(pid)?;
    refused_naming(&server, "Too many open files")
}

/// Lowers `pid`'s descriptor limit so that no number is free below it: to
/// the least number it has not open.
#[cfg(target_os = "linux")]
fn no_descriptor_free(pid: i32) -> Outcome {
    let mut open: Vec<u64> = (std::fs::read_dir(format!("/proc/{pid}/fd")).map_err(e)?)
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    open.sort_unstable();
    let free = (0u64..).zip(&open).find(|(n, fd)| n != *fd);
    let free = free.map_or(u64::try_from(open.len()).map_err(e)?, |(n, _)| n);
    limit(pid, "nofile", free)
}

/// So is a client its waiting connection's process would have taken, had
/// it not been stopped: left to that process once, it is told by the
/// server, which does not wait on it for good.
#[cfg(target_os = "linux")]
#[test]
fn a_server_out_of_descriptors_tells_a_client_its_stopped_connection_left() -> Outcome {
    let server = Server::start("")?;
    let pid = server_pid(&server)?;
    let stopped = spare(pid)?;
    signal(stopped, Signal::Stop);
    no_descriptor_free(pid)?;
    let asked = within(&server, 30, &["(length (panes (all)))"]);
    signal(stopped, Signal::Cont);
    let asked = asked?;
    assert!(
        asked.stderr.contains("Too many open files"),
        "{}",
        asked.stderr
    );
    Ok(())
}

/// A server that can start no process tells a client it can give none so,
/// which fails at once naming the cause, not with a connection reset.
#[cfg(target_os = "linux")]
#[test]
fn a_server_out_of_processes_tells_a_client_so() -> Outcome {
    // A limit on processes does not hold root.
    if fuxix::process::geteuid() == 0 {
        return Ok(());
    }
    let server = Server::start("")?;
    let pid = server_pid(&server)?;
    spare(pid)?;
    limit(pid, "nproc", 1)?;
    refused_naming(&server, "starting a connection's process")
}

/// A connection's process that waits at the socket and is stopped holds up
/// no client: the server takes there what it has not taken.
#[cfg(target_os = "linux")]
#[test]
fn a_stopped_waiting_connection_holds_up_no_client() -> Outcome {
    let server = Server::start("")?;
    let stopped = spare(server_pid(&server)?)?;
    signal(stopped, Signal::Stop);
    let asked = within(&server, 30, &["(length (panes (all)))"]);
    signal(stopped, Signal::Cont);
    let asked = asked?;
    assert_eq!(asked.status, 0, "{}", asked.stderr);
    Ok(())
}

/// A write past a file-size limit (`ulimit -f`) fails as a write, where it
/// is told, and ends no process of fux: a tree change whose record can't be
/// written so is refused, and the server lives; a client whose output
/// can't be says so.
#[cfg(target_os = "linux")]
#[test]
fn a_write_past_the_file_size_limit_is_refused_not_fatal() -> Outcome {
    let server = Server::start("")?;
    let pid = server_pid(&server)?;
    let shell = pane(&server, "a", &["sh"])?;
    limit(pid, "fsize", 16)?;
    let refused = server.fux(&[&format!(
        "(sequence! (rename (pane {}) \"renamed\") #inert)",
        fux::script::quoted(&shell)
    )])?;
    assert_ne!(refused.status, 0);
    assert!(
        refused.stderr.contains("could not be written"),
        "{}",
        refused.stderr
    );
    let listing = server.ok(&["(listing (list-session))"])?;
    assert!(!listing.contains(" renamed "), "{listing}");
    assert!(alive(pid), "the server");
    let saved = server.dir.join("saved");
    let dump = (server.command("/bin/sh"))
        .args(["-c", "ulimit -f 0 && exec \"$0\" \"$@\" > \"$SAVED\""])
        .args([fux(), "(dump (all))"])
        .env("SAVED", &saved)
        .output()
        .map_err(e)?;
    let said = String::from_utf8_lossy(&dump.stderr);
    assert!(
        said.contains("writing the output"),
        "{:?} {said}",
        dump.status
    );
    Ok(())
}

/// A client whose request waits, and its connection's process.
#[cfg(target_os = "linux")]
fn waiting_client(server: &Server) -> Result<(std::process::Child, i32), String> {
    let pid = server_pid(server)?;
    let before = connections(pid);
    let waits = started(server, &["(shows (pane 1) \"never shown\" 600)"])?;
    let mut conn = None;
    eventually("its connection", || {
        conn = (connections(pid).into_iter()).find(|conn| !before.contains(conn));
        Ok(conn.is_some())
    })?;
    Ok((waits, conn.ok_or("its connection")?))
}

/// A client whose server is killed while its request waits is told the
/// server's process ended, and how its run is taken back: its connection's
/// process, which outlives its server, says so.
#[cfg(target_os = "linux")]
#[test]
fn a_client_whose_server_is_killed_says_it_ended() -> Outcome {
    let server = Server::start("")?;
    let _reap = reap_run(&server)?;
    let (waits, _) = waiting_client(&server)?;
    signal(server_pid(&server)?, Signal::Kill);
    let out = waits.wait_with_output().map_err(e)?;
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("the server process ended") && said.contains("`fux '(start)'`"),
        "{said}"
    );
    Ok(())
}

/// A client whose connection's process alone is killed does not say its
/// server ended, which serves on: it says its connection's process did,
/// and how to tell whether a server runs.
#[cfg(target_os = "linux")]
#[test]
fn a_client_whose_connection_process_is_killed_blames_no_server() -> Outcome {
    let server = Server::start("")?;
    let (waits, conn) = waiting_client(&server)?;
    signal(conn, Signal::Kill);
    let out = waits.wait_with_output().map_err(e)?;
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("connection process") && !said.contains("the server process ended"),
        "{said}"
    );
    server.ok(&["(length (panes (all)))"])?;
    Ok(())
}

/// Kills the server that serves now, as a crash does, and waits till it
/// is gone.
fn crash(server: &Server) -> Outcome {
    let pid = serving(server)?;
    signal(pid, Signal::Kill);
    eventually("the server gone", || Ok(!alive(pid)))
}

/// The run's record as its crashed server left it, changed by `change`
/// and written back: what a crash at another moment would have left.
fn rerecord(server: &Server, change: impl FnOnce(&mut fux_server::record::Record)) -> Outcome {
    let path = kept(server).join("run");
    let bytes = std::fs::read(&path).map_err(e)?;
    let mut record: fux_server::record::Record =
        fux_wire::wire::Reader::new(&bytes).whole().map_err(e)?;
    change(&mut record);
    std::fs::write(&path, fux_wire::wire::encode(&record)).map_err(e)
}

/// `(capture PANE)` asked of `server`.
fn capture(server: &Server, pane: &str) -> Result<Output, String> {
    within(
        server,
        30,
        &[&format!("(capture (pane {}))", fux::script::quoted(pane))],
    )
}

/// `(capture PANE)` asked of `server`, started and not waited for.
fn capturing(server: &Server, pane: &str) -> Result<std::process::Child, String> {
    server
        .command(fux())
        .arg(format!("(capture (pane {}))", fux::script::quoted(pane)))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(e)
}

/// 26. A pane's process stopped as its server crashes holds up neither the
///     start nor the other panes: `start` waits for no pane, the rest is
///     served, and what is asked of the stopped pane waits till it
///     answers, and is answered then; a run that ends meanwhile ends it
///     once it reads (D-171, D-172; it hung the start, and every client).
#[test]
fn a_stopped_pane_holds_up_only_what_is_asked_of_it() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let goes = pane(server, "w", &["sleep", "1010"])?;
    let stops = pane(server, "w", &["sleep", "1011"])?;
    let _reap = reap_run(server)?;
    let stopped = own_process(server, &stops)?;
    signal(stopped, Signal::Stop);
    crash(server)?;
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    // Asked of the stopped pane first: it is still unanswered once the
    // other pane, asked after it, has answered, and a listing has.
    let mut waits = capturing(server, &stops)?;
    let rest = capture(server, &goes)?;
    assert_eq!(rest.status, 0, "{}", rest.stderr);
    assert!(server.ok(&["(listing (list-session))"])?.contains(&stops));
    assert!(waits.try_wait().map_err(e)?.is_none(), "it did not wait");
    signal(stopped, Signal::Cont);
    let answered = waits.wait_with_output().map_err(e)?;
    let said = String::from_utf8_lossy(&answered.stderr);
    assert!(answered.status.success(), "it waited, and failed: {said}");
    assert_eq!(own_process(server, &stops)?, stopped);
    // Stopped again, and its server crashed again: the run ends (a stop of
    // the server that took it back) without waiting for it, and it ends
    // with it once it reads what it was told.
    let program = program_pid(server, &stops)?;
    signal(stopped, Signal::Stop);
    crash(server)?;
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    let ending = serving(server)?;
    let stop = within(server, 30, &["(sequence! (kill-server) #inert)"])?;
    assert_eq!(stop.status, 0, "{}", stop.stderr);
    eventually("the server that ended the run gone", || Ok(!alive(ending)))?;
    // Not known to have read that its run ended, it keeps the run's record
    // there, which says so: a start meanwhile is not refused for a record
    // that is gone, and begins a new run beside it.
    assert!(kept(server).join("run").exists(), "its record went");
    assert!(alive(stopped), "the stopped pane's process ended unread");
    let again = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(again.status, 0, "{}", again.stderr);
    assert_eq!(server.ok(&["(length (panes (all)))"])?.trim(), "0");
    signal(stopped, Signal::Cont);
    eventually("the pane that stopped ended with its run", || {
        Ok(!alive(stopped) && !alive(program))
    })
}

/// 27. A pane whose process its server started and never began (the
///     server killed between the two, a few ms into a `new-tab`) is begun
///     by the server that takes the run back, from its record: that
///     process runs its program, and the run is not refused for it
///     (D-171). The state is made, not raced (`unbegun`).
#[test]
fn a_pane_never_begun_is_begun_from_its_record() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let beside = pane(server, "w", &["sleep", "1012"])?;
    let pane_id = pane(server, "w", &["sleep", "1013"])?;
    let mut reap = reap_run(server)?;
    vacate(server, &pane_id)?;
    let fresh = unbegun(server, &pane_id)?;
    reap.0.push(fresh);
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    // Asked of the pane, so answered once it began: by that very process.
    assert_eq!(own_process(server, &pane_id)?, fresh);
    assert_eq!(capture(server, &beside)?.status, 0);
    Ok(())
}

/// The record's tree without the tab that holds `pane`.
fn without(tree: &mut fux_server::record::Tree, pane: fux_wire::id::PaneId) {
    use fux_wire::command::AnyRef;
    let holds = |tab: &fux_server::record::Tab| tab.panes.iter().any(|p| p.id == pane);
    let gone: Vec<AnyRef> = (tree.tabs.iter().filter(|tab| holds(tab)))
        .map(|tab| AnyRef::Tab(tab.id))
        .collect();
    tree.tabs.retain(|tab| !holds(tab));
    for ws in &mut tree.workspaces {
        ws.children.retain(|child| !gone.contains(child));
    }
}

/// 28. A pane process the run's record lacks (its close was recorded, and
///     the server killed before it told the pane) is ended by the server
///     that takes the run back: no process is left that no word lists
///     (D-171). The state is made, not raced: the record of a crashed run
///     with one pane's tab taken out.
#[test]
fn a_pane_process_its_record_lacks_is_ended() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let stays = pane(server, "w", &["sleep", "1014"])?;
    let stray = pane(server, "w", &["sleep", "1015"])?;
    let _reap = reap_run(server)?;
    let (own, program) = (own_process(server, &stray)?, program_pid(server, &stray)?);
    crash(server)?;
    let id: fux_wire::id::PaneId = stray.parse().map_err(|_| format!("{stray}: no pane"))?;
    rerecord(server, |record| without(&mut record.tree, id))?;
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    eventually("the process its record lacks and its program gone", || {
        Ok(!alive(own) && !alive(program))
    })?;
    let listing = server.ok(&["(listing (list-session))"])?;
    assert!(
        listing.contains(&stays) && !listing.contains(&stray),
        "{listing}"
    );
    Ok(())
}

/// What a cleaner of idle files can remove beside a live server's socket
/// is removed: every file there named after the socket, the socket's own
/// too (its lock, on a build that keeps one in a file; the file that
/// orders starts). The run's directory, in use, stays.
fn clean_beside(server: &Server) -> Outcome {
    let name = server.socket.file_name().ok_or("the socket's name")?;
    let name = name.to_string_lossy().into_owned();
    let beside = server.socket.parent().ok_or("the socket's directory")?;
    for entry in std::fs::read_dir(beside).map_err(e)? {
        let entry = entry.map_err(e)?;
        let named = entry.file_name().to_string_lossy().starts_with(&name);
        if named && !entry.file_type().map_err(e)?.is_dir() {
            std::fs::remove_file(entry.path()).map_err(e)?;
        }
    }
    Ok(())
}

/// What a `start` on `server`'s socket, refused, said: on its own stderr,
/// or in the log of the server it started.
fn refused_start(server: &Server) -> Result<String, String> {
    let start = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_ne!(start.status, 0, "a second server started");
    let log = std::fs::read_to_string(server.socket.with_file_name("fux.log"));
    Ok(start.stderr + &log.unwrap_or_default())
}

/// 29. A run a live server holds is not taken, whatever was removed
///     beside its socket (a cleaner of idle files): a `start` is refused,
///     naming the server that holds the run and saying its socket is
///     gone, and changes nothing: no socket is made, the record is as it
///     was, and the panes go on, held as before (D-180). (The lock was a
///     file beside the socket: with it removed a second server started on
///     the run, and left only once a pane said who held it.)
#[test]
fn a_run_a_live_server_holds_is_refused_whatever_was_removed_beside_it() -> Outcome {
    let server = Server::start("")?;
    let sleeper = pane(&server, "w", &["sleep", "1016"])?;
    let _reap = reap_run(&server)?;
    let own = own_process(&server, &sleeper)?;
    let programs: Vec<i32> = server.panes()?.into_iter().map(|(_, pid)| pid).collect();
    let pid = server_pid(&server)?;
    clean_beside(&server)?;
    let record = kept(&server).join("run");
    let before = std::fs::read(&record).map_err(e)?;
    let said = refused_start(&server)?;
    assert!(
        said.contains("another fux server") && said.contains("its socket file is gone"),
        "{said}"
    );
    #[cfg(target_os = "linux")]
    assert!(said.contains(&format!("(process {pid})")), "{said}");
    assert!(!server.socket.exists(), "the refused start made a socket");
    assert_eq!(std::fs::read(&record).map_err(e)?, before, "its record");
    assert!(alive(own) && alive(pid), "the pane or its server ended");
    assert!(programs.iter().all(|pid| alive(*pid)), "a program ended");
    // Its server, which no client reaches now, ends its run as it stops.
    signal(pid, Signal::Term);
    eventually("the first server and its pane gone", || {
        Ok(!alive(pid) && !alive(own))
    })
}

/// 29b. A run is its server's while that server lives, though it is
///     stopped and a pane of it gone meanwhile: a `start` then is refused
///     too, and neither writes a record nor removes a name's file (D-180;
///     a second server once took such a run, recorded the pane gone and
///     wrote names of its own beside the first's, D-175).
#[test]
fn a_stopped_servers_run_is_refused_with_its_record_and_names_as_they_were() -> Outcome {
    let server = Server::start("")?;
    let stays = pane(&server, "w", &["sleep", "1017"])?;
    let goes = pane(&server, "w", &["sleep", "1018"])?;
    let _reap = reap_run(&server)?;
    server.ok(&[&format!(
        "(sequence! (rename (pane {}) \"held\") #inert)",
        fux::script::quoted(&stays)
    )])?;
    let (own, pid) = (own_process(&server, &goes)?, server_pid(&server)?);
    let names = name_files(&server);
    assert!(!names.is_empty(), "{names:?}");
    // Stopped: what follows it does not see, nor record.
    signal(pid, Signal::Stop);
    let _resume = Resumed(pid);
    signal(own, Signal::Kill);
    eventually("the pane's process gone", || Ok(!alive(own)))?;
    clean_beside(&server)?;
    let record = kept(&server).join("run");
    let before = std::fs::read(&record).map_err(e)?;
    let said = refused_start(&server)?;
    assert!(said.contains("another fux server"), "{said}");
    assert_eq!(std::fs::read(&record).map_err(e)?, before, "its record");
    assert_eq!(name_files(&server), names, "its names' files");
    signal(pid, Signal::Cont);
    signal(pid, Signal::Term);
    eventually("the first server gone", || Ok(!alive(pid)))
}

/// 29c. A server that took a run back and is stopped before a pane has
///     answered it (the pane's process is stopped) records the run's end
///     all the same (D-174): the record says the run ended, and stays,
///     its pane not known to have been told.
#[test]
fn a_server_stopped_before_a_pane_answered_records_its_runs_end() -> Outcome {
    let server = Server::start("")?;
    let sleeper = pane(&server, "w", &["sleep", "1019"])?;
    let _reap = reap_run(&server)?;
    let own = own_process(&server, &sleeper)?;
    signal(own, Signal::Stop);
    let _resume = Resumed(own);
    crash(&server)?;
    let taken = within(&server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    let serving = serving(&server)?;
    signal(serving, Signal::Term);
    eventually("the second server gone", || Ok(!alive(serving)))?;
    let record = fux_server::record::read(&kept(&server).join("run"))?.ok_or("no record")?;
    assert!(record.ended, "the run's end is recorded");
    Ok(())
}

/// A process a test stopped, let go on as the test ends.
struct Resumed(i32);

impl Drop for Resumed {
    fn drop(&mut self) {
        signal(self.0, Signal::Cont);
    }
}

/// 30. What is beside the socket where a run keeps its panes and is no
///     directory is said to be that, with its path and what it is; it is
///     not read as a run that can't be taken back, and not replaced
///     (D-171).
#[test]
fn a_file_where_a_run_keeps_its_panes_is_refused_as_one() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["(sequence! (kill-server) #inert)"])?;
    let kept = kept(&server);
    eventually("the run's directory gone", || Ok(!kept.exists()))?;
    std::fs::write(&kept, b"").map_err(e)?;
    let refused = within(&server, 30, &["(sequence! (start) #inert)"])?;
    assert_ne!(refused.status, 0, "{}", refused.stdout);
    let said = &refused.stderr;
    assert!(
        said.contains(&format!(
            "{} is a regular file, not a directory",
            kept.display()
        )),
        "{said}"
    );
    assert!(!said.contains("its panes wait"), "{said}");
    assert!(kept.is_file(), "the file was replaced");
    std::fs::remove_file(&kept).map_err(e)
}

/// 31. A server with too few descriptors for the run's panes and a client
///     refuses to take the run back, saying how
///     many it takes and how many it could open, before it takes any pane:
///     a server with room then takes all of it (D-173; it took the run,
///     said so, and served no one).
#[test]
fn a_takeover_with_too_few_descriptors_is_refused() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    for _ in 0..12 {
        pane(server, "w", &["sleep", "1017"])?;
    }
    let panes = server.panes()?.len();
    let _reap = reap_run(server)?;
    crash(server)?;
    let limited = server
        .command("sh")
        .args([
            "-c",
            "ulimit -n 20; exec \"$0\" '(sequence! (start) #inert)'",
        ])
        .arg(fux())
        .output()
        .map_err(e)?;
    let said = String::from_utf8_lossy(&limited.stderr).into_owned();
    assert!(!limited.status.success(), "{said}");
    assert!(
        said.contains(&format!("has {panes} panes"))
            && said.contains(&format!("takes {} more descriptors", panes + 5))
            && said.contains("could open only"),
        "{said}"
    );
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    let count = server.ok(&["(length (panes (all)))"])?;
    assert_eq!(count.trim(), panes.to_string());
    Ok(())
}

/// 32. A run whose end was recorded and whose server crashed before it
///     told its panes and let go of what it kept: the next `start` ends
///     those pane processes and serves, a new run with no pane (D-174).
///     Said by its record, or, where that could not be written, by the
///     file beside it. The state is made, not raced: a crashed run's
///     record marked ended, or the mark put beside it.
#[test]
fn a_run_that_ended_as_its_server_crashed_is_finished_by_the_next_start() -> Outcome {
    for said in ["record", "mark"] {
        let started = Server::start("")?;
        let _stop = Stop(&started);
        let server = &started;
        let sleeper = pane(server, "w", &["sleep", "1018"])?;
        let _reap = reap_run(server)?;
        let (own, program) = (
            own_process(server, &sleeper)?,
            program_pid(server, &sleeper)?,
        );
        crash(server)?;
        match said {
            "record" => rerecord(server, |record| record.ended = true)?,
            _ => std::fs::write(kept(server).join(fux_server::record::ENDED), b"").map_err(e)?,
        }
        let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
        assert_eq!(taken.status, 0, "{said}: {}", taken.stderr);
        eventually("the ended run's pane process and program gone", || {
            Ok(!alive(own) && !alive(program))
        })?;
        assert_eq!(
            server.ok(&["(length (panes (all)))"])?.trim(),
            "0",
            "{said}"
        );
        // The new run is recorded, and the old one's end forgotten only
        // then: a crash between would have found the end still marked.
        let kept = kept(server);
        assert!(
            kept.join("run").exists() && !kept.join(fux_server::record::ENDED).exists(),
            "{said}: the new run's record, and no mark of the old one's end"
        );
        pane(server, "w", &["sleep", "1019"])?;
    }
    Ok(())
}

/// A process of `server`'s run's fux as a pane's own, at pane `pane`'s
/// socket, that no server said a first word to: as a server killed
/// between starting it and beginning it leaves it. Its pid.
fn unbegun(server: &Server, pane: &str) -> Result<i32, String> {
    use std::os::unix::net::{UnixListener, UnixStream};
    let socket = kept(server).join(pane.trim_start_matches('%'));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).map_err(e)?;
    // The link of the server that died before it said its first word.
    let dead = UnixStream::connect(&socket).map_err(e)?;
    let child = pane_process(server, listener, &socket)?;
    drop(dead);
    i32::try_from(child.id()).map_err(e)
}

/// A process of `server`'s run's fux as a pane's own, started as its
/// server starts it: `listener`, bound at `socket`, on its stdin, and told
/// which file that is.
fn pane_process(
    server: &Server,
    listener: std::os::unix::net::UnixListener,
    socket: &std::path::Path,
) -> Result<std::process::Child, String> {
    pane_process_bound(
        server,
        listener,
        &std::fs::symlink_metadata(socket).map_err(e)?,
    )
}

/// `pane_process`, told its socket's file is `bound`.
fn pane_process_bound(
    server: &Server,
    listener: std::os::unix::net::UnixListener,
    bound: &std::fs::Metadata,
) -> Result<std::process::Child, String> {
    use std::os::unix::fs::MetadataExt;
    std::process::Command::new(kept(server).join("exe"))
        .arg("__pane")
        .args([bound.dev().to_string(), bound.ino().to_string()])
        .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
            listener,
        )))
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(e)
}

/// A crashed run with pane `pane`'s own process and program killed: what
/// is left where its process was is the test's to put there.
fn vacate(server: &Server, pane: &str) -> Outcome {
    let (own, program) = (own_process(server, pane)?, program_pid(server, pane)?);
    crash(server)?;
    signal(own, Signal::Kill);
    signal(program, Signal::Kill);
    eventually("its first process gone", || {
        Ok(!alive(own) && !alive(program))
    })
}

/// 33. A pane never begun whose process reads nothing (stopped) as its run
///     is taken back and then ends: told to close after it was asked to be
///     taken, it ends once it reads both; it does not answer the first and
///     drop the second (D-171, D-174).
#[test]
fn a_pane_never_begun_ends_with_its_run_once_it_reads() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let pane_id = pane(server, "w", &["sleep", "1020"])?;
    let mut reap = reap_run(server)?;
    vacate(server, &pane_id)?;
    let fresh = unbegun(server, &pane_id)?;
    reap.0.push(fresh);
    signal(fresh, Signal::Stop);
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    let ending = serving(server)?;
    let stop = within(server, 30, &["(sequence! (kill-server) #inert)"])?;
    assert_eq!(stop.status, 0, "{}", stop.stderr);
    eventually("the server that ended the run gone", || Ok(!alive(ending)))?;
    assert!(alive(fresh), "the stopped process ended unread");
    signal(fresh, Signal::Cont);
    eventually("the pane never begun ended with its run", || {
        Ok(!alive(fresh))
    })
}

/// 34. Beginning a pane never begun waits for nothing: with what listens
///     at its socket saying it has not begun and then reading no more (a
///     process stopped just then; here the test itself, as that process),
///     the server sends its start and goes on serving the other panes;
///     what is asked of that pane waits (D-171; the server waited for its
///     answer, and served no one).
#[test]
fn beginning_a_pane_never_begun_holds_up_no_one() -> Outcome {
    use fux_server::link::{End, FromPane, Taking, ToPane};
    use std::os::unix::net::UnixListener;
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let beside = pane(server, "w", &["sleep", "1021"])?;
    let faked = pane(server, "w", &["sleep", "1022"])?;
    let _reap = reap_run(server)?;
    vacate(server, &faked)?;
    let socket = kept(server).join(faked.trim_start_matches('%'));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).map_err(e)?;
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    let (stream, _) = listener.accept().map_err(e)?;
    let mut end = End::new(stream).map_err(e)?;
    let asked = end.wait::<ToPane>().map_err(e)?;
    assert!(matches!(asked, Some(ToPane::Take { .. })), "{asked:?}");
    end.send(&FromPane::Taking(Taking::Unbegun {
        pid: i32::try_from(std::process::id()).map_err(e)?,
    }));
    end.flush_all().map_err(e)?;
    // Its start comes on the same link, and is never answered.
    let begin = end.wait::<ToPane>().map_err(e)?;
    assert!(matches!(begin, Some(ToPane::Begin(_))), "{begin:?}");
    let mut waits = capturing(server, &faked)?;
    let rest = capture(server, &beside)?;
    assert_eq!(rest.status, 0, "{}", rest.stderr);
    assert!(waits.try_wait().map_err(e)?.is_none(), "it did not wait");
    // The process there goes without beginning: the pane is lost, and
    // what waited for it is told so.
    drop(end);
    drop(listener);
    let _ = std::fs::remove_file(&socket);
    let answered = waits.wait_with_output().map_err(e)?;
    assert!(
        !answered.status.success(),
        "a pane with no process answered"
    );
    Ok(())
}

/// Connections left untaken at `socket` till it has room for no more (its
/// backlog is full), to hold for as long as it is to stay so.
#[cfg(target_os = "linux")]
fn fill(socket: &std::path::Path) -> Result<Vec<std::os::fd::OwnedFd>, String> {
    let mut queued = Vec::new();
    while queued.len() < 100_000 {
        let fd = fuxix::socket::stream().map_err(e)?;
        fuxix::io::set_nonblocking(&fd, true).map_err(e)?;
        match fuxix::socket::connect(&fd, socket) {
            Ok(()) => queued.push(fd),
            Err(fuxix::Errno::AGAIN) => return Ok(queued),
            Err(error) => return Err(e(error)),
        }
    }
    Err("its backlog never filled".to_owned())
}

/// 35. A pane's process with no room at its socket (stopped, its backlog
///     full) can't be told its run ended: what the run kept stays, its
///     record saying it ended, and the next start tells the pane once it
///     has room; its socket is not removed before, and no process is left
///     that nothing reaches (D-174; it was dropped untold, and its socket
///     deleted). Linux: macOS refuses a full backlog as it does no
///     listener.
#[cfg(target_os = "linux")]
#[test]
fn a_pane_with_no_room_at_its_socket_is_told_its_run_ended_once_it_has() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let full = pane(server, "w", &["sleep", "1023"])?;
    let _reap = reap_run(server)?;
    let (own, program) = (own_process(server, &full)?, program_pid(server, &full)?);
    let socket = kept(server).join(full.trim_start_matches('%'));
    signal(own, Signal::Stop);
    let queued = fill(&socket)?;
    crash(server)?;
    // Taken back, the run ends: by `kill-server`, then by a start that
    // finds it ended, each while the pane has no room.
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    let pid = serving(server)?;
    let stop = within(server, 30, &["(sequence! (kill-server) #inert)"])?;
    assert_eq!(stop.status, 0, "{}", stop.stderr);
    eventually("the server that ended the run gone", || Ok(!alive(pid)))?;
    assert!(kept(server).join("run").exists(), "its record went untold");
    let again = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(again.status, 0, "{}", again.stderr);
    assert_eq!(server.ok(&["(length (panes (all)))"])?.trim(), "0");
    assert!(socket.exists(), "its socket went before it was told");
    assert!(alive(own) && alive(program), "it ended untold");
    // With room again, it is reached, told, and ends.
    drop(queued);
    signal(own, Signal::Cont);
    eventually("the pane told once it had room, and ended", || {
        Ok(!alive(own) && !alive(program))
    })
}

/// The pid of the server that serves `server`'s socket now: the one that
/// took its run back, which is no child of the test's.
fn serving(server: &Server) -> Result<i32, String> {
    // Where the system names no socket's peer (macOS), `status` has no
    // such field: the one process that holds the socket is the server, as
    // the cookbook finds it there (`lsof -t SOCKET`).
    let status = server.fux(&["(get (status) \"server-process-id\")"])?;
    let pid = match status.status {
        0 => status.stdout,
        _ => {
            let listed = std::process::Command::new("lsof")
                .arg("-t")
                .arg(&server.socket)
                .output()
                .map_err(e)?;
            String::from_utf8_lossy(&listed.stdout).into_owned()
        }
    };
    let first = pid.lines().next().unwrap_or_default().trim();
    first.parse().map_err(|_| format!("its server: {pid:?}"))
}

/// A pane's number.
fn number(pane: &str) -> Result<u32, String> {
    pane.trim_start_matches('%').parse().map_err(e)
}

/// 36. A run begun where another ended, with a pane process of that one
///     still to end (stopped: told, and not heard to close): the new run's
///     panes are numbered past it, so none is ever at its path; making one
///     waits for nothing; and once the old process reads and ends, the new
///     run and its server go on (D-174; the new run's first pane took the
///     old one's number and path: making it froze the server while the old
///     process was stopped, and the close meant for the old one then
///     reached the new, whose answer made the server leave its own run).
#[test]
fn a_new_runs_panes_are_numbered_past_a_pane_still_to_end() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    pane(server, "w", &["sleep", "1024"])?;
    let (old, old_program) = server.newest_pane()?;
    let _reap = reap_run(server)?;
    let old_own = parent(old_program)?;
    let old_socket = kept(server).join(old.trim_start_matches('%'));
    signal(old_own, Signal::Stop);
    crash(server)?;
    rerecord(server, |record| record.ended = true)?;
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    let serves = serving(server)?;
    // Made while the old process is stopped: the server answers.
    let made = within(
        server,
        30,
        &[&format!(
            "(new-tab {})",
            program("(size (in (options) \"w\") 80 24)", &["sleep", "1025"])
        )],
    )?;
    assert_eq!(made.status, 0, "{}", made.stderr);
    let new = id(&made.stdout);
    assert!(
        number(&new)? > number(&old)?,
        "{new} is not past {old}, which is still to end"
    );
    assert!(old_socket.exists(), "the old pane's socket went untold");
    let _reap_new = reap_run(server)?;
    let new_own = own_process(server, &new)?;
    signal(old_own, Signal::Cont);
    eventually("the old run's pane gone", || Ok(!alive(old_own)))?;
    // Its own run goes on, under the same server, and is taken back whole.
    assert_eq!(capture(server, &new)?.status, 0);
    assert_eq!(serving(server)?, serves, "another server serves it now");
    signal(serves, Signal::Kill);
    eventually("the server gone", || Ok(!alive(serves)))?;
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    assert_eq!(capture(server, &new)?.status, 0);
    assert_eq!(own_process(server, &new)?, new_own);
    Ok(())
}

/// 38. A pane's process removes, as it ends, only the socket file its
///     server bound for it: with another file at its path before it ever
///     ran (its run's directory made anew meanwhile), it leaves that one
///     (D-174; it took whatever was at its path as it began for its own).
///     Each is told to close before it runs, and ends as it reads that.
#[test]
fn a_pane_removes_only_the_socket_file_its_server_bound() -> Outcome {
    use fux_server::link::{End, ToPane};
    use std::os::unix::net::{UnixListener, UnixStream};
    let server = Server::start("")?;
    for replaced in [false, true] {
        let socket = server.dir.join(format!("own-{replaced}"));
        let listener = UnixListener::bind(&socket).map_err(e)?;
        let mut end = End::new(UnixStream::connect(&socket).map_err(e)?).map_err(e)?;
        end.send(&ToPane::Close { closed: true });
        end.flush_all().map_err(e)?;
        let bound = std::fs::symlink_metadata(&socket).map_err(e)?;
        if replaced {
            std::fs::remove_file(&socket).map_err(e)?;
            std::fs::write(&socket, b"another's").map_err(e)?;
        }
        let mut child = pane_process_bound(&server, listener, &bound)?;
        let mut reap = Reap::default();
        reap.0.push(i32::try_from(child.id()).map_err(e)?);
        eventually("the pane told to close ended", || {
            Ok(child.try_wait().map_err(e)?.is_some())
        })?;
        assert_eq!(socket.exists(), replaced, "replaced: {replaced}");
    }
    Ok(())
}

/// 39. A pane closed by its user while it begins from its run's record is
///     told to close once it has begun: it is not linked as a pane no tab
///     has (D-171). The test is what listens at its socket: it says it has
///     not begun, takes its start, and only after the pane is closed has a
///     real pane's process begin as told, passing on what that one says.
#[test]
fn a_pane_closed_while_it_begins_is_told_to_close() -> Outcome {
    use fux_server::link::{End, FromPane, Taking, ToPane};
    use std::os::unix::net::{UnixListener, UnixStream};
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let faked = pane(server, "w", &["sleep", "1026"])?;
    let mut reap = reap_run(server)?;
    vacate(server, &faked)?;
    let socket = kept(server).join(faked.trim_start_matches('%'));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).map_err(e)?;
    let taken = within(server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(taken.status, 0, "{}", taken.stderr);
    let (stream, _) = listener.accept().map_err(e)?;
    let mut end = End::new(stream).map_err(e)?;
    let asked = end.wait::<ToPane>().map_err(e)?;
    assert!(matches!(asked, Some(ToPane::Take { .. })), "{asked:?}");
    end.send(&FromPane::Taking(Taking::Unbegun {
        pid: i32::try_from(std::process::id()).map_err(e)?,
    }));
    end.flush_all().map_err(e)?;
    let Some(ToPane::Begin(begin)) = end.wait::<ToPane>().map_err(e)? else {
        return Err("its start did not come".to_owned());
    };
    let closed = within(
        server,
        30,
        &[&format!(
            "(sequence! (kill (pane {})) #inert)",
            fux::script::quoted(&faked)
        )],
    )?;
    assert_eq!(closed.status, 0, "{}", closed.stderr);
    // It begins all the same, as a process stopped till now would.
    let real_socket = server.dir.join("real");
    let real_listener = UnixListener::bind(&real_socket).map_err(e)?;
    let mut real = pane_process(server, real_listener, &real_socket)?;
    reap.0.push(i32::try_from(real.id()).map_err(e)?);
    let mut to_real = End::new(UnixStream::connect(&real_socket).map_err(e)?).map_err(e)?;
    to_real.send(&ToPane::Begin(begin));
    to_real.flush_all().map_err(e)?;
    loop {
        let said = to_real.wait::<FromPane>().map_err(e)?;
        let said = said.ok_or("the pane's process went before its first facts")?;
        let facts = matches!(said, FromPane::Update(_));
        end.send(&said);
        end.flush_all().map_err(e)?;
        if facts {
            break;
        }
    }
    // Begun, and no pane of the run: told to close on that link.
    loop {
        match end.wait::<ToPane>().map_err(e)? {
            Some(ToPane::Close { .. }) => break,
            Some(_) => {}
            None => return Err("its link closed with no close said".to_owned()),
        }
    }
    to_real.send(&ToPane::Close { closed: true });
    to_real.flush_all().map_err(e)?;
    eventually("the pane's process ended", || {
        Ok(real.try_wait().map_err(e)?.is_some())
    })
}

/// 37. A stopping server keeps its run's lock till it is gone: while it
///     waits for a pane's process to close (here one that is stopped),
///     its socket gone already, no other server has the run, and a
///     `start` then waits its turn and begins a run of its own, which it
///     serves (D-180; the lock was let go with the socket, and a server
///     started in between found the old panes held by one still alive,
///     and left its own new run).
#[test]
fn a_stopping_server_keeps_its_runs_lock_till_it_is_gone() -> Outcome {
    let server = Server::start("")?;
    let sleeper = pane(&server, "w", &["sleep", "1037"])?;
    let _reap = reap_run(&server)?;
    let own = own_process(&server, &sleeper)?;
    let pid = server_pid(&server)?;
    let lock = std::fs::File::open(kept(&server)).map_err(e)?;
    signal(own, Signal::Stop);
    let _resume = Resumed(own);
    signal(pid, Signal::Term);
    eventually("the first server's socket gone", || {
        Ok(!server.socket.exists())
    })?;
    let free = lock.try_lock().is_ok();
    assert!(
        alive(pid),
        "the first server ended before its lock was asked"
    );
    assert!(!free, "a stopping server's run was free to take");
    let start = within(&server, 30, &["(sequence! (start) #inert)"])?;
    assert_eq!(start.status, 0, "{}", start.stderr);
    assert!(!alive(pid), "the start did not wait for the first server");
    let second = serving(&server)?;
    assert_ne!(second, pid);
    assert_eq!(server.ok(&["(length (panes (all)))"])?.trim(), "0");
    assert!(alive(second), "the second server left its run");
    server.ok(&["(sequence! (kill-server) #inert)"])?;
    Ok(())
}

/// The servers started on `server`'s socket (`fux __serve SOCKET …`) that
/// wait for a lock, as the system lists them (`/proc/locks`).
#[cfg(target_os = "linux")]
fn waiting_for_a_lock(server: &Server) -> Vec<i32> {
    let socket = server.socket.to_string_lossy().into_owned();
    let starting: Vec<i32> = (processes().into_iter())
        .filter(|(_, _, args)| args.contains("__serve") && args.contains(&socket))
        .map(|(pid, ..)| pid)
        .collect();
    let locks = std::fs::read_to_string("/proc/locks").unwrap_or_default();
    (locks.lines())
        .filter(|line| line.contains("->"))
        .filter_map(|line| line.split_whitespace().find_map(|word| word.parse().ok()))
        .filter(|pid| starting.contains(pid))
        .collect()
}

/// 40. A start held up behind another reads what is beside the socket
///     again once it has the socket's locks: a run found ended before
///     them, and dealt with since by the server ahead (finished, a new run
///     begun and recorded, its mark removed, that server then crashed), is
///     not taken for the old one, and its panes are taken back, not ended
///     (D-174; the mark read before the locks was acted on after them).
///     The test is the server ahead: it holds the lock a starting server
///     waits for, and changes what is there while the start waits.
///     Linux: it is where the system says who waits for a lock.
#[cfg(target_os = "linux")]
#[test]
fn a_start_held_up_reads_the_run_again_under_the_locks() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let sleeper = pane(server, "w", &["sleep", "1027"])?;
    let _reap = reap_run(server)?;
    let (own, program) = (
        own_process(server, &sleeper)?,
        program_pid(server, &sleeper)?,
    );
    crash(server)?;
    let mark = kept(server).join(fux_server::record::ENDED);
    std::fs::write(&mark, b"").map_err(e)?;
    let mut path = server.socket.clone().into_os_string();
    path.push(".starting");
    let ahead = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(e)?;
    ahead.lock().map_err(e)?;
    let start = server
        .command(fux())
        .arg("(sequence! (start) #inert)")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(e)?;
    eventually("the start waiting for the lock", || {
        Ok(!waiting_for_a_lock(server).is_empty())
    })?;
    // The server ahead is done: the run there is a live one's, recorded,
    // with its pane, and no mark of an end.
    std::fs::remove_file(&mark).map_err(e)?;
    drop(ahead);
    let done = start.wait_with_output().map_err(e)?;
    let said = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{said}");
    assert_eq!(capture(server, &sleeper)?.status, 0);
    assert_eq!(own_process(server, &sleeper)?, own);
    assert!(alive(program), "its program was ended");
    Ok(())
}

/// 27. A server killed while a paste is on its way: the paste's sender,
///     which still holds what its pane has not taken, is told its server
///     ended and how the run is taken back, and fails; `start` then takes
///     the run back, its program still running (D-181; the sender had
///     been answered at once, its paste queued in the pane's process).
#[cfg(target_os = "linux")]
#[test]
fn a_server_killed_mid_paste_tells_its_sender_and_its_run_is_taken_back() -> Outcome {
    use std::io::Write;
    let run = Server::start("")?;
    let _stop = Stop(&run);
    let server = &run;
    let sleeper = pane(server, "a", &["sleep", "1000"])?;
    let own = own_process(server, &sleeper)?;
    let program = program_pid(server, &sleeper)?;
    let links = |pid: i32| std::fs::read_dir(format!("/proc/{pid}/fd")).map(Iterator::count);
    let before = links(own).map_err(e)?;
    let mut sending = started(
        server,
        &[&format!(
            "(send-keys (paste {} (input)))",
            support::reference(&sleeper)
        )],
    )?;
    let mut stdin = sending.stdin.take().ok_or("its stdin")?;
    stdin.write_all(&vec![b'x'; 32 << 20]).map_err(e)?;
    drop(stdin);
    // Its connection reached the pane with a piece, which the pane's
    // program, reading nothing, leaves the rest behind.
    eventually("a piece on its way", || Ok(links(own).map_err(e)? > before))?;
    assert!(sending.try_wait().map_err(e)?.is_none(), "it waits");
    let pid = server_pid(server)?;
    signal(pid, Signal::Kill);
    let out = sending.wait_with_output().map_err(e)?;
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{said}");
    assert!(
        said.contains("ended without a word") && said.contains("(start)"),
        "{said}"
    );
    server.ok(&["(sequence! (start) #inert)"])?;
    server.version(&sleeper)?;
    assert!(alive(program), "its program ended with the server");
    assert_eq!(own_process(server, &sleeper)?, own);
    Ok(())
}

/// 28. A program that writes 10 MB before it reads, a megabyte sent it
///     meanwhile, gets all of it: what waits to be written to it never
///     stops its output being read, nor the other way (D-183; a terminal
///     that stops one for the other deadlocks here).
#[cfg(target_os = "linux")]
#[test]
fn a_program_that_writes_ten_megabytes_before_it_reads_gets_all_it_was_sent() -> Outcome {
    use std::io::Write;
    const SENT: usize = 1 << 20;
    let server = Server::start("")?;
    let got = server.dir.join("got");
    // Raw, so that what it is sent is neither a line nor echoed; it says
    // so, reads one byte (the go), writes, and only then reads the rest.
    let script = format!(
        "stty raw -echo; printf RAW-%s NOW; dd bs=1 count=1 >/dev/null 2>&1; \
         head -c 10000000 /dev/zero | tr '\\0' x; printf WROTE-%s ALL; \
         head -c {SENT} | wc -c > {part}; mv {part} {got}; sleep 1000",
        part = server.dir.join("part").display(),
        got = got.display()
    );
    let writer = pane(&server, "a", &["sh", "-c", &script])?;
    server.shows(&writer, "RAW-NOW")?;
    let mut sending = started(
        &server,
        &[&format!(
            "(send-keys (paste {} (input)))",
            support::reference(&writer)
        )],
    )?;
    let mut stdin = sending.stdin.take().ok_or("its stdin")?;
    stdin.write_all(&vec![b'y'; SENT + 1]).map_err(e)?;
    drop(stdin);
    let mut kept = String::new();
    for _ in 0..12 {
        let read = eventually("all it was sent, read", || {
            kept = std::fs::read_to_string(&got).unwrap_or_default();
            Ok(!kept.is_empty())
        });
        if read.is_ok() {
            break;
        }
    }
    assert_eq!(kept.trim(), SENT.to_string());
    assert!(sending.wait().map_err(e)?.success(), "its paste failed");
    assert!(
        capture(&server, &writer)?.stdout.contains("WROTE-ALL"),
        "its output was not read to its end"
    );
    Ok(())
}

/// 30. A client answers a question while a paste of its own waits: the
///     first byte of a 2 MiB paste sets its pane's program to ask the
///     client that pastes it what keys it has (`keys`), before it reads the
///     rest. The client, which sends each further piece only as it is asked
///     for, is free to answer, and its connection reads the answer though
///     the paste is still on its way; then the program reads all of it
///     (D-181; the client sat in the write of the whole paste, its
///     connection read nothing of its client's, and all three waited for
///     ever).
#[cfg(target_os = "linux")]
#[test]
fn a_client_answers_a_question_while_its_own_paste_waits() -> Outcome {
    const SENT: usize = 2 << 20;
    let server = Server::start("")?;
    let mut terminal = Terminal::attach(&server, 24, 80, &[])?;
    terminal.wait_for("$")?;
    let listing = server.ok(&["(listing (list-session))"])?;
    let client: String = (listing.split("client ").nth(1))
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect()
        })
        .ok_or("its client")?;
    let (asked, got) = (server.dir.join("asked"), server.dir.join("got"));
    let script = format!(
        "stty raw -echo; printf RAW-%s NOW; dd bs=1 count=1 >/dev/null 2>&1; \
         \"$FUX\" '(keys (client \"{client}\"))' > {asked}.part 2>&1; mv {asked}.part {asked}; \
         head -c {rest} | wc -c > {got}.part; mv {got}.part {got}; stty sane\n",
        asked = asked.display(),
        got = got.display(),
        rest = SENT - 1
    );
    let run = server.dir.join("run.sh");
    std::fs::write(&run, script).map_err(e)?;
    terminal.type_bytes(format!("sh {}\r", run.display()).as_bytes())?;
    terminal.wait_for("RAW-NOW")?;
    let mut paste = b"\x1b[200~".to_vec();
    paste.resize(paste.len() + SENT, b'y');
    paste.extend_from_slice(b"\x1b[201~");
    terminal.type_bytes(&paste)?;
    eventually("its answer, while its paste waited", || {
        terminal.pump();
        Ok(asked.exists())
    })?;
    let mut read = String::new();
    for _ in 0..6 {
        let all = eventually("all it pasted, read", || {
            terminal.pump();
            read = std::fs::read_to_string(&got).unwrap_or_default();
            Ok(!read.is_empty())
        });
        if all.is_ok() {
            break;
        }
    }
    assert_eq!(read.trim(), (SENT - 1).to_string());
    Ok(())
}

/// 31. Two pastes of several pieces, sent at once to the same two panes,
///     both arrive whole at both: each pane takes the bodies in the order
///     its server asked for them, so no pane begins one while the other
///     begins the other, each sender then waiting for ever on the pane
///     that held its piece back (D-192).
#[cfg(target_os = "linux")]
#[test]
fn two_pastes_sent_at_once_to_the_same_two_panes_both_arrive() -> Outcome {
    use std::io::Write;
    const SENT: usize = 3 << 20;
    let server = Server::start("")?;
    let reader = |name: &str| {
        let got = server.dir.join(name);
        let script = format!(
            "stty raw -echo; printf RAW-%s NOW; head -c {all} | wc -c > {got}.part; \
             mv {got}.part {got}; sleep 1000",
            all = SENT * 2,
            got = got.display()
        );
        (got, script)
    };
    let (got_a, script_a) = reader("got-a");
    let (got_b, script_b) = reader("got-b");
    let a = pane(&server, "a", &["sh", "-c", &script_a])?;
    let b = id(&server.ok(&[format!(
        "(split {})",
        program(
            &format!("(axis (pane {}) \"horizontal\")", fux::script::quoted(&a)),
            &["sh", "-c", &script_b]
        )
    )])?);
    for pane in [&a, &b] {
        server.shows(pane, "RAW-NOW")?;
    }
    let tab = server.ok(&[&format!(
        "(sequence! (define! its-pane (pane {})) \"tab\" (get its-pane \"tab\"))",
        fux::script::quoted(&a)
    )])?;
    let paste = format!(
        "(send-keys (paste (tab {}) (input)))",
        fux::script::quoted(tab.trim())
    );
    let mut senders = Vec::new();
    for byte in *b"xy" {
        let mut sending = started(&server, &[&paste])?;
        let mut stdin = sending.stdin.take().ok_or("its stdin")?;
        stdin.write_all(&vec![byte; SENT]).map_err(e)?;
        drop(stdin);
        senders.push(sending);
    }
    let mut read = (String::new(), String::new());
    for _ in 0..6 {
        let all = eventually("both pastes read by both panes", || {
            read = (
                std::fs::read_to_string(&got_a).unwrap_or_default(),
                std::fs::read_to_string(&got_b).unwrap_or_default(),
            );
            Ok(!read.0.is_empty() && !read.1.is_empty())
        });
        if all.is_ok() {
            break;
        }
    }
    let all = (SENT * 2).to_string();
    assert_eq!((read.0.trim(), read.1.trim()), (all.as_str(), all.as_str()));
    for mut sending in senders {
        let out = sending.wait().map_err(e)?;
        assert!(out.success(), "a paste failed");
    }
    Ok(())
}

/// 32. Megabytes typed at a program that isn't reading (a terminal that
///     pastes as plain keys) queue nowhere: a frame of what was typed is
///     the most its pane holds, the keys past it are not taken, and their
///     typist is told so; neither the pane's process, the server nor the
///     connection's grows with them, and the client's bindings run all
///     the while (D-193; all of it queued in the pane's process).
#[cfg(target_os = "linux")]
#[test]
fn keys_typed_past_a_frame_at_a_program_that_reads_nothing_are_not_taken() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(24, 100)?;
    client.ready()?;
    let pane = support::focused(&server)?;
    server.type_line(&pane, "stty raw -echo; printf RAW-%s NOW; sleep 1000")?;
    server.shows(&pane, "RAW-NOW")?;
    let own = own_process(&server, &pane)?;
    flooded_with_keys(&server, &mut client, own)
}

/// 32a. The same of a pane whose own process is stopped, and takes
///      nothing of what it is sent: a frame of what was typed is the most
///      the server keeps for it, the keys past it are not taken, and their
///      typist is told so (D-193; its client's frames were left unread,
///      and with them its answers and its detach).
#[cfg(target_os = "linux")]
#[test]
fn keys_typed_past_a_frame_at_a_stopped_pane_are_not_taken() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(24, 100)?;
    client.ready()?;
    let pane = support::focused(&server)?;
    let own = own_process(&server, &pane)?;
    signal(own, Signal::Stop);
    let flooded = flooded_with_keys(&server, &mut client, own);
    signal(own, Signal::Cont);
    flooded
}

/// 8 MiB of keys typed by `client` at the pane it focuses, whose process
/// is `own`, none of them read: no fux process grows with them, the client
/// is told they are not taken, and its detach binding then runs.
#[cfg(target_os = "linux")]
fn flooded_with_keys(server: &Server, client: &mut Client, own: i32) -> Outcome {
    use fux_wire::act::Act;
    use fux_wire::protocol::{AttachedFrame, Frame};
    const KEYS: usize = 8 << 20;
    let pid = server_pid(server)?;
    let conns = connections(pid);
    let before = (peak_kib(own)?, peak_kib(pid)?);
    let key = AttachedFrame::Act(Box::new(Act::Key(fux_vt::keys::KeyPress::char('k').into())));
    let frame = key.encode();
    let mut frames = Vec::new();
    for _ in 0..4096 {
        frames.extend_from_slice(&frame);
    }
    // Its connection's process, as it is read while the keys come.
    let mut held = 0u64;
    for _ in 0..KEYS / 4096 {
        client.write(&frames)?;
        // Its server counts each, taken or not: the binding below waits
        // for all of them.
        client.wrote(4096);
        for conn in &conns {
            held = held.max(peak_kib(*conn).unwrap_or(0));
        }
    }
    eventually("it is told its keys are not taken", || {
        client.pump()?;
        let text = client.text();
        Ok(text.contains("isn't reading what is typed") || text.contains("typed faster"))
    })?;
    let grew = |pid: i32, before: u64| peak_kib(pid).map(|peak| peak.saturating_sub(before));
    let (in_pane, served) = (grew(own, before.0)?, grew(pid, before.1)?);
    assert!(in_pane < 4 << 10, "its pane's process grew {in_pane} KiB");
    // A frame of what was typed may wait in the server for a pane's
    // process that takes none, with what its buffer grew by: far from the
    // hundred megabytes these keys are as frames.
    assert!(served < 8 << 10, "the server grew {served} KiB");
    assert!(held < 24 << 10, "a connection's process held {held} KiB");
    // `C-b d`: its bindings run whatever its pane takes.
    client.send(b"\x02d")?;
    eventually("it detached", || {
        client.pump()?;
        Ok(client.exit.as_deref() == Some("detached"))
    })
}

/// 32b. Keys typed while a paste waits for a program that isn't reading
///      land after the paste, in the order they were typed: its pane
///      queues nothing else inside a paste that is open or still to come
///      (D-187).
#[cfg(target_os = "linux")]
#[test]
fn keys_typed_while_a_paste_waits_land_after_it() -> Outcome {
    const PASTED: usize = 200_000;
    let server = Server::start("")?;
    let mut client = server.attach(24, 100)?;
    client.ready()?;
    let pane = support::focused(&server)?;
    let (go, got) = (server.dir.join("go"), server.dir.join("got"));
    let script = format!(
        "stty raw -echo; printf RAW-%s NOW; until [ -e {go} ]; do sleep 0.05; done; \
         head -c {all} > {got}.part; mv {got}.part {got}; stty sane\n",
        go = go.display(),
        got = got.display(),
        all = PASTED + 4
    );
    let run = server.dir.join("run.sh");
    std::fs::write(&run, script).map_err(e)?;
    server.type_line(&pane, &format!("sh {}", run.display()))?;
    server.shows(&pane, "RAW-NOW")?;
    // More than its terminal takes while it reads nothing: the paste
    // waits; then four keys.
    let mut paste = b"\x1b[200~".to_vec();
    paste.resize(paste.len() + PASTED, b'p');
    paste.extend_from_slice(b"\x1b[201~");
    client.send(&paste)?;
    client.send(b"KEYS")?;
    std::fs::write(&go, b"").map_err(e)?;
    let mut read = Vec::new();
    eventually("all of it read", || {
        client.pump()?;
        read = std::fs::read(&got).unwrap_or_default();
        Ok(!read.is_empty())
    })?;
    assert_eq!(read.len(), PASTED + 4);
    assert!(
        read.ends_with(b"pppKEYS"),
        "its keys are not after its paste"
    );
    assert!(!read.get(..PASTED).unwrap_or_default().contains(&b'K'));
    Ok(())
}

/// 33. A paste to a program that isn't reading locks its client out of
///     nothing: with 2 MiB pasted into a raw-mode program that reads
///     none of it, more than its terminal takes, the client's bindings
///     run at once. It changes its focus, and what it types then goes to
///     the pane it focuses now; and it detaches (D-189; bindings waited
///     behind the paste, and the client could only be killed).
#[cfg(target_os = "linux")]
#[test]
fn a_paste_that_waits_keeps_its_client_from_no_binding() -> Outcome {
    const PASTED: usize = 2 << 20;
    let server = Server::start("")?;
    let mut terminal = Terminal::attach(&server, 24, 80, &[])?;
    terminal.wait_for("$")?;
    let stuck = support::focused(&server)?;
    let other = id(&server.ok(&[format!(
        "(split {})",
        program(
            &format!(
                "(axis (pane {}) \"horizontal\")",
                fux::script::quoted(&stuck)
            ),
            &["sh"]
        )
    )])?);
    server.shows(&other, "$")?;
    // Its focus is where it types: the program there reads nothing.
    let typed_into = support::focused(&server)?;
    let (reading, idle) = match typed_into == stuck {
        true => (&other, &stuck),
        false => (&stuck, &other),
    };
    let _ = idle;
    terminal.type_bytes(b"stty raw -echo; printf RAW-%s NOW; sleep 1000\r")?;
    server.shows(&typed_into, "RAW-NOW")?;
    let mut paste = b"\x1b[200~".to_vec();
    paste.resize(paste.len() + PASTED, b'y');
    paste.extend_from_slice(b"\x1b[201~");
    terminal.type_bytes(&paste)?;
    // A second paste, which waits its turn behind the first: nothing of it
    // is sent, and no binding waits for it.
    terminal.type_bytes(b"\x1b[200~second\x1b[201~")?;
    // `C-b o`: the next pane. What is typed now is for that one.
    let marked = server.dir.join("marked");
    terminal.type_bytes(b"\x02o")?;
    eventually("its focus on the other pane", || {
        terminal.pump();
        Ok(&support::focused(&server)? == reading)
    })?;
    terminal.type_bytes(format!("echo there > {}\r", marked.display()).as_bytes())?;
    eventually("what it typed, in the pane it focuses now", || {
        terminal.pump();
        Ok(marked.exists())
    })?;
    // `C-b d`: it detaches, its paste still waiting.
    terminal.type_bytes(b"\x02d")?;
    let status = terminal.wait_exit()?;
    assert!(status.success(), "{status:?}");
    Ok(())
}

/// 34. A program that floods its terminal with queries and reads no answer
///     stalls only itself: a client that shows its pane and focuses it is
///     kept from nothing, and its bindings run (D-183).
#[cfg(target_os = "linux")]
#[test]
fn a_flood_of_queries_keeps_a_client_that_watches_from_no_binding() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(24, 100)?;
    client.ready()?;
    let pane = support::focused(&server)?;
    let own = own_process(&server, &pane)?;
    let before = peak_kib(own)?;
    let asks = r#"stty raw -echo; q=$(printf '\033[c%.0s' $(seq 200)); yes "$q" | head -c 4000000; sleep 1000"#;
    server.type_line(&pane, asks)?;
    // A frame of answers and more waits in its pane, unread.
    eventually("a frame of answers unread in its pane", || {
        client.pump()?;
        Ok(peak_kib(own)?.saturating_sub(before) >= 1 << 10)
    })?;
    client.send(b"\x02d")?;
    eventually("it detached", || {
        client.pump()?;
        Ok(client.exit.as_deref() == Some("detached"))
    })
}

/// 36. Megabytes of text sent to a client (`send-keys` to a client, typed
///     there as keys) all reach the program it types into: the client
///     sends a long run of keys as it sends a paste's text, a piece once
///     the last is taken, so none comes faster than it is taken (D-189;
///     each was a frame of its own, and past a frame waiting its
///     connection dropped them while the request said they were sent).
#[cfg(target_os = "linux")]
#[test]
fn text_sent_to_a_client_all_reaches_the_program_it_types_into() -> Outcome {
    use std::io::Write;
    const SENT: usize = 2 << 20;
    let server = Server::start("")?;
    let (mut client, id, got) = keeping(&server)?;
    let mut sending = started(
        &server,
        &[&format!(
            "(send-keys (text (client {}) (input)))",
            fux::script::quoted(&id)
        )],
    )?;
    let mut stdin = sending.stdin.take().ok_or("its stdin")?;
    stdin.write_all(&vec![b't'; SENT]).map_err(e)?;
    drop(stdin);
    // Megabytes typed a key at a time take their time: waited for as
    // long as more of them keeps arriving.
    let whole = u64::try_from(SENT).map_err(e)?;
    let (mut kept, mut status) = (0u64, None);
    loop {
        let before = kept;
        let done = eventually("more of it kept, or the keys sent", || {
            client.pump()?;
            kept = std::fs::metadata(&got).map_err(e)?.len();
            status = status.or(sending.try_wait().map_err(e)?);
            Ok(status.is_some() && kept >= whole)
        });
        match done {
            Ok(()) => break,
            Err(_) if kept > before => {}
            Err(waited) => return Err(format!("{waited}: {kept} of {whole} kept")),
        }
    }
    assert!(status.is_some_and(|status| status.success()));
    assert_eq!(kept, u64::try_from(SENT).map_err(e)?);
    Ok(())
}

/// 37. A paste whose connection can't reach its pane's socket (here the
///     socket's name is gone) holds nothing back in that pane: the
///     connection tells its server, which told the pane to wait for the
///     paste, and the pane waits no more. What is typed after it reaches
///     the program (D-187; the pane waited for that paste for good, and
///     held every other input and answer behind it).
#[cfg(target_os = "linux")]
#[test]
fn a_paste_that_cant_reach_its_pane_holds_nothing_back_there() -> Outcome {
    let server = Server::start("")?;
    let (mut client, _, got) = keeping(&server)?;
    let pane = support::focused(&server)?;
    let socket = pane_socket(&server, &pane);
    std::fs::rename(&socket, socket.with_extension("away")).map_err(e)?;
    client.send(b"\x1b[200~lost\x1b[201~")?;
    client.send(b"KEYS")?;
    let mut read = Vec::new();
    eventually("what was typed after it read", || {
        client.pump()?;
        read = std::fs::read(&got).unwrap_or_default();
        Ok(read.ends_with(b"KEYS"))
    })?;
    assert!(!read.contains(&b'l'), "its paste was typed");
    Ok(())
}
/// 41. A start held up behind a stopping server chooses the fux it runs
///     only in its turn: the run that server ends, with the fux it ran,
///     is removed by it as it goes, and is not this start's to run. The
///     start begins a run with the fux that was started (D-180; it chose
///     the ended run's fux before its turn and ran it, and the server
///     ahead then removed that file: where the system has no other way
///     to a running program's file, macOS, the start then kept no fux and
///     failed). The test is the server ahead, as in 40.
#[cfg(target_os = "linux")]
#[test]
fn a_start_held_up_runs_no_fux_the_server_ahead_removed() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    crash(server)?;
    let kept = kept(server);
    let exe = kept.join("exe");
    std::fs::write(kept.join(fux_server::record::ENDED), b"").map_err(e)?;
    assert!(exe.exists(), "the run's fux");
    let mut path = server.socket.clone().into_os_string();
    path.push(".starting");
    let ahead = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(e)?;
    ahead.lock().map_err(e)?;
    let start = server
        .command(fux())
        .arg("(sequence! (start) #inert)")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(e)?;
    eventually("the start waiting for the lock", || {
        Ok(!waiting_for_a_lock(server).is_empty())
    })?;
    // The server ahead is done: what its run kept is gone.
    for entry in std::fs::read_dir(&kept).map_err(e)? {
        std::fs::remove_file(entry.map_err(e)?.path()).map_err(e)?;
    }
    drop(ahead);
    let done = start.wait_with_output().map_err(e)?;
    let said = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{said}");
    let pid = serving(server)?;
    let args = (processes().into_iter())
        .find_map(|(process, _, args)| (process == pid).then_some(args))
        .ok_or("the new server's process")?;
    assert!(
        !args.contains(&*exe.to_string_lossy()),
        "it runs the fux of the run that was removed: {args}"
    );
    Ok(())
}
