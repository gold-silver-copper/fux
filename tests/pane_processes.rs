//! Each pane in a process of its own, and each client connection (D-120,
//! D-123), and a run that outlives a crashed server (D-124): what one
//! process's end does to the rest.
mod support;
use fuxix::process::Signal;
use std::path::PathBuf;
use support::*;

/// `argv` as a pane's program: its words for `program` and `arg`.
fn program<'a>(argv: &[&'a str]) -> Vec<&'a str> {
    let mut words = Vec::new();
    let mut rest = argv.iter();
    if let Some(first) = rest.next() {
        words.extend(["str", first, "program"]);
    }
    for word in rest {
        words.extend(["str", word, "arg"]);
    }
    words
}

/// A pane running `argv` in a tab of its own, in workspace `ws`: its id.
fn pane(server: &Server, ws: &str, argv: &[&str]) -> Result<String, String> {
    let words = [
        &["opts", "str", ws, "in", "80", "24", "size"][..],
        &program(argv),
        &["new-tab"],
    ]
    .concat();
    Ok(id(&server.ok(&words)?))
}

/// The pane an action's answer names.
fn id(answer: &str) -> String {
    let first = answer.split_whitespace().next().unwrap_or_default();
    first.split(':').next().unwrap_or_default().to_owned()
}

/// The journal's end now: a cursor.
fn cursor(server: &Server) -> Result<String, String> {
    let next = ["all", "str", "", "0", "next-events", "str", "next", "get"];
    Ok(server.ok(&next)?.trim().to_owned())
}

/// The run a server serves, as its pane's version ends.
fn run(server: &Server, pane: &str) -> Result<String, String> {
    let version = server.version(pane)?;
    Ok(version.rsplit(':').next().unwrap_or_default().to_owned())
}

/// What a process's `/proc` stat says of it: its parent.
fn parent(pid: i32) -> Result<i32, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(e)?;
    let parent = stat
        .rsplit(')')
        .next()
        .and_then(|r| r.split_whitespace().nth(1));
    parent
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("{pid}'s parent"))
}

/// The pid of `pane`'s program.
fn program_pid(server: &Server, pane: &str) -> Result<i32, String> {
    let pid = server.ok(&[
        "str", pane, "pane", "str", "program", "get", "str", "pid", "get",
    ])?;
    pid.trim()
        .parse()
        .map_err(|_| format!("{pane}'s program: {pid}"))
}

/// The pid of `pane`'s own process: its program's parent.
fn own_process(server: &Server, pane: &str) -> Result<i32, String> {
    parent(program_pid(server, pane)?)
}

/// The processes whose parent is `pid` and whose argv holds `word`.
fn children(pid: i32, word: &str) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    (entries.flatten())
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter(|child| parent(*child).ok() == Some(pid))
        .filter(|child| {
            std::fs::read(format!("/proc/{child}/cmdline"))
                .is_ok_and(|argv| argv.split(|b| *b == 0).any(|arg| arg == word.as_bytes()))
        })
        .collect()
}

/// The journal's entries from `cursor` on, as JSON lines.
fn told(server: &Server, cursor: &str) -> Result<String, String> {
    server.ok(&["all", "str", cursor, "0", "next-events"])
}

/// Where the run on `server`'s socket keeps what outlives its server.
fn kept(server: &Server) -> PathBuf {
    fux_server::socket::panes(&server.socket)
}

