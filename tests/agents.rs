//! Driving programs from the command line, as a script or an agent does:
//! `run` starts one out of every client's way and keeps its pane once it
//! exits, `wait` answers when a pane shows something, goes quiet or exits,
//! `send-keys` and `send-mouse` send what a person would, sizes are set,
//! and a headless client stands in for a terminal.
mod support;
use support::*;

/// The id `run` (or another creating command) printed.
fn id(out: &str) -> String {
    out.trim().to_owned()
}

/// A program `run` starts keeps its pane once it exits: its last screen,
/// and its status, which `wait --exit` and `ls` report; one that cannot
/// start is refused. It takes no more input. The program starts at `--size`, in the
/// `agent` workspace, which no client is moved to.
#[test]
fn a_run_program_keeps_its_pane_screen_and_status() -> Outcome {
    let server = Server::start("")?;
    let script = "stty size; echo started; exit 3";
    let pane = id(&server.ok(&["run", "--size", "40x10", "--", "sh", "-c", script])?);
    assert_eq!(server.ok(&["wait", "-t", &pane, "--exit"])?, "exited 3\n");
    let screen = server.ok(&["capture-pane", "-t", &pane])?;
    assert_eq!(
        screen, "10 40\nstarted\n",
        "trimmed, and sized from the start"
    );
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("+2 agent"), "{ls}");
    assert!(ls.contains(&format!("{pane} sh 40x10 exited 3")), "{ls}");
    let refused = server.fux(&["send-keys", "-t", &pane, "x"])?;
    assert_eq!(refused.status, 1);
    assert!(
        refused.stderr.contains("exited with status 3"),
        "{}",
        refused.stderr
    );
    // A program that cannot start is refused, saying why.
    let missing = server.fux(&["run", "--", "/no/such/program"])?;
    assert_eq!(missing.status, 1);
    assert!(
        missing.stderr.contains("No such file"),
        "{}",
        missing.stderr
    );
    Ok(())
}

/// `wait` answers as soon as the screen shows its text, stops showing it,
/// changes, or the pane goes quiet; on timeout it fails, saying so.
/// `send-keys --wait-idle --print` sends, waits and reads in one call.
#[test]
fn wait_answers_as_the_pane_shows_changes_or_goes_quiet() -> Outcome {
    let server = Server::start("")?;
    let script = "echo one; sleep 0.5; echo two; exec cat";
    let pane = id(&server.ok(&["run", "--", "sh", "-c", script])?);
    let start = std::time::Instant::now();
    server.ok(&["wait", "-t", &pane, "--text", "two", "--timeout", "5"])?;
    assert!(
        start.elapsed() < std::time::Duration::from_secs(4),
        "it answers as it comes"
    );
    server.ok(&["wait", "-t", &pane, "--gone", "never shown"])?;
    let quiet = server.ok(&["wait", "-t", &pane, "--idle", "200", "--print"])?;
    assert_eq!(quiet, "one\ntwo\n");
    let late = server.fux(&["wait", "-t", &pane, "--text", "never", "--timeout", "0.3"])?;
    assert_eq!(late.status, 1);
    assert!(
        late.stderr.contains("timed out after 0.3s"),
        "{}",
        late.stderr
    );
    // A wait for a change, answered by keys sent while it waits.
    let mut change = std::process::Command::new(FUX)
        .args(["wait", "-t", &pane, "--change"])
        .env("FUX_SOCKET", &server.socket)
        .spawn()
        .map_err(e)?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    let typed = ["send-keys", "-t", &pane, "--text", "hello", "Enter"];
    let echoed = server.ok(&[&typed[..], &["--wait-idle", "200", "--print"]].concat())?;
    assert!(echoed.ends_with("hello\nhello\n"), "{echoed}");
    assert!(change.wait().map_err(e)?.success());
    Ok(())
}

/// Sizes are set, not worked out: a tab no client shows is laid out on its
/// `--size` and later on `resize`'s, and a pane is made a width in cells or
/// a percentage of its tab; a size it cannot have is refused, nothing
/// changed.
#[test]
fn sizes_are_set_as_asked() -> Outcome {
    let server = Server::start("")?;
    let tab = server.ok(&["new-tab", "-t", "+1", "--size", "100x20"])?;
    let (tab, left) = tab.trim().split_once(' ').ok_or("a tab and a pane")?;
    let size = |pane: &str| -> Result<String, String> {
        let ls = server.ok(&["ls"])?;
        let line = ls
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{pane} ")));
        let line = line.ok_or(format!("{pane} in {ls}"))?;
        Ok(line
            .split_whitespace()
            .nth(2)
            .unwrap_or_default()
            .to_owned())
    };
    assert_eq!(size(left)?, "100x20");
    let right = id(&server.ok(&["split", "-h", "-t", left])?);
    server.ok(&["resize-pane", "-t", right.as_str(), "-x", "25%"])?;
    assert_eq!(size(&right)?, "25x20");
    assert_eq!(size(left)?, "74x20");
    let refused = server.fux(&["resize-pane", "-t", left, "-x", "100"])?;
    assert_eq!(refused.status, 1);
    assert!(
        refused.stderr.contains("cannot be made 100x20"),
        "{}",
        refused.stderr
    );
    assert_eq!(size(left)?, "74x20", "nothing changed");
    server.ok(&["resize", "-t", tab, "40x10"])?;
    eventually(
        "the tab laid out again",
        || Ok(size(left)?.ends_with("x10")),
    )?;
    Ok(())
}