/// Caps `pid`'s address space at `more` bytes past what it has now
/// (`prlimit`, util-linux): an allocation past it fails.
fn cap(pid: i32, more: u64) -> Outcome {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).map_err(e)?;
    let kib: u64 = (status.lines())
        .find_map(|line| line.strip_prefix("VmSize:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .ok_or("its size")?;
    let limit = (kib * 1024 + more).to_string();
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
        Ok(lines.contains(r#""event":"read_failed""#) && lines.contains(r#""event":"exited""#))
    })?;
    let lines = told(&server, &cursor)?;
    let failed = lines.find(r#""event":"read_failed""#).unwrap_or(usize::MAX);
    let exited = lines.find(r#""event":"exited""#).unwrap_or(0);
    assert!(failed < exited, "{lines}");
    assert!(
        lines.contains("its pane's own process was killed by signal 9"),
        "{lines}"
    );
    // Its program hung up as its terminal closed: its real status.
    assert!(lines.contains(r#""signal":1"#), "{lines}");
    assert!(
        server
            .pid()
            .is_some_and(|p| alive(i32::try_from(p).unwrap_or(0)))
    );
    server.type_line(&shell, "echo still-here")?;
    server.shows(&shell, "still-here")?;
    // A program's pane stays, its program ended.
    let state = server.ok(&[
        "str", &sleeper, "pane", "str", "program", "get", "str", "state", "get",
    ])?;
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
    cap(own, 256 << 20)?;
    // It asks its terminal what it is, and never reads the answers.
    let asks = r#"q=$(printf '\033[c%.0s' $(seq 200)); yes "$q""#;
    let respawn = [
        &["str", &flood, "pane"][..],
        &program(&["sh", "-c", asks]),
        &["kill-first", "respawn"],
    ]
    .concat();
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
    let conns = children(pid, "__conn");
    let [conn] = conns.as_slice() else {
        return Err(format!("its connection's process: {conns:?}"));
    };
    let conn = *conn;
    cap(conn, 64 << 20)?;
    let shell = support::focused(&server)?;
    // It no longer reads (`pump`) from here on.
    let copies = r#"while :; do printf '\033]52;c;aGk=\a'; done"#;
    server.type_line(&shell, copies)?;
    eventually("its connection's process to end", || Ok(!alive(conn)))?;
    eventually("its client gone", || {
        Ok(!server.ok(&["ls", "listing"])?.contains("client "))
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
    let stale = server.fux(&[
        "str",
        &seen,
        "expect",
        "str",
        "NOT-SENT",
        "text",
        "send-keys",
    ])?;
    assert!(stale.stderr.contains("stale: "), "{}", stale.stderr);
    let lines = told(&server, &cursor)?;
    assert!(!lines.contains(r#""event":"input""#), "{lines}");
    let capture = server.ok(&["str", &shell, "pane", "capture"])?;
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
    let ended = id(&server.ok(&[
        &["str", &shell, "pane", "str", "horizontal", "axis"][..],
        &program(&["sh", "-c", "exit 3"]),
        &["split"],
    ]
    .concat())?);
    assert_eq!(server.exits(&ended)?, "3");
    let tab = server.ok(&["str", &shell, "pane", "str", "tab", "get"])?;
    let sent = server.fux(&["str", tab.trim(), "tab", "str", "TOOK", "text", "send-keys"])?;
    assert_ne!(sent.status, 0);
    assert!(sent.stderr.contains("not_running: "), "{}", sent.stderr);
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
///    told what it lost; the attached client had said why it ended.
#[cfg(target_os = "linux")]
#[test]
fn a_crashed_servers_run_is_taken_back() -> Outcome {
    let started = Server::start("")?;
    let _stop = Stop(&started);
    let server = &started;
    let shell = pane(server, "work", &["sh"])?;
    server.ok(&["str", &shell, "pane", "str", "editor", "rename"])?;
    let sleeper = id(&server.ok(&[
        &["str", &shell, "pane", "str", "vertical", "axis"][..],
        &program(&["sleep", "1000"]),
        &["split"],
    ]
    .concat())?);
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
    assert!(said.contains("ended without stopping"), "{said}");
    for program in programs {
        assert!(alive(program), "{program} ended with the server");
    }
    server.ok(&["start"])?;
    assert_eq!(layout(server)?, before);
    assert_eq!(run(server, &shell)?, was);
    assert!(
        server
            .ok(&["str", &shell, "pane", "capture"])?
            .contains("MARK-BEFORE")
    );
    let lost = server.fux(&["all", "str", &cursor, "0", "next-events"])?;
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
    let line = format!(r#""$FUX" ls str run get > '{}'"#, here.display());
    server.type_line(&shell, &line)?;
    eventually("its shell's answer", || {
        Ok(std::fs::read_to_string(&here).is_ok_and(|said| said.trim() == was))
    })
}

/// Stops a server however its test ends: one that took a run back is no
/// child of the test's.
struct Stop<'a>(&'a Server);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        let _ = self.0.fux(&["kill-server"]);
    }
}

/// What `ls listing` says of a server's workspaces, tabs and panes.
fn layout(server: &Server) -> Result<String, String> {
    let listing = server.ok(&["ls", "listing"])?;
    let kept = (listing.lines()).filter(|line| {
        !["client ", "cursor "]
            .iter()
            .any(|w| line.trim_start().starts_with(w))
    });
    Ok(kept.collect::<Vec<_>>().join("\n"))
}

/// 7. `kill-server` and SIGTERM end the run: no pane's process is left,
///    nor its record.
#[cfg(target_os = "linux")]
#[test]
fn stopping_ends_the_run_and_its_pane_processes() -> Outcome {
    for stop in ["kill-server", "SIGTERM"] {
        let server = Server::start("")?;
        let sleeper = pane(&server, "a", &["sleep", "1000"])?;
        let own = own_process(&server, &sleeper)?;
        let program = program_pid(&server, &sleeper)?;
        match stop {
            "kill-server" => drop(server.ok(&["kill-server"])?),
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
#[cfg(target_os = "linux")]
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
        let refused = server.fux(&["start"])?;
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