/// Reads what a pane's program is sent, one read per line in hex, in raw
/// mode with bracketed paste and SGR mouse reports on, until `q`.
const READS: &str = r#"
import os, sys, tty
tty.setraw(0)
os.write(1, b"\x1b[?2004h\x1b[?1000h\x1b[?1006hready\r\n")
while True:
    got = os.read(0, 256)
    os.write(1, (got.hex() + "\r\n").encode())
    if got.endswith(b"q"):
        break
"#;

/// Keys reach the program as a person would send them: a lone Escape read
/// alone, not as Alt with what follows; typed text apart from the Enter
/// after it; a paste bracketed; a count repeated; a click encoded as the
/// program asked.
#[test]
fn input_reaches_the_program_as_a_person_sends_it() -> Outcome {
    let server = Server::start("")?;
    let path = server.dir.join("reads.py");
    std::fs::write(&path, READS).map_err(e)?;
    let script = path.to_string_lossy().into_owned();
    let pane = id(&server.ok(&["run", "--", "python3", "-I", &script])?);
    server.ok(&["wait", "-t", &pane, "--text", "ready"])?;
    let sent = |args: &[&str]| -> Result<String, String> {
        let send = [
            &["send-keys", "-t", &pane][..],
            args,
            &["--wait-idle", "150", "--print"],
        ];
        let screen = server.ok(&send.concat())?;
        Ok(screen.lines().skip(1).collect::<Vec<_>>().join(" "))
    };
    assert_eq!(sent(&["Escape", "Escape"])?, "1b 1b");
    assert_eq!(sent(&["--text", "hi", "Enter"])?, "1b 1b 6869 0d");
    let pasted = sent(&["--paste", "a b"])?;
    assert!(
        pasted.ends_with(" 1b5b3230307e6120621b5b3230317e"),
        "{pasted}"
    );
    assert!(sent(&["-N", "3", "BSpace"])?.ends_with("7f7f7f"));
    server.ok(&["send-mouse", "-t", &pane, "click", "2", "4"])?;
    let screen = server.ok(&["wait", "-t", &pane, "--idle", "150", "--print"])?;
    assert!(
        screen.contains("1b5b3c303b353b334d1b5b3c303b353b336d"),
        "{screen}"
    );
    server.ok(&["send-keys", "-t", &pane, "q"])?;
    assert_eq!(server.ok(&["wait", "-t", &pane, "--exit"])?, "exited 0\n");
    Ok(())
}

/// A headless client stands in for a terminal of a size set from the
/// command line: it prints its id, records what it is sent, and goes on a
/// signal. A terminal's client sets its own size.
#[test]
fn a_headless_client_is_sized_from_the_command_line() -> Outcome {
    use std::io::BufRead;
    let server = Server::start("")?;
    let log = server.dir.join("client.log");
    let mut client = std::process::Command::new(FUX)
        .args(["attach", "--headless", "--size", "50x10", "--log"])
        .arg(&log)
        .env("FUX_SOCKET", &server.socket)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(e)?;
    let stdout = client.stdout.take().ok_or("its stdout")?;
    let mut lines = std::io::BufReader::new(stdout).lines();
    let id = lines.next().ok_or("its id")?.map_err(e)?;
    assert_eq!(id, "c1");
    assert!(server.ok(&["ls"])?.contains("client c1 50x10"));
    server.ok(&["resize", "-c", "c1", "60x12"])?;
    assert!(server.ok(&["ls"])?.contains("client c1 60x12"));
    eventually("a paint recorded", || {
        Ok(std::fs::read_to_string(&log).is_ok_and(|l| l.contains("main")))
    })?;
    let mut terminal = server.attach(10, 40)?;
    terminal.pump()?;
    let refused = server.fux(&["resize", "-c", "c2", "60x12"])?;
    assert!(
        refused.stderr.contains("has a terminal"),
        "{}",
        refused.stderr
    );
    signal(
        i32::try_from(client.id()).map_err(e)?,
        fuxix::process::Signal::Term,
    );
    assert_eq!(
        lines.next().ok_or("its end")?.map_err(e)?,
        "[detached by a signal]"
    );
    assert!(client.wait().map_err(e)?.success());
    Ok(())
}

/// A tmux name or flag gets a hint, and a usage error shows the command's
/// own usage, not every command's.
#[test]
fn tmux_habits_get_hints_and_usage_errors_stay_short() -> Outcome {
    let server = Server::start("")?;
    let out = server.fux(&["list-panes"])?;
    assert_eq!(out.status, 2);
    assert!(out.stderr.contains("`fux ls` lists"), "{}", out.stderr);
    let out = server.fux(&["capture-pane", "-p"])?;
    assert!(
        out.stderr.contains("prints the screen without it"),
        "{}",
        out.stderr
    );
    let out = server.fux(&["split", "-R"])?;
    assert_eq!(out.stderr.lines().count(), 2, "{}", out.stderr);
    assert!(
        out.stderr.contains("usage: fux split -h|-v"),
        "{}",
        out.stderr
    );
    Ok(())
}
