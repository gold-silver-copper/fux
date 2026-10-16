//! Driving programs from the command line, as a script or an agent does:
//! `run` starts one out of every client's way and keeps its pane once it
//! exits; every action settles and answers with what the program does and
//! what came; `wait` answers when a pane shows something, settles or exits;
//! `send-keys` and `send-mouse` send what a person would, sizes are set,
//! and a headless client stands in for a terminal.
mod support;
use support::*;

/// The pane an answer is about: the first word of its first line.
fn id(out: &str) -> String {
    out.split_whitespace().next().unwrap_or_default().to_owned()
}

/// An answer's first line: the pane and what its program does.
fn head(out: &str) -> &str {
    out.lines().next().unwrap_or_default()
}

/// What an answer shows after its first line.
fn body(out: &str) -> &str {
    out.split_once('\n').map_or("", |(_, rest)| rest)
}

/// `argv` as options words: its program, then each argument.
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

/// `run`'s words: `opts`, then `before` (name, size, waits…), then the
/// program, then `run`.
fn run<'a>(before: &[&'a str], argv: &[&'a str]) -> Vec<&'a str> {
    [&["opts"][..], before, &program(argv), &["run"]].concat()
}

/// A program `run` starts keeps its pane once it exits: its last screen,
/// and its status, which `wait-exit` and `ls` report; one that cannot
/// start is refused. It takes no more input. The program starts at its
/// `size`, in the `agent` workspace, which no client is moved to.
#[test]
fn a_run_program_keeps_its_pane_screen_and_status() -> Outcome {
    let server = Server::start("")?;
    let script = "stty size; echo started; exit 3";
    let started = server.ok(&run(&["40", "10", "size"], &["sh", "-c", script]))?;
    let pane = id(&started);
    assert_eq!(
        started,
        format!("{pane} exited 3\n10 40\nstarted\n"),
        "run answers once it settles: here, once it exits"
    );
    // As timeout(1), a wait for the exit exits with the program's status.
    let ended = server.fux(&["str", &pane, "pane", "opts", "wait-exit", "wait"])?;
    assert_eq!(
        (ended.status, ended.stdout.as_str()),
        (3, format!("{pane} exited 3\n10 40\nstarted\n").as_str()),
        "its state, and what came since it started"
    );
    let screen = server.ok(&["str", &pane, "pane", "opts", "capture"])?;
    assert_eq!(
        screen,
        format!("{pane} exited 3\n10 40\nstarted\n"),
        "trimmed, and sized from the start"
    );
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("+2 agent"), "{ls}");
    assert!(ls.contains(&format!("{pane} sh 40x10 exited 3")), "{ls}");
    let refused = server.fux(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "x",
        "text",
        "send-keys",
    ])?;
    assert_eq!(refused.status, 1);
    assert!(refused.stderr.contains("exited 3"), "{}", refused.stderr);
    // A program that cannot start is refused, saying why.
    let missing = server.fux(&run(&[], &["/no/such/program"]))?;
    assert_eq!(missing.status, 1);
    assert!(
        missing.stderr.contains("No such file"),
        "{}",
        missing.stderr
    );
    Ok(())
}

/// `wait` answers as soon as the screen shows its text or stops showing
/// it; on its own, with no text, once the pane settles; on timeout it
/// fails, saying so. A text that cannot come, the program reading its
/// terminal without it, ends the wait at once (status 7) with the screen.
/// `send-keys` answers with what the keys brought, their echo included.
#[test]
fn wait_answers_as_the_pane_shows_or_settles() -> Outcome {
    let server = Server::start("")?;
    let script =
        r#"echo one; sleep 0.5; echo two; sleep 0.2; while read l; do echo "got $l"; done"#;
    let pane = id(&server.ok(&run(&["0", "timeout"], &["sh", "-c", script]))?);
    let start = std::time::Instant::now();
    let p = ["str", pane.as_str(), "pane", "opts"];
    let two =
        server.ok(&[&p[..], &["str", "two", "wait-text", "5", "timeout", "wait"]].concat())?;
    assert_eq!(two, "two\n", "the line it is on");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(4),
        "it answers as it comes"
    );
    let gone = server.ok(&[&p[..], &["str", "never shown", "wait-gone", "wait"]].concat())?;
    // Met at once: the pane's state as it is, and what came.
    assert!(gone.starts_with(&format!("{pane} ")), "{gone}");
    assert_eq!(body(&gone), "one\ntwo\n");
    let quiet = server.ok(&[&p[..], &["wait"]].concat())?;
    assert_eq!(
        quiet,
        format!("{pane} {READING}\none\ntwo\n"),
        "what came since it started: no keys went in"
    );
    // Where fux is not told the program reads (not Linux), the text is
    // waited for to the limit, wait-seconds (30s).
    if PROBED {
        let start = std::time::Instant::now();
        let never = server.fux(&[&p[..], &["str", "never", "wait-text", "wait"]].concat())?;
        assert_eq!(
            (never.status, never.stdout.as_str()),
            (7, format!("{pane} reading\none\ntwo\n").as_str()),
            "{}",
            never.stderr
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "not its timeout"
        );
    }
    let echoed = server.ok(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "hello",
        "text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    assert_eq!(echoed, format!("{pane} {READING}\nhello\ngot hello\n"));
    // A program at work: the wait runs out, and says so.
    let busy = id(&server.ok(&run(&["0", "timeout"], &["sleep", "60"]))?);
    let late = server.fux(&[
        "str",
        &busy,
        "pane",
        "opts",
        "str",
        "never",
        "wait-text",
        "1",
        "timeout",
        "wait",
    ])?;
    assert_eq!(late.status, 124, "a timeout is told from a failure");
    assert!(
        late.stderr.contains("timed out after 1s"),
        "{}",
        late.stderr
    );
    Ok(())
}

/// Sizes are set, not worked out: a tab no client shows is laid out on its
/// `size` and later on `resize`'s, and a pane is made a width in cells or
/// a percentage of its tab; a size it cannot have is refused, nothing
/// changed.
#[test]
fn sizes_are_set_as_asked() -> Outcome {
    let server = Server::start("")?;
    let made = server.ok(&[
        "opts",
        "str",
        "+1",
        "workspace",
        "in",
        "100",
        "20",
        "size",
        "new-tab",
        "dup",
        "tab",
    ])?;
    let (left, tab) = made.trim().split_once('\n').ok_or("a pane and its tab")?;
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
    server.ok(&["str", left, "pane", "100", "22", "resize"])?;
    assert_eq!(size(left)?, "100x22", "a pane's tab");
    server.ok(&["str", tab, "tab", "100", "20", "resize"])?;
    let right = id(&server.ok(&["str", left, "pane", "opts", "horizontal", "split"])?);
    server.ok(&["str", &right, "pane", "25", "percent", "resize-x"])?;
    assert_eq!(size(&right)?, "25x20");
    assert_eq!(size(left)?, "74x20");
    let refused = server.fux(&["str", left, "pane", "100", "resize-x"])?;
    assert_eq!(refused.status, 1);
    assert!(
        refused.stderr.contains("cannot be made 100x20"),
        "{}",
        refused.stderr
    );
    assert_eq!(size(left)?, "74x20", "nothing changed");
    server.ok(&["str", tab, "tab", "40", "10", "resize"])?;
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
/// after it; a paste bracketed; a count repeating only the key before it;
/// a click encoded as the program asked. Each answers with what it brought.
#[test]
fn input_reaches_the_program_as_a_person_sends_it() -> Outcome {
    let server = Server::start("")?;
    let path = server.dir.join("reads.py");
    std::fs::write(&path, READS).map_err(e)?;
    let script = path.to_string_lossy().into_owned();
    let pane = id(&server.ok(&run(
        &["str", "ready", "wait-text"],
        &["python3", "-I", &script],
    ))?);
    // What the program read, one read per line.
    let read = |answer: &str| body(answer).lines().collect::<Vec<_>>().join(" ");
    let sent = |keys: &[&str]| -> Result<String, String> {
        let send = [
            &["str", pane.as_str(), "pane", "keys"][..],
            keys,
            &["send-keys"],
        ];
        let answer = server.ok(&send.concat())?;
        assert!(answer.starts_with(&format!("{pane} {READING}")), "{answer}");
        Ok(read(&answer))
    };
    assert_eq!(
        sent(&["str", "Escape", "key", "str", "Escape", "key"])?,
        "1b 1b"
    );
    assert_eq!(
        sent(&["str", "hi", "text", "str", "Enter", "key"])?,
        "6869 0d"
    );
    assert_eq!(
        sent(&["str", "a b", "paste"])?,
        "1b5b3230307e6120621b5b3230317e"
    );
    let repeated = sent(&["str", "BSpace", "key", "3", "times", "str", "x", "text"])?;
    assert_eq!(repeated.replace(' ', ""), "7f7f7f78", "x once");
    let menu = sent(&["str", "Down", "key", "2", "times", "str", "Enter", "key"])?;
    assert_eq!(menu.replace(' ', ""), "1b5b421b5b420d", "Down Down Enter");
    let clicked = server.ok(&[
        "str",
        &pane,
        "pane",
        "str",
        "click",
        "2",
        "4",
        "mouse",
        "send-mouse",
    ])?;
    assert_eq!(read(&clicked), "1b5b3c303b353b334d1b5b3c303b353b336d");
    server.ok(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "q",
        "text",
        "send-keys",
    ])?;
    assert_eq!(
        server
            .ok(&["str", &pane, "pane", "opts", "wait-exit", "wait"])?
            .lines()
            .next(),
        Some(format!("{pane} exited 0").as_str())
    );
    Ok(())
}

/// A headless client stands in for a terminal of a size set from the
/// command line: it prints its id, shows what it is sent, and goes on a
/// signal. A terminal's client sets its own size.
#[test]
fn a_headless_client_is_sized_from_the_command_line() -> Outcome {
    use std::io::BufRead;
    let server = Server::start("")?;
    let mut client = std::process::Command::new(FUX)
        .args(["50", "10", "headless"])
        .env("FUX_SOCKET", &server.socket)
        .env("XDG_CONFIG_HOME", server.dir.join("config"))
        .env_remove("FUX_PANE")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(e)?;
    let stdout = client.stdout.take().ok_or("its stdout")?;
    let mut lines = std::io::BufReader::new(stdout).lines();
    let id = lines.next().ok_or("its id")?.map_err(e)?;
    assert_eq!(id, "c1");
    assert!(server.ok(&["ls"])?.contains("client c1 50x10"));
    server.ok(&["str", "c1", "client", "60", "12", "resize-client"])?;
    assert!(server.ok(&["ls"])?.contains("client c1 60x12"));
    eventually("a paint", || {
        Ok(server
            .ok(&["str", "c1", "client", "capture-client"])?
            .contains("main"))
    })?;
    // Keys typed at it are a person's: the prefix and `c` open copy mode.
    server.ok(&[
        "str",
        "c1",
        "client",
        "keys",
        "str",
        "C-b",
        "key",
        "str",
        "c",
        "text",
        "send-keys",
    ])?;
    eventually("copy mode", || {
        Ok(server
            .ok(&["str", "c1", "client", "capture-client"])?
            .contains("COPY"))
    })?;
    server.ok(&[
        "str",
        "c1",
        "client",
        "keys",
        "str",
        "q",
        "text",
        "send-keys",
    ])?;
    let mut terminal = server.attach(10, 40)?;
    terminal.ready()?;
    let refused = server.fux(&["str", "c2", "client", "60", "12", "resize-client"])?;
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

/// A pane's pictures reach a client whose terminal draws sixel, clipped
/// to the pane, after the program was told the terminal draws them; and
/// the capture's JSON says where they are.
#[test]
fn pictures_reach_a_terminal_that_draws_sixel() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 60)?;
    let painted = |client: &Client, what: &[u8]| {
        (0..client.painted.len()).any(|i| {
            client
                .painted
                .get(i..)
                .is_some_and(|rest| rest.starts_with(what))
        })
    };
    eventually("the terminal asked", || {
        client.pump()?;
        Ok(painted(&client, b"\x1b[16t\x1b[?u\x1b[c"))
    })?;
    // A terminal of 10 by 20 pixel cells that draws sixel.
    client.send(b"\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[6;20;10t\x1b[?62;4;22c")?;
    let asked =
        r"printf '\033[c'; read -r _; printf '\033Pq#1;2;100;0;0!20~-!20~\033\\'; echo drawn";
    server.ok(&[
        "str",
        "%1",
        "pane",
        "keys",
        "str",
        asked,
        "text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    server.ok(&[
        "str",
        "%1",
        "pane",
        "opts",
        "str",
        "62;4;22c",
        "wait-text",
        "wait",
    ])?;
    server.ok(&[
        "str",
        "%1",
        "pane",
        "keys",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    eventually("the picture sent", || {
        client.pump()?;
        Ok(painted(&client, b"\x1bP0;1;0q\"1;1;20;12"))
    })?;
    let json = server.ok(&["str", "%1", "pane", "opts", "capture", "json"])?;
    assert!(
        json.contains(r#""col":0,"rows":1,"cols":2,"width":20,"height":12}]"#),
        "{json}"
    );
    Ok(())
}

/// A wait misses nothing and never waits out its timeout for nothing: text
/// that scrolled past since it began is found and its line printed, a
/// program that exits first ends it (status 3), and a timeout is 124.
/// send-keys arms its wait before the keys go in, and run as the program
/// starts, so what they bring cannot come too soon to be seen.
#[test]
fn waits_see_what_scrolled_past_and_end_when_the_program_does() -> Outcome {
    let server = Server::start("")?;
    let script = "read go; echo 'the needle line'; seq 300; read go; exit 2";
    let pane = id(&server.ok(&run(&["40", "10", "size"], &["sh", "-c", script]))?);
    let enter = ["str", pane.as_str(), "pane", "keys", "str", "Enter", "key"];
    let found = server.ok(&[&enter[..], &["str", "needle", "wait-text", "send-keys"]].concat())?;
    assert_eq!(found, "the needle line\n", "found though it scrolled away");
    let json = server.ok(&[
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "300",
        "wait-text",
        "wait",
        "json",
    ])?;
    assert!(json.contains(r#""met":"text""#), "{json}");
    let ended = server.fux(&[&enter[..], &["str", "never", "wait-text", "send-keys"]].concat())?;
    assert_eq!(ended.status, 3, "{}", ended.stderr);
    assert!(ended.stderr.contains("exited 2"), "{}", ended.stderr);
    let ended = server.fux(&["str", &pane, "pane", "opts", "wait-exit", "wait"])?;
    assert_eq!(ended.status, 2);
    assert!(
        ended.stdout.starts_with(&format!("{pane} exited 2\n")),
        "{}",
        ended.stdout
    );
    // run waits from the program's first output: a flood cannot outrun it.
    let flood = "echo 'a needle'; seq 300; exec cat";
    let started = server.ok(&run(&["str", "needle", "wait-text"], &["sh", "-c", flood]))?;
    assert!(started.starts_with('%'), "{started}");
    let busy = id(&server.ok(&run(&["0", "timeout"], &["sleep", "60"]))?);
    let late = server.fux(&[
        "str",
        &busy,
        "pane",
        "opts",
        "str",
        "ready",
        "wait-text",
        "1",
        "timeout",
        "wait",
    ])?;
    assert_eq!(late.status, 124, "{}", late.stderr);
    Ok(())
}

/// Every action answers once the program settles, with what it does:
/// `reading` when it waits for keys, and `· no change` when the keys drew
/// nothing; `busy` when it is still at work at the limit (status 124), or
/// when asked with `0 timeout` (at once, status 0). A wait on its own
/// counts what came since the last input: what came before cannot come
/// again, and a program reading its terminal without it ends it (status 7).
#[test]
fn answers_say_what_the_program_does() -> Outcome {
    let server = Server::start("")?;
    let script = "stty -echo; echo ready; read x; echo later; exec sleep 60";
    let started = server.ok(&run(&[], &["sh", "-c", script]))?;
    let pane = id(&started);
    assert_eq!(started, format!("{pane} {READING}\nready\n"));
    let keys = |keys: &[&str]| {
        server.fux(
            &[
                &["str", pane.as_str(), "pane", "keys"][..],
                keys,
                &["send-keys"],
            ]
            .concat(),
        )
    };
    let unseen = keys(&["str", "x", "text"])?;
    assert_eq!(unseen.stdout, format!("{pane} {UNCHANGED}\n"));
    let p = ["str", pane.as_str(), "pane", "opts"];
    // Not told it reads (not Linux), fux waits for the text to the limit.
    if PROBED {
        let before = server.fux(&[&p[..], &["str", "ready", "wait-text", "wait"]].concat())?;
        assert_eq!(
            (before.status, before.stdout.as_str()),
            (7, format!("{pane} reading\nready\n").as_str()),
            "it came before the keys"
        );
    }
    // Asleep: busy at the limit; elsewhere, quiet, taken as settled.
    let at_work = keys(&["str", "Enter", "key", "1", "timeout"])?;
    assert_eq!(
        (at_work.status, at_work.stdout.as_str()),
        (
            if PROBED { 124 } else { 0 },
            format!("{pane} {}\nlater\n", asleep(&pane)).as_str()
        )
    );
    let since = server.ok(&[&p[..], &["str", "later", "wait-text", "wait"]].concat())?;
    assert_eq!(since, "later\n", "it came after the keys");
    let now = server.fux(&[&p[..], &["0", "timeout", "wait"]].concat())?;
    assert_eq!(
        (now.status, now.stdout.as_str()),
        (0, format!("{pane} {}\nlater\n", asleep(&pane)).as_str()),
        "what came since the keys"
    );
    assert_eq!(
        server.ok(&["str", &pane, "pane", "str", "state", "get"])?,
        format!("{ASLEEP}\n")
    );
    Ok(())
}

/// A shell's OSC 133 marks tell when a command ends and how (the answer's
/// `· done S`), and which output was its; a bell or a desktop notification is a pane asking for
/// a person, which `wait-bell` hears and the JSON counts.
#[test]
fn shell_marks_and_bells_are_heard() -> Outcome {
    let server = Server::start("")?;
    let script = r"read go; printf '\033]133;C\007'; echo out1; echo out2; printf '\033]133;D;7\007'; echo prompt; read go; printf '\a\033]9;tests passed\007'; read go; printf '\a'; exec cat";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", script]))?);
    let enter = ["str", pane.as_str(), "pane", "keys", "str", "Enter", "key"];
    let done = server.ok(&[&enter[..], &["send-keys"]].concat())?;
    assert_eq!(head(&done), format!("{pane} {READING} · done 7"));
    assert!(body(&done).contains("out1\nout2\nprompt\n"), "{done}");
    assert_eq!(last_output(&server, &pane)?, "out1\nout2\n");
    // A bell then a notification in one write: the wait met the bell,
    // and says that alert, not the pane's last.
    let rung = server.ok(&[&enter[..], &["wait-bell", "send-keys", "json"]].concat())?;
    assert!(rung.contains(r#""notification":null"#), "{rung}");
    let json = server.ok(&["str", &pane, "pane", "opts", "capture", "json"])?;
    for field in [
        r#""command_running":false"#,
        r#""last_status":7"#,
        r#""notification":"tests passed""#,
        r#""bells":2"#,
    ] {
        assert!(json.contains(field), "{field} in {json}");
    }
    // A plain bell that meets a later wait is said as one.
    let bell = server.ok(&[&enter[..], &["wait-bell", "send-keys", "json"]].concat())?;
    assert!(bell.contains(r#""notification":null"#), "{bell}");
    Ok(())
}

/// `respawn` starts a run program again in its pane, found by its tab's
/// name; `log-to` keeps all it wrote; `all-history` reads all history,
/// `main-screen` what a full-screen program hides, `alt-screen` its last
/// screen; a mistyped key name is refused.
#[test]
fn run_programs_are_restarted_logged_and_read_whole() -> Outcome {
    let server = Server::start("")?;
    let log = server.dir.join("job.log");
    let log_arg = log.to_string_lossy().into_owned();
    let script = "echo before; seq 500; printf '\\033[?1049h'; echo \"full $V\"; read go";
    let before = [
        "str", "job", "name", "str", "V", "str", "a", "env", "str", &log_arg, "log-to", "40", "10",
        "size",
    ];
    let pane = id(&server.ok(&run(&before, &["sh", "-c", script]))?);
    server.ok(&[
        "str",
        "job",
        "pane",
        "opts",
        "str",
        "full a",
        "wait-text",
        "wait",
    ])?;
    server.ok(&run(
        &["str", "other", "name"],
        &["sh", "-c", "printf 'R> '; exec cat"],
    ))?;
    let prompt = [
        "str",
        "other",
        "pane",
        "opts",
        "str",
        "R> ",
        "wait-text",
        "5",
        "timeout",
        "wait",
    ];
    assert!(
        server.ok(&prompt)?.ends_with("R>\n"),
        "a prompt's blank is the line's end"
    );
    let main = server.ok(&[
        "str",
        &pane,
        "pane",
        "opts",
        "main-screen",
        "all-history",
        "capture",
    ])?;
    assert!(body(&main).starts_with("before\n1\n"), "{main}");
    assert!(main.contains("\n500\n"), "{main}");
    let refused = server.fux(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "Entr",
        "key",
        "send-keys",
    ])?;
    assert_eq!(refused.status, 1);
    assert!(refused.stderr.contains("Entr"), "{}", refused.stderr);
    let busy = server.fux(&["str", &pane, "pane", "opts", "respawn"])?;
    assert_eq!(busy.status, 1, "it runs: {}", busy.stderr);
    let again = [
        "str",
        "job",
        "pane",
        "opts",
        "kill-first",
        "str",
        "V",
        "str",
        "b",
        "env",
        "str",
        "full b",
        "wait-text",
        "respawn",
    ];
    assert_eq!(
        server.ok(&again)?,
        format!("{pane}\nfull b\n"),
        "its pane, then the line waited for"
    );
    let empty = server.fux(&["str", &pane, "pane", "opts", "str", "", "wait-text", "wait"])?;
    assert_eq!(
        empty.status, 1,
        "an empty text is refused: {}",
        empty.stderr
    );
    let kept = std::fs::read_to_string(&log).map_err(e)?;
    assert!(kept.contains("full a") && kept.contains("full b"), "{kept}");
    server.ok(&["str", &pane, "pane", "str", "done", "notify"])?;
    // A full-screen program's last screen, after it left it and exited.
    let menu = r"printf '\033[?1049hpicked beta'; printf '\033[?1049l'";
    let menu = id(&server.ok(&run(&["wait-exit"], &["sh", "-c", menu]))?);
    assert!(
        !server
            .ok(&["str", &menu, "pane", "opts", "capture"])?
            .contains("picked")
    );
    assert_eq!(
        server.ok(&["str", &menu, "pane", "opts", "alt-screen", "capture"])?,
        format!("{menu} exited 0\npicked beta\n")
    );
    Ok(())
}

/// `send-keys` to `pane` typing `line`, then Enter, and `wait` (words).
fn typed<'a>(pane: &'a str, line: &'a str, wait: &[&'a str]) -> Vec<&'a str> {
    [
        &[
            "str", pane, "pane", "keys", "str", line, "text", "str", "Enter", "key",
        ][..],
        wait,
        &["send-keys"],
    ]
    .concat()
}

/// The last shell command's output in `pane`.
fn last_output(server: &Server, pane: &str) -> Result<String, String> {
    let capture = server.ok(&["str", pane, "pane", "opts", "last-output", "capture"])?;
    Ok(body(&capture).to_owned())
}

/// How the shell command an answer saw end ended: `done S`.
fn done(answer: &str) -> Option<&str> {
    head(answer)
        .split(" · ")
        .find(|said| said.starts_with("done"))
}

/// `line` in `pane`: `text` typed, then Enter; the answer once the shell
/// settles, its first line saying how the command ended.
fn line(server: &Server, pane: &str, text: &str) -> Result<String, String> {
    server.ok(&["str", pane, "pane", "str", text, "line"])
}

/// The shells fux starts mark their commands with no setup, after the
/// user's own startup, whose prompt and `PROMPT_COMMAND` behave as before;
/// a wait after typed keys does not take their echo for the output; and
/// `set shell-marks off` leaves shells as they are.
#[test]
fn shells_mark_their_commands_with_no_setup() -> Outcome {
    let server = Server::start("")?;
    let home = server.dir.join("home");
    std::fs::create_dir_all(&home).map_err(e)?;
    let rc = "PS1='mine> '\nPROMPT_COMMAND='echo \"[saw $?]\"'\n";
    std::fs::write(home.join(".bashrc"), rc).map_err(e)?;
    let home_dir = home.to_string_lossy().into_owned();
    let start = [
        "str",
        "sh",
        "name",
        "str",
        "HOME",
        "str",
        &home_dir,
        "env",
        "str",
        "mine>",
        "wait-text",
    ];
    server.ok(&run(&start, &["bash"]))?;
    assert_eq!(done(&line(&server, "sh", "false")?), Some("done 1"));
    let echo = "echo RESULT=$((6*7))";
    assert_eq!(
        server.ok(&typed("sh", echo, &["str", "RESULT=", "wait-text"]))?,
        "RESULT=42\n",
        "not the typed line"
    );
    server.ok(&[
        "str",
        "sh",
        "pane",
        "opts",
        "str",
        "[saw 0]",
        "wait-text",
        "wait",
    ])?;
    assert_eq!(last_output(&server, "sh")?, "RESULT=42\n");
    let screen = server.ok(&["str", "sh", "pane", "opts", "capture"])?;
    assert!(
        screen.contains("[saw 1]") && screen.contains("mine> "),
        "{screen}"
    );
    // zsh and fish, where there are (CI, with FUX_TEST_SHELLS, insists).
    let has = |shell: &str| -> Result<bool, String> {
        let here = std::process::Command::new(shell)
            .args(["-c", "true"])
            .status()
            .is_ok_and(|s| s.success());
        if !here && std::env::var_os("FUX_TEST_SHELLS").is_some() {
            return Err(format!("{shell} is not installed"));
        }
        Ok(here)
    };
    if has("zsh")? {
        let zdot = server.dir.join("zdot");
        std::fs::create_dir_all(&zdot).map_err(e)?;
        std::fs::write(zdot.join(".zshrc"), "PROMPT='zed> '\n").map_err(e)?;
        // Read through fux's shim; it also spares Ubuntu's global compinit.
        std::fs::write(zdot.join(".zshenv"), "skip_global_compinit=1\n").map_err(e)?;
        let zdot = zdot.to_string_lossy().into_owned();
        let start = [
            "str",
            "z",
            "name",
            "str",
            "ZDOTDIR",
            "str",
            &zdot,
            "env",
            "str",
            "zed>",
            "wait-text",
        ];
        server.ok(&run(&start, &["zsh"]))?;
        assert_eq!(done(&line(&server, "z", "(exit 3)")?), Some("done 3"));
    }
    // bash without its startup files, as agents start it, is marked too.
    server.ok(&run(&["str", "bare", "name"], &["bash", "--norc", "-i"]))?;
    // `true` first: bash before 4.4 (macOS's) marks from a trap, which a
    // subshell alone does not set off.
    assert_eq!(
        done(&line(&server, "bare", "true; (exit 4)")?),
        Some("done 4")
    );
    // fish, its prompt from its own config, marked by an event handler.
    if has("fish")? {
        let xdg = server.dir.join("xdg");
        std::fs::create_dir_all(xdg.join("fish")).map_err(e)?;
        let prompt = "function fish_prompt; echo -n 'fsh> '; end\n";
        std::fs::write(xdg.join("fish").join("config.fish"), prompt).map_err(e)?;
        let xdg = xdg.to_string_lossy().into_owned();
        let start = [
            "str",
            "f",
            "name",
            "str",
            "XDG_CONFIG_HOME",
            "str",
            &xdg,
            "env",
            "str",
            "fsh>",
            "wait-text",
        ];
        server.ok(&run(&start, &["fish"]))?;
        assert_eq!(done(&line(&server, "f", "sh -c 'exit 6'")?), Some("done 6"));
        line(&server, "f", "echo fish-$status")?;
        assert_eq!(last_output(&server, "f")?, "fish-6\n");
    }
    let off = Server::start("str shell-marks str off set")?;
    let start = [
        "str",
        "sh",
        "name",
        "str",
        "HOME",
        "str",
        &home_dir,
        "env",
        "str",
        "mine>",
        "wait-text",
    ];
    off.ok(&run(&start, &["bash"]))?;
    let unmarked = line(&off, "sh", "true")?;
    assert_eq!(done(&unmarked), None, "{unmarked}");
    Ok(())
}

const CLICKS: &str = r#"
import os, tty
tty.setraw(0)
os.write(1, b"\x1b[?1000h\x1b[?1006hready\r\n")
clicks = 0
while True:
    data = os.read(0, 64)
    if not data or data == b"q":
        break
    clicks += 1
    if clicks == 1:
        os.write(1, b"clicked once\r\n")
"#;

/// Every action can be confirmed: a click waits as keys do, and says when
/// it changed nothing; a selection shows in plain text; a signal reaches a
/// program that ignores hangups, and answers once it settles; a wait that
/// ends unmet says what the pane is doing; a full-screen program's answer
/// is its whole screen.
#[test]
fn actions_are_confirmed_and_screens_read_plainly() -> Outcome {
    let server = Server::start("")?;
    let path = server.dir.join("clicks.py");
    std::fs::write(&path, CLICKS).map_err(e)?;
    let script = path.to_string_lossy().into_owned();
    server.ok(&run(
        &["str", "dialog", "name", "str", "ready", "wait-text"],
        &["python3", "-I", &script],
    ))?;
    let click = ["str", "dialog", "pane", "str", "click", "0", "0", "mouse"];
    let answered =
        server.ok(&[&click[..], &["str", "clicked", "wait-text", "send-mouse"]].concat())?;
    assert_eq!(answered, "clicked once\n");
    let ignored = server.ok(&[&click[..], &["send-mouse"]].concat())?;
    assert!(ignored.ends_with(&format!("{UNCHANGED}\n")), "{ignored}");
    // A selection in reverse video, marked in plain text.
    let menu = r"printf 'alpha\n\033[7mbeta\033[0m\ngamma\n'; exec cat";
    let menu_pane = id(&server.ok(&run(
        &["str", "menu", "name", "str", "gamma", "wait-text"],
        &["sh", "-c", menu],
    ))?);
    let marked = server.ok(&["str", &menu_pane, "pane", "opts", "marks", "capture"])?;
    assert_eq!(body(&marked), "alpha\n⟦beta⟧\ngamma\n");
    // A program that ignores TERM and hangups: a timeout says what it is
    // doing, KILL ends it, and the wait says so.
    let stubborn = "trap '' TERM HUP INT; echo still here; read x";
    server.ok(&run(
        &["str", "stub", "name", "str", "here", "wait-text"],
        &["sh", "-c", stubborn],
    ))?;
    let ignored = server.ok(&["str", "stub", "pane", "str", "TERM", "signal"])?;
    assert!(ignored.ends_with(&format!("{UNCHANGED}\n")), "{ignored}");
    let term = server.fux(&[
        "str",
        "stub",
        "pane",
        "opts",
        "wait-exit",
        "1",
        "timeout",
        "wait",
    ])?;
    assert_eq!(term.status, 124);
    assert!(
        term.stderr.contains("unchanged for") && term.stderr.contains("last line: `still here`"),
        "{}",
        term.stderr
    );
    server.ok(&["str", "stub", "pane", "str", "KILL", "signal"])?;
    let kill = server.fux(&["str", "stub", "pane", "opts", "wait-exit", "wait"])?;
    assert_eq!(
        (kill.status, kill.stdout.lines().next()),
        (137, Some("%4 killed by SIGKILL"))
    );
    // A program that exits first: its last words come with the error.
    let crash = server.fux(&run(
        &["str", "READY", "wait-text"],
        &["sh", "-c", "echo 'fatal: no config'; exit 2"],
    ))?;
    assert_eq!(crash.status, 3);
    assert!(
        crash.stderr.contains("fatal: no config"),
        "{}",
        crash.stderr
    );
    // A tab closed by its name.
    server.ok(&["str", "dialog", "tab", "kill"])?;
    assert!(!server.ok(&["ls"])?.contains("dialog"));
    // A split where it is asked, a line typed as it is.
    let sub = server.dir.join("sub");
    std::fs::create_dir_all(&sub).map_err(e)?;
    let sub_arg = sub.to_string_lossy().into_owned();
    let new = id(&server.ok(&[
        "str",
        &menu_pane,
        "pane",
        "opts",
        "horizontal",
        "str",
        &sub_arg,
        "cwd",
        "str",
        "pwd && echo split-$((1+1))",
        "typed",
        "split",
    ])?);
    server.ok(&[
        "str",
        &new,
        "pane",
        "opts",
        "str",
        "split-2",
        "wait-text",
        "wait",
    ])?;
    let screen = server.ok(&["str", &new, "pane", "opts", "joined", "capture"])?;
    assert!(body(&screen).contains(&sub_arg), "{screen}");
    // A program just started has drawn nothing: its text goes only once it
    // has come.
    let loading = r"sleep 0.3; printf loading; sleep 0.3; printf '\r\033[Kready\n'; exec cat";
    let gone = id(&server.ok(&run(
        &["str", "loading", "wait-gone"],
        &["sh", "-c", loading],
    ))?);
    let done = server.ok(&["str", &gone, "pane", "opts", "capture"])?;
    assert!(
        done.ends_with("ready\n") || done.contains("\nready"),
        "{done}"
    );
    // A name a pane is found by is one tab's only: a second is refused,
    // saying why.
    let again = server.fux(&run(&["str", "menu", "name"], &["cat"]))?;
    assert_eq!(again.status, 1);
    assert!(
        again.stderr.contains("already") && again.stderr.contains(&menu_pane),
        "{}",
        again.stderr
    );
    // A line printed just before a program takes the full screen is seen,
    // and a full-screen program answers with its whole screen.
    let behind = r"echo 'error: disk full'; printf '\033[?1049hFULL SCREEN'; exec cat";
    server.ok(&run(
        &["str", "disk full", "wait-text"],
        &["sh", "-c", behind],
    ))?;
    let full = server.ok(&run(&[], &["sh", "-c", behind]))?;
    assert!(
        body(&full).contains("FULL SCREEN") && !full.contains("disk full"),
        "{full}"
    );
    Ok(())
}

/// What the stress tests found waits get wrong: text that comes again
/// after a clear, or after a Space key, is new; a respawn ends a wait on
/// the program before; and Enter typed after text, before more text, is
/// read alone.
#[test]
fn waits_hold_up_under_stress() -> Outcome {
    let server = Server::start("")?;
    let failed = server.fux(&run(
        &["str", "a", "name", "wait-exit"],
        &["sh", "-c", "echo FAIL-A; exit 1"],
    ))?;
    assert_eq!(failed.status, 1, "the program's status");
    server.ok(&run(&["str", "sh", "name"], &["bash", "--norc", "-i"]))?;
    line(&server, "sh", "clear; echo READY")?;
    // Cleared and printed again, other text: new, on the same row.
    assert_eq!(
        server.ok(&typed(
            "sh",
            "clear; echo READY 2",
            &["str", "READY", "wait-text"]
        ))?,
        "READY 2\n"
    );
    let spaced = [
        "str",
        "sh",
        "pane",
        "keys",
        "str",
        "echo",
        "text",
        "str",
        "Space",
        "key",
        "str",
        "done42",
        "text",
        "str",
        "Enter",
        "key",
        "str",
        "done42",
        "wait-text",
        "send-keys",
    ];
    assert_eq!(server.ok(&spaced)?, "done42\n");
    server.ok(&run(
        &["str", "r", "name", "0", "timeout"],
        &["sleep", "60"],
    ))?;
    let waiting = std::process::Command::new(FUX)
        .args([
            "str",
            "r",
            "pane",
            "opts",
            "wait-exit",
            "10",
            "timeout",
            "wait",
        ])
        .env("FUX_SOCKET", &server.socket)
        .env_remove("FUX_PANE")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(e)?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    server.ok(&[
        "str",
        "r",
        "pane",
        "opts",
        "kill-first",
        "0",
        "timeout",
        "respawn",
    ])?;
    let ended = waiting.wait_with_output().map_err(e)?;
    assert_eq!(ended.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&ended.stderr).contains("respawn"));
    Ok(())
}

/// What the stress tests found the command line gets wrong: a name that
/// reads as an id, or one taken, is refused; a tab's name finds it; a
/// click outside the pane is refused; a shell is not signalled.
#[test]
fn names_clicks_and_signals_are_checked() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(&["str", "ed", "name"], &["cat"]))?;
    assert_eq!(
        server.fux(&run(&["str", "@1", "name"], &["cat"]))?.status,
        1
    );
    assert_eq!(
        server
            .fux(&["opts", "str", "ed", "name", "new-tab"])?
            .status,
        1,
        "a name taken"
    );
    server.ok(&["str", "ed", "tab", "40", "12", "resize"])?;
    assert!(
        server.ok(&["ls"])?.contains(" 40x12"),
        "resized by its tab's name"
    );
    let clicks = "printf '\\033[?1000h\\033[?1006hready'; exec cat";
    server.ok(&run(
        &[
            "str",
            "m",
            "name",
            "20",
            "5",
            "size",
            "str",
            "ready",
            "wait-text",
        ],
        &["sh", "-c", clicks],
    ))?;
    let outside = server.fux(&[
        "str",
        "m",
        "pane",
        "str",
        "click",
        "5",
        "0",
        "mouse",
        "send-mouse",
    ])?;
    assert!(
        outside.stderr.contains("rows count 0-4"),
        "{}",
        outside.stderr
    );
    server.ok(&run(&["str", "shell", "name"], &["bash", "--norc", "-i"]))?;
    let signalled = server.fux(&["str", "shell", "pane", "str", "HUP", "signal"])?;
    assert!(
        signalled.stderr.contains("but its shell"),
        "{}",
        signalled.stderr
    );
    Ok(())
}

/// What the stress tests found screens and shells get wrong: a selection
/// drawn with a background, and one around wide glyphs, is marked whole;
/// `clear`'s scrollback erase empties the history; a line bash cannot
/// parse still ends; and an rc file's `set -u` or exported PROMPT_COMMAND
/// does not break the marks.
#[test]
fn screens_and_shells_hold_up_under_stress() -> Outcome {
    let server = Server::start("")?;
    let menu = r"printf 'alpha\n\033[48;5;236mbeta\033[0m\n\033[7m 日本 \033[0m\n'; exec cat";
    server.ok(&run(
        &["str", "menu", "name", "str", "beta", "wait-text"],
        &["sh", "-c", menu],
    ))?;
    let marked = server.ok(&["str", "menu", "pane", "opts", "marks", "capture"])?;
    assert_eq!(body(&marked), "alpha\n⟦beta⟧\n⟦ 日本 ⟧\n");
    // In SGR too, the marks outside what is highlighted.
    let styled = ["str", "menu", "pane", "opts", "marks", "styles", "capture"];
    let styled = server.ok(&styled)?;
    let beta = body(&styled).lines().nth(1).unwrap_or_default();
    assert!(
        beta.starts_with("⟦\x1b[") && beta.ends_with("\x1b[0m⟧") && beta.contains("beta"),
        "{beta:?}"
    );
    let flood = r"seq 50; printf '\033[H\033[2J\033[3Jnew'; exec cat";
    server.ok(&run(
        &[
            "str",
            "clear",
            "name",
            "20",
            "5",
            "size",
            "str",
            "new",
            "wait-text",
        ],
        &["sh", "-c", flood],
    ))?;
    let history = server.ok(&["str", "clear", "pane", "opts", "all-history", "capture"])?;
    assert_eq!(body(&history), "new\n");
    let home = server.dir.join("home");
    std::fs::create_dir_all(&home).map_err(e)?;
    let rc = "set -u\nexport PROMPT_COMMAND='echo own'\n";
    std::fs::write(home.join(".bashrc"), rc).map_err(e)?;
    let home = home.to_string_lossy().into_owned();
    server.ok(&run(
        &["str", "b", "name", "str", "HOME", "str", &home, "env"],
        &["bash"],
    ))?;
    // bash could not parse it: 2, or 258 before bash 4.
    let status = line(&server, "b", "if then fi")?;
    assert!(
        [Some("done 2"), Some("done 258")].contains(&done(&status)),
        "{status}"
    );
    line(&server, "b", "bash --norc -c 'echo $PROMPT_COMMAND'")?;
    let output = last_output(&server, "b")?;
    assert!(
        !output.contains("__fux"),
        "fux's PROMPT_COMMAND is not inherited: {output}"
    );
    Ok(())
}

/// A bash that reads no file of its own (`--norc`) keeps `PIPESTATUS`, marks
/// its first command too, and shows its own prompt with no function of
/// fux's left.
#[test]
fn a_bare_bash_keeps_its_pipestatus_and_prompt() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(
        &["str", "n", "name", "str", "PS1", "str", "mine> ", "env"],
        &["bash", "--norc", "-i"],
    ))?;
    line(&server, "n", "false | true; echo \"<${PIPESTATUS[*]}>\"")?;
    assert_eq!(last_output(&server, "n")?, "<1 0>\n");
    line(&server, "n", "echo \"[$(declare -F | grep fux)]\"")?;
    assert_eq!(last_output(&server, "n")?, "[]\n");
    let screen = server.ok(&["str", "n", "pane", "opts", "capture"])?;
    assert!(
        screen.contains("mine> ") && !screen.contains("PROMPT_COMMAND"),
        "{screen}"
    );
    Ok(())
}

/// Whether the `bash` panes run is older than 4.4 (macOS's 3.2).
fn old_bash() -> Result<bool, String> {
    let version = std::process::Command::new("bash")
        .args(["-c", "echo $((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1]))"])
        .output()
        .map_err(e)?;
    let version: u32 = String::from_utf8_lossy(&version.stdout)
        .trim()
        .parse()
        .map_err(e)?;
    Ok(version < 404)
}

/// The marks set off none of the user's traps: a DEBUG trap sees one
/// command of fux's startup, then only the command typed, an ERR trap
/// counts a failure once; a PROMPT_COMMAND set anew in the running shell
/// leaves them marked; a readonly PS1 is left alone.
#[test]
fn bash_marks_leave_the_users_traps_and_hooks_alone() -> Outcome {
    let server = Server::start("")?;
    let home = server.dir.join("home");
    std::fs::create_dir_all(&home).map_err(e)?;
    let rc = "PS1='mine> '\nE=0\ntrap 'E=$((E+1))' ERR\n\
              trap 'printf \"DBG %s\\n\" \"$BASH_COMMAND\"' DEBUG\n";
    std::fs::write(home.join(".bashrc"), rc).map_err(e)?;
    let home = home.to_string_lossy().into_owned();
    server.ok(&run(
        &["str", "t", "name", "str", "HOME", "str", &home, "env"],
        &["bash"],
    ))?;
    // Of fux's startup the DEBUG trap sees one command: the call that sets
    // the marks; before bash 4.4 (macOS's 3.2) also the line after it that
    // sets fux's trap, as nothing a function sets outlasts its call there.
    let first = server.ok(&["str", "t", "pane", "opts", "capture"])?;
    let fuxs = [
        "fux",
        "BASH_VERSINFO",
        "PS0",
        "PS1",
        "PROMPT_COMMAND",
        "export",
    ];
    let seen: Vec<&str> = (first.lines())
        .filter(|l| l.starts_with("DBG") && fuxs.iter().any(|f| l.contains(f)))
        .collect();
    let (call, trap) = seen.split_first().ok_or(first.clone())?;
    assert_eq!(*call, "DBG __fux_marks", "{first}");
    assert_eq!(trap.len(), usize::from(old_bash()?), "{first}");
    assert!(
        trap.iter().all(|l| l.starts_with("DBG trap -- ")),
        "{first}"
    );
    line(&server, "t", "echo hi")?;
    assert_eq!(last_output(&server, "t")?, "DBG echo hi\nhi\n");
    assert_eq!(done(&line(&server, "t", "false")?), Some("done 1"));
    line(&server, "t", "echo E=$E")?;
    assert_eq!(last_output(&server, "t")?, "DBG echo E=$E\nE=1\n");
    line(&server, "t", "PROMPT_COMMAND=:")?;
    assert_eq!(done(&line(&server, "t", "(exit 3)")?), Some("done 3"));
    line(&server, "t", "unset PROMPT_COMMAND")?;
    // `true` first: with PS1's marks alone, bash before 4.4 starts a
    // command from its trap, which a subshell alone does not set off.
    assert_eq!(done(&line(&server, "t", "true; (exit 5)")?), Some("done 5"));
    // A readonly PS1 is left as it is, with no complaint, and the marks
    // come from PROMPT_COMMAND.
    let ro = server.dir.join("ro");
    std::fs::create_dir_all(&ro).map_err(e)?;
    std::fs::write(ro.join(".bashrc"), "readonly PS1='ro> '\n").map_err(e)?;
    let ro = ro.to_string_lossy().into_owned();
    server.ok(&run(
        &["str", "ro", "name", "str", "HOME", "str", &ro, "env"],
        &["bash"],
    ))?;
    assert_eq!(done(&line(&server, "ro", "(exit 2)")?), Some("done 2"));
    let screen = server.ok(&["str", "ro", "pane", "opts", "capture"])?;
    assert!(
        screen.contains("ro> ") && !screen.contains("readonly"),
        "{screen}"
    );
    Ok(())
}

/// What the agents' rechecks found: Ctrl-C ends a command, with fewer
/// command ends than lines; a command still at work at the limit answers
/// `busy`; a command typed while another runs answers for its own end;
/// `clear` starts a command's output again; a split or resize that would
/// hide a pane is refused; a short typed line is its echo only as a word
/// of its own.
#[test]
fn the_rechecks_hold() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(&["str", "sh", "name"], &["bash", "--norc", "-i"]))?;
    // Elsewhere than Linux, quiet, it is taken as settled.
    let at_work = server.fux(&typed("sh", "sleep 30", &["1", "timeout"]))?;
    let status = if PROBED { 124 } else { 0 };
    assert_eq!(at_work.status, status, "{}", at_work.stderr);
    assert!(
        head(&at_work.stdout).ends_with(&format!(" {}", asleep("%2"))),
        "{}",
        at_work.stdout
    );
    let stopped = server.ok(&[
        "str",
        "sh",
        "pane",
        "keys",
        "str",
        "C-c",
        "key",
        "send-keys",
    ])?;
    assert_eq!(done(&stopped), Some("done 130"), "{stopped}");
    // Each command's end is its own: the next answers for the next.
    assert_eq!(done(&line(&server, "sh", "(exit 3)")?), Some("done 3"));
    assert_eq!(done(&line(&server, "sh", "true")?), Some("done 0"));
    line(&server, "sh", "clear; printf 'a\\nb\\n'")?;
    assert_eq!(last_output(&server, "sh")?, "a\nb\n");
    let small = id(&server.ok(&run(
        &["str", "small", "name", "10", "5", "size", "0", "timeout"],
        &["sleep", "60"],
    ))?);
    let split = [
        &[
            "str",
            small.as_str(),
            "pane",
            "opts",
            "vertical",
            "0",
            "timeout",
        ][..],
        &program(&["sleep", "60"]),
        &["0", "timeout", "split"],
    ]
    .concat();
    server.ok(&split)?;
    let full = server.fux(&split)?;
    assert!(full.stderr.contains("no room"), "{}", full.stderr);
    let tiny = server.fux(&["str", "small", "tab", "10", "2", "resize"])?;
    assert!(
        tiny.stderr.contains("cannot show its 2 panes"),
        "{}",
        tiny.stderr
    );
    // Typed `a` is not echoed by the `a` in `answer: A`.
    let ask = r#"read l; echo "answer: $(echo $l | tr a-z A-Z)"; exec cat"#;
    server.ok(&run(&["str", "ask", "name"], &["sh", "-c", ask]))?;
    assert_eq!(
        server.ok(&typed(
            "ask",
            "a",
            &["str", "answer:", "wait-text", "5", "timeout"]
        ))?,
        "answer: A\n"
    );
    Ok(())
}

/// A prompt waited for after typing a command is the next one, drawn once
/// the command ends, not the line the command was typed on: what a program
/// writes just before it reads its terminal is seen before it is taken to
/// be reading without it.
#[test]
fn a_prompt_waited_for_is_the_next_one() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(
        &["str", "sh", "name", "str", "PS1", "str", "mine> ", "env"],
        &["bash", "--norc", "-i"],
    ))?;
    let started = std::time::Instant::now();
    let prompt = server.ok(&typed("sh", "sleep 0.5", &["str", "mine>", "wait-text"]))?;
    assert_eq!(prompt, "mine>\n");
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(400),
        "not the echo"
    );
    Ok(())
}

/// Output printed after a `clear`, on the row the command was typed on, is
/// output, not the command's echo.
#[test]
fn output_after_a_clear_is_not_the_echo() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(&["str", "sh", "name"], &["bash", "--norc", "-i"]))?;
    let ready = server.ok(&typed(
        "sh",
        "clear; echo READY",
        &["str", "READY", "wait-text"],
    ))?;
    assert_eq!(ready, "READY\n");
    Ok(())
}

/// A program that echoes nothing writes its answer on the row the keys
/// were typed on: that is its output, not their echo; so is a line it
/// writes over the echo, and the next line after keys alone.
#[test]
fn a_program_that_echoes_nothing_is_heard() -> Outcome {
    let server = Server::start("")?;
    let quiet = "stty -echo; echo 'reply now'; read l; echo 'CLOSED cleanly'; \
                 read l; echo NEXT; exec cat";
    server.ok(&run(
        &["str", "q", "name", "str", "reply now", "wait-text"],
        &["sh", "-c", quiet],
    ))?;
    let closed = server.ok(&typed("q", "shutdown x", &["str", "CLOSED", "wait-text"]))?;
    assert_eq!(closed, "CLOSED cleanly\n");
    let keys = ["str", "q", "pane", "keys", "str", "Enter", "key"];
    let next = server.ok(&[&keys[..], &["str", "NEXT", "wait-text", "send-keys"]].concat())?;
    assert_eq!(next, "NEXT\n");
    let over =
        "stty -onlcr; echo 'WATCH READY'; read l; printf '\\r\\033[2KPARKED t-1\\r\\n'; exec cat";
    server.ok(&run(
        &["str", "o", "name", "str", "WATCH READY", "wait-text"],
        &["sh", "-c", over],
    ))?;
    let parked = server.ok(&typed("o", "park t-1", &["str", "PARKED", "wait-text"]))?;
    assert_eq!(parked, "PARKED t-1\n");
    Ok(())
}

/// Settling a workspace settles each of its panes; `split` and `respawn`
/// answer as `run` does, a respawned program's wait counting from its own
/// start; a met text answers the first line that held it.
#[test]
fn every_pane_and_every_start_is_waited_for() -> Outcome {
    let server = Server::start("")?;
    for s in ["0.1", "1"] {
        let late = format!("sleep {s}; echo done-{s}; exec cat");
        server.ok(&run(&["0", "timeout"], &["sh", "-c", &late]))?;
    }
    let all = server.ok(&["str", "agent", "workspace", "opts", "wait"])?;
    assert!(all.contains("done-0.1") && all.contains("done-1"), "{all}");
    let pane = id(&server.ok(&run(&[], &["sh", "-c", "read x; echo got"]))?);
    server.ok(&typed(&pane, "hi", &[]))?;
    let again = "echo 'value 7'; echo 'value>'; exec cat";
    let respawned = server.ok(&[
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        again,
        "arg",
        "str",
        "value",
        "wait-text",
        "respawn",
    ])?;
    assert_eq!(respawned, format!("{pane}\nvalue 7\n"));
    let split = server.ok(&[
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "sleep 0.3; echo SPLIT; exec cat",
        "arg",
        "split",
    ])?;
    // Not told the program sleeps (not Linux), fux takes 300ms of quiet as
    // settled, which the sleep outlasts: there a wait for its text follows.
    if PROBED {
        assert!(split.ends_with(" reading\nSPLIT\n"), "{split}");
    } else {
        let pane = id(&split);
        let wait = [
            "str",
            &pane,
            "pane",
            "opts",
            "str",
            "SPLIT",
            "wait-text",
            "wait",
        ];
        assert_eq!(server.ok(&wait)?, "SPLIT\n");
    }
    Ok(())
}

/// A menu redrawn in place, above the row the keys were typed on, is in
/// the answer: what came is said from the topmost row that changed.
#[test]
fn a_menu_redrawn_above_is_answered() -> Outcome {
    let server = Server::start("")?;
    let menu = "printf '> one\\n  two\\nchoose: '; read -r x; \
                printf '\\033[2A\\r  one\\033[K\\n> two\\033[K\\nchoose: \\033[K'; exec cat";
    server.ok(&run(
        &["str", "m", "name", "str", "choose:", "wait-text"],
        &["sh", "-c", menu],
    ))?;
    let moved = server.ok(&typed("m", "j", &[]))?;
    assert!(moved.contains("  one\n> two\n"), "{moved}");
    Ok(())
}

/// A wait of its own after a separate input knows that input's echo: it
/// answers the program's line, not the typed command that holds the text.
#[test]
fn a_later_wait_skips_the_last_inputs_echo() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(&["str", "sh", "name"], &["bash", "--norc", "-i"]))?;
    let start = [
        "str",
        "sh",
        "pane",
        "keys",
        "str",
        "sleep 0.5; echo READY",
        "text",
        "str",
        "Enter",
        "key",
        "0",
        "timeout",
        "send-keys",
    ];
    server.ok(&start)?;
    let ready = server.ok(&[
        "str",
        "sh",
        "pane",
        "opts",
        "str",
        "READY",
        "wait-text",
        "wait",
    ])?;
    assert_eq!(ready, "READY\n");
    Ok(())
}

/// Joined, a row range takes each line it touches whole; a column range
/// cuts a wide glyph to a blank, and reaches the last column.
#[test]
fn a_row_range_takes_joined_lines_whole() -> Outcome {
    let server = Server::start("")?;
    let long = "echo aaaaaaaaaaaaaaaaaaaaBBBBBBBBBBBBBBBBBBBBcc; echo next; exec cat";
    let pane = id(&server.ok(&run(&["20", "6", "size"], &["sh", "-c", long]))?);
    let line = server.ok(&[
        "str", &pane, "pane", "opts", "1", "1", "rows", "joined", "capture",
    ])?;
    assert_eq!(body(&line), "aaaaaaaaaaaaaaaaaaaaBBBBBBBBBBBBBBBBBBBBcc\n");
    // A column range cuts a wide glyph to a blank, a column a cell.
    let wide = id(&server.ok(&run(
        &["20", "6", "size"],
        &["sh", "-c", "printf 'A界BC              Z'; exec cat"],
    ))?);
    let cut = |first: &str, last: &str| {
        let opts = [
            "str", &wide, "pane", "opts", "0", "0", "rows", first, last, "cols", "capture",
        ];
        server.ok(&opts).map(|c| body(&c).to_owned())
    };
    assert_eq!(cut("2", "4")?, " BC\n");
    assert_eq!(cut("0", "1")?, "A\n");
    assert_eq!(cut("19", "19")?, "Z\n");
    // The cursor, on the empty row after the last line, is on none printed.
    let json = server.ok(&["str", &pane, "pane", "opts", "json", "capture"])?;
    assert!(json.contains(r#""cursor":null,"#), "{json}");
    Ok(())
}

/// The key after a lone Escape waits until the program has taken the
/// Escape alone (here after 120ms), so it is not read as Alt and that key,
/// or a second at most; a program that leaves its full screen and exits
/// answers with that screen.
#[test]
fn escape_alone_and_full_screens() -> Outcome {
    let server = Server::start("")?;
    let esc = "import os, select, tty\n\
               tty.setraw(0); os.write(1, b'ready\\r\\n')\n\
               while True:\n\
               \x20   b = os.read(0, 1)\n\
               \x20   if b == b'\\x1b':\n\
               \x20       alt = select.select([0], [], [], 0.12)[0]\n\
               \x20       os.write(1, b'ALT\\r\\n' if alt else b'ESC alone\\r\\n')\n\
               \x20       if alt: os.read(0, 1)\n\
               \x20   elif b == b'\\x13': os.write(1, b'CTRL-S\\r\\n'); break\n";
    let pane = id(&server.ok(&run(
        &["str", "ready", "wait-text"],
        &["python3", "-c", esc],
    ))?);
    let keys = server.ok(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "Escape",
        "key",
        "str",
        "C-s",
        "key",
        "send-keys",
    ])?;
    assert!(keys.contains("ESC alone\nCTRL-S\n"), "{keys}");
    // One that only polls for keys never waits for them with no limit: the
    // key after the Escape goes in once the Escape's second is up.
    let polls = "import os, select, tty\n\
                 tty.setraw(0); os.write(1, b'ready\\r\\n')\n\
                 while True:\n\
                 \x20   if select.select([0], [], [], 0.05)[0] and os.read(0, 1) == b'x':\n\
                 \x20       os.write(1, b'GOT X\\r\\n')\n";
    let polling = id(&server.ok(&run(
        &["str", "ready", "wait-text"],
        &["python3", "-c", polls],
    ))?);
    let got = server.ok(&[
        "str",
        &polling,
        "pane",
        "keys",
        "str",
        "Escape",
        "key",
        "str",
        "x",
        "text",
        "str",
        "GOT X",
        "wait-text",
        "send-keys",
    ])?;
    assert!(got.ends_with("GOT X\n"), "{got}");
    let full = "printf '\\033[?1049hEDIT> '; read x; echo saved; sleep 0.2; printf '\\033[?1049l'";
    let editor = id(&server.ok(&run(&["str", "EDIT>", "wait-text"], &["sh", "-c", full]))?);
    // Waited to its exit: where a quiet screen is taken as settled (not
    // Linux), the 0.2s before it would answer first.
    let saved = server.ok(&typed(&editor, "q", &["wait-exit"]))?;
    assert!(
        saved.starts_with(&format!("{editor} exited 0\n")) && saved.contains("saved"),
        "{saved}"
    );
    Ok(())
}

/// A wait with a condition on a workspace waits on each pane, as a bare
/// one settles each: met once every pane meets it, a line of each.
#[test]
fn a_condition_on_a_workspace_is_met_in_each_pane() -> Outcome {
    let server = Server::start("")?;
    let mut panes = Vec::new();
    for pause in ["0", "0.5", "1"] {
        let start = ["str", "g", "in", "0", "timeout"];
        let made = server.ok(&run(
            &start,
            &["sh", "-c", &format!("sleep {pause}; echo E")],
        ))?;
        panes.push(id(&made));
    }
    let words = ["str", "g", "workspace", "opts", "wait-exit", "wait"];
    let exited = server.ok(&words)?;
    for pane in &panes {
        assert!(exited.contains(&format!("{pane} exited 0\n")), "{exited}");
    }
    Ok(())
}

/// A pane that may go on alone (`waiting`: a pause that polls for keys)
/// is waited on by a later `wait` until something comes: its prompt here.
#[test]
fn a_later_wait_outlasts_a_pause() -> Outcome {
    let server = Server::start("")?;
    let pause = "import select, sys, time, tty\n\
                 tty.setcbreak(0); print('loading', flush=True); end = time.time() + 2.5\n\
                 while time.time() < end:\n\
                 \x20   if select.select([0], [], [], 0.1)[0]: sys.exit('early input')\n\
                 print('PROMPT>', flush=True); sys.stdin.read(1)\n";
    let started = server.ok(&run(&[], &["python3", "-c", pause]))?;
    let pane = id(&started);
    let waiting = if PROBED { "waiting" } else { "settled" };
    assert!(
        started.starts_with(&format!("{pane} {waiting}")),
        "{started}"
    );
    let later = server.ok(&["str", &pane, "pane", "opts", "wait"])?;
    assert_eq!(later, format!("{pane} {READING}\nloading\nPROMPT>\n"));
    Ok(())
}

/// A signal that ends the program answers how it ended, even after the
/// pane has been quiet a while.
#[test]
fn a_signal_that_ends_the_program_says_so() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&[], &["sh", "-c", "read x"]))?);
    // Quiet a while: the program reads, the wait for its end runs out.
    let quiet = server.fux(&[
        "str",
        &pane,
        "pane",
        "opts",
        "wait-exit",
        "1",
        "timeout",
        "wait",
    ])?;
    assert_eq!(quiet.status, 124, "{}", quiet.stderr);
    let killed = server.ok(&["str", &pane, "pane", "str", "KILL", "signal"])?;
    assert_eq!(killed, format!("{pane} killed by SIGKILL\n"));
    Ok(())
}

/// A text wait sees every line written since it began, however fast they
/// pass: past history's budget between two looks, or off the alternate
/// screen, which keeps none. Of several texts already shown, the one that
/// came first answers. An answer whose start was let go says so.
#[test]
fn a_wait_misses_no_line_however_fast_it_passes() -> Outcome {
    let server = Server::start("str history-bytes 2000 set")?;
    let flood = "import os; os.write(1, b'FIRST_READY\\n' + b'filler\\n' * 5000); input()";
    let first = server.fux(&run(
        &["str", "FIRST_READY", "wait-text"],
        &["python3", "-c", flood],
    ))?;
    assert_eq!(first.status, 0, "{}{}", first.stdout, first.stderr);
    assert_eq!(body(&first.stdout), "FIRST_READY\n");
    let full = "printf '\\033[?1049h'; echo START SCROLLED MATCH; seq 100; read x";
    let scrolled = server.ok(&run(
        &["40", "10", "size", "str", "SCROLLED MATCH", "wait-text"],
        &["sh", "-c", full],
    ))?;
    assert_eq!(body(&scrolled), "START SCROLLED MATCH\n");
    // Lines that scroll off between two looks, after some already had.
    let later = "import os, time\nos.write(1, b'\\x1b[?1049h' + b'x\\n' * 30); time.sleep(1)\n\
                 os.write(1, b'LATE MATCH\\n' + b'y\\n' * 30); input()\n";
    let late = server.ok(&run(
        &["40", "10", "size", "str", "LATE MATCH", "wait-text"],
        &["python3", "-c", later],
    ))?;
    assert_eq!(body(&late), "LATE MATCH\n");
    let two = "echo FIRST one; sleep 0.2; echo SECOND two; read x";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", two]))?);
    let came = server.ok(&[
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "SECOND",
        "wait-text",
        "str",
        "FIRST",
        "wait-text",
        "wait",
    ])?;
    assert_eq!(came, "FIRST one\n");
    let long = server.ok(&run(&[], &["sh", "-c", "seq 100; read x"]))?;
    assert!(
        body(&long).starts_with("… earlier lines not kept") && long.ends_with("\n100\n"),
        "{long}"
    );
    Ok(())
}

/// A later wait counts what came since the last input, a click or a bell
/// included, or since the program started: an old line is not new to it,
/// a bell rung since is.
#[test]
fn a_later_wait_counts_from_any_input() -> Outcome {
    let server = Server::start("")?;
    let mouse = "import os, tty\n\
                 tty.setraw(0); os.write(1, b'\\x1b[?1000h\\x1b[?1006hOLD43\\r\\n')\n\
                 while True: os.read(0, 64); os.write(1, b'EVENT43\\r\\n')\n";
    let pane = id(&server.ok(&run(
        &["str", "OLD43", "wait-text"],
        &["python3", "-c", mouse],
    ))?);
    server.ok(&[
        "str",
        &pane,
        "pane",
        "str",
        "click",
        "0",
        "0",
        "mouse",
        "send-mouse",
    ])?;
    let later = server.ok(&["str", &pane, "pane", "opts", "wait"])?;
    assert!(
        later.contains("EVENT43") && !later.contains("OLD43"),
        "{later}"
    );
    let old = server.fux(&[
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "OLD43",
        "wait-text",
        "2",
        "timeout",
        "wait",
    ])?;
    assert_eq!(old.status, CANNOT_COME, "{}{}", old.stdout, old.stderr);
    let bells = "printf '\\a'; echo rang; read x; printf '\\aDONE\\n'; exec cat";
    let rang = id(&server.ok(&run(&[], &["sh", "-c", bells]))?);
    let bell = [
        "str",
        &rang,
        "pane",
        "opts",
        "wait-bell",
        "2",
        "timeout",
        "wait",
    ];
    assert_eq!(server.fux(&bell)?.status, 0, "a bell since the start");
    server.ok(&typed(&rang, "x", &["str", "DONE", "wait-text"]))?;
    assert_eq!(server.fux(&bell)?.status, 0, "a bell since the input");
    Ok(())
}

/// The echo is only what was typed: a reply the program writes after it
/// on the same row (its terminal not echoing) is output.
#[test]
fn a_reply_after_the_typed_text_is_not_its_echo() -> Outcome {
    let server = Server::start("")?;
    let reply = "stty -echo; echo ready; read l; echo \"$l DONE42\"; exec cat";
    server.ok(&run(
        &["str", "q", "name", "str", "ready", "wait-text"],
        &["sh", "-c", reply],
    ))?;
    let done = server.ok(&typed("q", "request42", &["str", "DONE42", "wait-text"]))?;
    assert_eq!(done, "request42 DONE42\n");
    // Within what was typed, it is the echo; the hint says what works.
    server.ok(&run(&["str", "e", "name"], &["sh", "-c", "read l; exit 0"]))?;
    let echoed = server.fux(&typed("e", "exit46", &["str", "exit46", "wait-text"]))?;
    assert_eq!(echoed.status, 3, "{}", echoed.stderr);
    assert!(
        echoed.stderr.contains("PANE opts capture"),
        "{}",
        echoed.stderr
    );
    Ok(())
}

/// What came is what a program wrote since the input: what it printed on
/// the main screen after leaving its full screen, and nothing when the
/// keys only moved the cursor.
#[test]
fn an_answer_holds_only_what_came() -> Outcome {
    let server = Server::start("")?;
    let menu = "printf '\\033[?1049hMENU> '; read x; printf '\\033[?1049l'; echo AFTER-MAIN; \
                exec cat";
    let pane = id(&server.ok(&run(
        &["40", "10", "size", "str", "MENU>", "wait-text"],
        &["sh", "-c", menu],
    ))?);
    let left = server.ok(&typed(&pane, "q", &[]))?;
    assert!(
        left.contains("AFTER-MAIN") && !left.contains("MENU"),
        "{left}"
    );
    // Left with nothing printed after, nor drawn for the keys: the screen
    // as it is, not the full screen it left (1047's, kept, too).
    for mode in ["1049", "1047"] {
        let menu = format!(
            "stty -echo; echo MAIN-{mode}; printf '\\033[?{mode}hMENU> '; read x; \
             printf '\\033[?{mode}l'; exec cat"
        );
        let pane = id(&server.ok(&run(
            &["40", "10", "size", "str", "MENU>", "wait-text"],
            &["sh", "-c", &menu],
        ))?);
        let left = server.ok(&typed(&pane, "q", &[]))?;
        assert!(
            left.contains(&format!("MAIN-{mode}")) && !left.contains("MENU"),
            "{left}"
        );
        let alt = server.ok(&["str", &pane, "pane", "opts", "alt-screen", "capture"])?;
        assert!(alt.contains("MENU>"), "{alt}");
    }
    let quiet = id(&server.ok(&run(&[], &["sh", "-c", "echo BEFORE-INPUT; read x"]))?);
    let keys = [
        "str",
        &quiet,
        "pane",
        "keys",
        "str",
        "Enter",
        "key",
        "send-keys",
    ];
    assert_eq!(server.ok(&keys)?, format!("{quiet} exited 0\n"));
    Ok(())
}

/// What came around a full screen: on a main screen filled to its last
/// row, every line from the input on; after the keys went in on a full
/// screen the program left, what it printed after, else the full screen it
/// drew since; a text that cannot come shows the screen, or the last full
/// screen while the screen is blank; and a bare wait with nothing new says
/// nothing came.
#[test]
fn answers_around_a_full_screen_say_what_came() -> Outcome {
    let server = Server::start("")?;
    let small = ["40", "5", "size"];
    let echo = "import sys\nfor i in range(10): print(i)\n\
                while True: print('got ' + input('> '), flush=True)\n";
    let full = id(&server.ok(&run(&small, &["python3", "-c", echo]))?);
    let got = server.ok(&typed(&full, "a", &[]))?;
    assert_eq!(body(&got), "> a\ngot a\n>\n", "{got}");
    let printed = "import sys\nw = sys.stdout.write\nprint('before', flush=True)\n\
                   w('\\x1b[?1049hMENU\\r\\n'); sys.stdout.flush(); input()\n\
                   w('\\x1b[?1049l'); print('SAVED', flush=True); input()\n";
    let pane = id(&server.ok(&run(
        &["str", "MENU", "wait-text"],
        &["python3", "-c", printed],
    ))?);
    let after = server.ok(&typed(&pane, "y", &[]))?;
    assert_eq!(body(&after), "SAVED\n", "{after}");
    let drawn = "import sys\nw = sys.stdout.write\nprint('before', flush=True)\n\
                 w('\\x1b[?1049hMENU\\r\\n'); sys.stdout.flush(); input()\n\
                 w('SAVED\\x1b[?1049l'); sys.stdout.flush(); input()\n";
    let pane = id(&server.ok(&run(
        &["str", "MENU", "wait-text"],
        &["python3", "-c", drawn],
    ))?);
    let last = server.ok(&typed(&pane, "y", &[]))?;
    assert!(last.contains("SAVED") && last.contains("MENU"), "{last}");
    let blank = r"printf '\033[?1049hMENU\033[?1049l'; read x";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", blank]))?);
    let p = ["str", pane.as_str(), "pane", "opts"];
    let nothing = server.ok(&[&p[..], &["wait"]].concat())?;
    assert_eq!(nothing, format!("{pane} {READING}\n"));
    let never = server.fux(
        &[
            &p[..],
            &["str", "NEVER", "wait-text", "2", "timeout", "wait"],
        ]
        .concat(),
    )?;
    assert_eq!(never.status, CANNOT_COME, "{}", never.stderr);
    // Status 7 answers with the screen; a time limit (not Linux) says why on
    // stderr alone.
    if PROBED {
        assert!(never.stdout.contains("MENU"), "{}", never.stdout);
    }
    Ok(())
}

/// `run`'s answer names its pane once, a state answer too. A wait on a
/// tab, settling or not, or an action on it, answers of each pane, a line
/// each, with the first failing status and each one's reason.
#[test]
fn answers_name_the_pane_once_and_the_busy_one() -> Outcome {
    let server = Server::start("")?;
    let stopped = server.fux(&run(
        &["str", "READY", "wait-text", "3", "timeout"],
        &["sh", "-c", "echo hi; read x"],
    ))?;
    assert_eq!(stopped.status, CANNOT_COME, "{}", stopped.stderr);
    let pane = id(&stopped.stdout);
    let said = if PROBED {
        format!("{pane} reading")
    } else {
        pane.clone()
    };
    assert_eq!(head(&stopped.stdout), said);
    let reader = id(&server.ok(&run(&[], &["sh", "-c", "echo R; read x"]))?);
    let busy = id(&server.ok(&[
        "str",
        &reader,
        "pane",
        "opts",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "echo B; exec sleep 30",
        "arg",
        "0",
        "timeout",
        "split",
    ])?);
    let out = server.fux(&[
        "str",
        &reader,
        "pane",
        "tab",
        "opts",
        "str",
        "NEVER",
        "wait-text",
        "1",
        "timeout",
        "wait",
    ])?;
    // Each answers: the reader cannot get the text (where fux is told it
    // reads), the one at work runs out of time; the reader's is first.
    let first = if PROBED { CANNOT_COME } else { 124 };
    assert_eq!(out.status, first, "{}", out.stderr);
    assert!(
        out.stderr.contains(&format!("{busy}: timed out")),
        "{}",
        out.stderr
    );
    let tab = ["str", reader.as_str(), "pane", "tab"];
    let settle = [&tab[..], &["opts", "1", "timeout", "wait"]].concat();
    let each = server.fux(&settle)?;
    assert_eq!(each.status, if PROBED { 124 } else { 0 }, "{}", each.stdout);
    let heads: Vec<String> = each.stdout.lines().map(id).collect();
    assert!(
        heads.contains(&reader) && heads.contains(&busy),
        "{}",
        each.stdout
    );
    let json = server.fux(&[&settle[..], &["json"]].concat())?.stdout;
    assert_eq!(json.lines().count(), 2, "{json}");
    // Keys to both: one reads them and the text cannot come; the other,
    // asleep, runs out of time, its reason on stderr (no hint: no line
    // reads as it did with the text in it).
    let keys = [
        &tab[..],
        &[
            "keys",
            "str",
            "y",
            "text",
            "str",
            "NEVER",
            "wait-text",
            "1",
            "timeout",
            "send-keys",
        ],
    ]
    .concat();
    let both = server.fux(&keys)?;
    assert_eq!(both.status, CANNOT_COME, "{}", both.stdout);
    // The first failing status names it: the reader's.
    let code = if PROBED { "stopped" } else { "timeout" };
    assert!(
        both.stderr.starts_with(&format!("fux: {code}: ")),
        "{}",
        both.stderr
    );
    let reason = format!("{busy}: timed out after 1s waiting for \"NEVER\" to show");
    assert!(both.stderr.contains(&reason), "{}", both.stderr);
    assert!(
        both.stderr.trim_end().ends_with("last line: `y`"),
        "{}",
        both.stderr
    );
    // None at work at the limit: the one still running is named, not the
    // one that exited before it.
    let ended = id(&server.ok(&run(&["wait-exit"], &["true"]))?);
    let still = id(&server.ok(&[
        "str", &ended, "pane", "opts", "str", "cat", "program", "0", "timeout", "split",
    ])?);
    let bell = server.fux(&[
        "str",
        &ended,
        "pane",
        "tab",
        "opts",
        "wait-bell",
        "1",
        "timeout",
        "wait",
    ])?;
    assert!(
        bell.stderr.contains(&format!("{still}: timed out")),
        "{}",
        bell.stderr
    );
    // A text on a line that reads as it did when the wait began: said so.
    let shown = id(&server.ok(&run(
        &["str", "READY", "wait-text"],
        &["sh", "-c", "echo READY; exec sleep 30"],
    ))?);
    let again = server.fux(&[
        "str",
        &shown,
        "pane",
        "keys",
        "str",
        "y",
        "text",
        "str",
        "READY",
        "wait-text",
        "1",
        "timeout",
        "send-keys",
    ])?;
    assert_eq!(again.status, 124, "{}", again.stdout);
    assert!(
        again.stderr.contains("on a line that reads as it did"),
        "{}",
        again.stderr
    );
    Ok(())
}

/// A met text wait's answer is its pane: a word that wants a pane takes it,
/// never a pane named by what the program wrote (a line `%13 fake`). What
/// `cat` writes back is output, though it reads as what was typed.
#[test]
fn a_met_waits_answer_is_its_pane() -> Outcome {
    let server = Server::start("")?;
    let fake = "read x; echo '%13 fake'; exec cat";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", fake]))?);
    let met = [
        &typed(&pane, "go", &["str", "fake", "wait-text"])[..],
        &["str", "name", "get"],
    ];
    assert_eq!(server.ok(&met.concat())?.trim(), "sh");
    let wait = [
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "fake",
        "wait-text",
        "wait",
        "str",
        "name",
        "get",
    ];
    assert_eq!(server.ok(&wait)?.trim(), "sh");
    let captured = [&wait[..8], &["opts", "capture"]].concat();
    let screen = server.ok(&captured)?;
    assert!(screen.starts_with(&format!("{pane} ")), "{screen}");
    let cat = id(&server.ok(&run(&[], &["cat"]))?);
    let back = [
        &typed(&cat, "hi", &["str", "hi", "wait-text"])[..],
        &["str", "x", "line"],
    ];
    assert_eq!(
        server.ok(&back.concat())?,
        format!("{cat} {READING}\nx\nx\n")
    );
    Ok(())
}

/// Input as large as a paste may be (900 KiB) goes in one send-keys, all
/// of it; more is refused as too long, before anything is sent.
#[test]
fn send_keys_takes_input_up_to_its_limit() -> Outcome {
    let server = Server::start("")?;
    let file = server.dir.join("paste.txt");
    let line: String = std::iter::repeat_n('a', 99).chain(['\n']).collect();
    std::fs::write(&file, std::iter::repeat_n(line, 9216).collect::<String>()).map_err(e)?;
    let count = "stty -echo; head -c 921600 | wc -c; cat > /dev/null";
    let sink = id(&server.ok(&run(&[], &["sh", "-c", count]))?);
    let path = file.to_string_lossy();
    let paste = [
        "str",
        &sink,
        "pane",
        "keys",
        "str",
        &path,
        "paste-file",
        "str",
        "921600",
        "wait-text",
        "send-keys",
    ];
    let pasted = server.fux(&paste)?;
    assert_eq!(pasted.status, 0, "{}{}", pasted.stdout, pasted.stderr);
    let long: String = std::iter::repeat_n('b', 1000).collect();
    let many = [
        "str",
        &sink,
        "pane",
        "keys",
        "str",
        &long,
        "text",
        "1000",
        "times",
        "send-keys",
    ];
    let refused = server.fux(&many)?;
    assert_eq!(refused.status, 1);
    assert!(refused.stderr.contains("too_long"), "{}", refused.stderr);
    // Keys count as the request carries them, their names and more: so
    // many that the request could not be sent are too long too.
    let mut keys = vec!["str", &sink, "pane", "keys"];
    for _ in 0..90 {
        keys.extend(["str", "C-M-F12", "key", "1000", "times"]);
    }
    keys.push("send-keys");
    let refused = server.fux(&keys)?;
    assert_eq!(refused.status, 1);
    assert!(refused.stderr.contains("too_long"), "{}", refused.stderr);
    Ok(())
}

/// A menu on the main screen that marks its choice by colour alone: what a
/// key that moves it brought starts at the first row whose colours
/// changed, though it reads as it did.
#[test]
fn a_highlight_moved_is_what_came() -> Outcome {
    let server = Server::start("")?;
    let menu = r"import os, tty
tty.setraw(0)
sel = 0
def draw():
    os.write(1, b'\x1b[H')
    for i, label in enumerate(['Alpha', 'Beta', 'Gamma']):
        style = '\x1b[7m' if i == sel else ''
        os.write(1, ('\x1b[2K' + style + label + '\x1b[0m\r\n').encode())
    os.write(1, b'\x1b[2KINPUT: arrows')
os.write(1, b'\x1b[2J')
draw()
while True:
    if os.read(0, 3) == b'\x1b[B':
        sel += 1
    draw()
";
    let pane = id(&server.ok(&run(
        &["str", "INPUT:", "wait-text"],
        &["python3", "-c", menu],
    ))?);
    let down = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "Down",
        "key",
        "send-keys",
    ];
    let moved = server.ok(&down)?;
    assert_eq!(
        body(&moved),
        "Alpha\nBeta\nGamma\nINPUT: arrows\n",
        "{moved}"
    );
    Ok(())
}

/// Keys that type nothing: a command recalled (Up, Enter) shows whole on
/// the prompt's row, its echo, not new text; a key the program answers on
/// the prompt's row, what it writes there is new.
#[test]
fn keys_that_type_nothing_leave_the_prompts_row_to_the_program() -> Outcome {
    let server = Server::start("")?;
    let shell = id(&server.ok(&run(&[], &["bash", "--norc", "-i"]))?);
    server.ok(&["str", &shell, "pane", "str", "echo RECALLED-ONE", "line"])?;
    let recalled = server.ok(&[
        "str",
        &shell,
        "pane",
        "keys",
        "str",
        "Up",
        "key",
        "str",
        "Enter",
        "key",
        "str",
        "RECALLED",
        "wait-text",
        "send-keys",
    ])?;
    assert_eq!(recalled.lines().last(), Some("RECALLED-ONE"), "{recalled}");
    let answer = "import sys, tty\ntty.setraw(0)\nprint('> ', end='', flush=True)\n\
                  sys.stdin.read(1); print('ANSWERED', end='', flush=True); sys.stdin.read(1)\n";
    let pane = id(&server.ok(&run(&[], &["python3", "-c", answer]))?);
    let key = server.fux(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "Escape",
        "key",
        "str",
        "ANSWERED",
        "wait-text",
        "send-keys",
    ])?;
    assert_eq!(key.status, 0, "{}{}", key.stdout, key.stderr);
    Ok(())
}

/// Typed text with no Enter is its echo too, on every row it spans, and
/// the prompt already there is no new text: a wait for it is never met by
/// the keys (7: the program reads), only by what the program writes.
#[test]
fn keys_with_no_enter_are_their_echo() -> Outcome {
    let server = Server::start("")?;
    let prompt = "import sys\nprint('PROMPT> ', end='', flush=True)\n\
                  for line in sys.stdin: print('ACCEPTED ' + line.strip(), flush=True)\n";
    let pane = id(&server.ok(&run(&[], &["python3", "-c", prompt]))?);
    let keys = |text: &str, wanted: &str| -> Result<Output, String> {
        server.fux(&[
            "str",
            &pane,
            "pane",
            "keys",
            "str",
            text,
            "text",
            "str",
            wanted,
            "wait-text",
            "2",
            "timeout",
            "send-keys",
        ])
    };
    assert_eq!(keys("ECHO_ONE", "ECHO_ONE")?.status, CANNOT_COME);
    assert_eq!(keys("X", "PROMPT")?.status, CANNOT_COME);
    let later = server.fux(&[
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "ECHO_ONE",
        "wait-text",
        "1",
        "timeout",
        "wait",
    ])?;
    assert_eq!(later.status, CANNOT_COME, "{}", later.stdout);
    let two = keys("\nCOMPLETE\nPARTIAL", "PARTIAL")?;
    assert_eq!(two.status, CANNOT_COME, "{}", two.stdout);
    let accepted = keys("\n", "ACCEPTED PARTIAL")?;
    assert_eq!(accepted.status, 0, "{}", accepted.stdout);
    // Enter alone, after the text: a program that writes over the typed
    // row writes output there.
    let over = "import sys, tty\ntty.setraw(0)\nw = sys.stdout.write\nw('> '); sys.stdout.flush()\n\
                while True:\n c = sys.stdin.read(1)\n w('\\rWORKING\\x1b[K' if c == '\\r' else c)\n \
                sys.stdout.flush()\n";
    let pane = id(&server.ok(&run(&[], &["python3", "-c", over]))?);
    server.ok(&["str", &pane, "pane", "str", "slow", "type"])?;
    let enter = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "Enter",
        "key",
        "str",
        "WORKING",
        "wait-text",
        "2",
        "timeout",
        "send-keys",
    ];
    assert_eq!(server.ok(&enter)?, "WORKING\n");
    Ok(())
}

/// A later text wait reads the lines let go past history's budget since
/// the input too, as the pane kept them; past what it keeps, it says they
/// were let go: the text may have come unseen.
#[test]
fn a_wait_says_when_lines_were_let_go_unread() -> Outcome {
    let server = Server::start("str history-bytes 2000 set  str journal-bytes 1048576 set")?;
    let wait = |pane: &str, text: &str| {
        let args = [
            "str",
            pane,
            "pane",
            "opts",
            "str",
            text,
            "wait-text",
            "3",
            "timeout",
            "wait",
        ];
        server.fux(&args)
    };
    let kept = id(&server.ok(&run(&[], &["sh", "-c", "echo GONE_LINE; seq 200; read x"]))?);
    let found = wait(&kept, "GONE_LINE")?;
    assert_eq!(
        (found.status, found.stdout.as_str()),
        (0, "GONE_LINE\n"),
        "{}",
        found.stderr
    );
    // Past what the pane's journal keeps (`journal-bytes`, 1 MiB here).
    let flood = "import sys; w = sys.stdout.write\nw('LOST_LINE\\n')\n\
                 w(('filler ' * 10 + '\\n') * 20000)\nw('END\\n'); sys.stdout.flush(); input()";
    let lost = id(&server.ok(&run(
        &["str", "END", "wait-text"],
        &["python3", "-c", flood],
    ))?);
    let out = wait(&lost, "LOST_LINE")?;
    assert_eq!(out.status, CANNOT_COME, "{}", out.stderr);
    // Told at once, the answer is the screen; at the limit, the error.
    let (said, why) = match PROBED {
        true => (&out.stdout, "more came than the pane's journal keeps"),
        false => (&out.stderr, "may have come unseen"),
    };
    assert!(said.contains(why), "{}{}", out.stdout, out.stderr);
    Ok(())
}

/// A form redrawn in place below the cursor's row is answered from the top
/// row that changed, not from the cursor's unchanged row.
#[test]
fn a_form_redrawn_below_is_answered_from_its_change() -> Outcome {
    let server = Server::start("")?;
    let form = "import sys, tty\ntty.setraw(0)\nw = sys.stdout.write\n\
                w('MAIN FORM\\r\\nVALUE: A\\r\\nSubmit with Enter\\r\\nFOOTER\\x1b[2;9H')\n\
                sys.stdout.flush(); sys.stdin.read(1)\n\
                w('\\x1b[3;1H\\x1b[2KSUBMITTED: A\\x1b[2;9H'); sys.stdout.flush()\n\
                sys.stdin.read(1)\n";
    let pane = id(&server.ok(&run(&[], &["python3", "-c", form]))?);
    let submitted = server.ok(&["str", &pane, "pane", "str", "Enter", "press"])?;
    assert_eq!(body(&submitted), "SUBMITTED: A\nFOOTER\n", "{submitted}");
    Ok(())
}

/// JSON answers carry the state line's notes: a shell command's end, keys
/// read that drew nothing.
#[test]
fn json_answers_carry_the_state_notes() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(&["str", "sh", "name"], &["bash", "--norc", "-i"]))?;
    let done = server.ok(&["str", "sh", "pane", "str", "(exit 4)", "line", "json"])?;
    assert!(done.contains("\"done\":4"), "{done}");
    let silent = "import sys, tty\ntty.setraw(0)\nprint('ready', end='', flush=True)\n\
                  while True: sys.stdin.read(1)\n";
    let pane = id(&server.ok(&run(&[], &["python3", "-c", silent]))?);
    let quiet = server.ok(&["str", &pane, "pane", "str", "x", "type", "json"])?;
    assert_eq!(quiet.contains("\"no_change\":true"), PROBED, "{quiet}");
    Ok(())
}

/// `json` on a saved capture converts what was captured: never a later
/// read of the pane, which may have moved on or closed.
#[test]
fn json_of_a_saved_capture_is_that_capture() -> Outcome {
    let server = Server::start("")?;
    let script = "echo BEFORE; read x; echo LA\"\"TER; read y";
    let pane = id(&server.ok(&run(&["str", "BEFORE", "wait-text"], &["sh", "-c", script]))?);
    let json = server.ok(&[
        "str",
        &pane,
        "pane",
        "let",
        "P",
        "P",
        "screen",
        "let",
        "C",
        "P",
        "keys",
        "str",
        "Enter",
        "key",
        "str",
        "LATER",
        "wait-text",
        "send-keys",
        "drop",
        "P",
        "kill",
        "C",
        "json",
    ])?;
    assert!(json.contains("BEFORE") && !json.contains("LATER"), "{json}");
    Ok(())
}

/// A word refusing a capture or a listing names it: an action's answer is
/// a pane, so "an answer" alone would mislead.
#[test]
fn a_refused_reading_is_named() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&[], &["sh", "-c", "read x"]))?);
    let capture = ["str", &pane, "pane", "screen"];
    for (reading, kind) in [(&capture[..], "a capture"), (&["ls"][..], "a listing")] {
        let refused =
            server.fux(&[reading, &["keys", "str", "x", "text", "send-keys"]].concat())?;
        assert_ne!(refused.status, 0, "{reading:?}");
        assert!(
            refused.stderr.contains(&format!("not {kind}")),
            "{}",
            refused.stderr
        );
    }
    Ok(())
}

/// In a list, an answer is as `json` gives it alone: a capture, one asked
/// as JSON too, and a listing are their objects.
#[test]
fn an_answer_in_a_list_is_its_object() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&[], &["sh", "-c", "read x"]))?);
    let capture = ["str", &pane, "pane", "opts"];
    let json = server.ok(&[
        &["list"][..],
        &capture,
        &["capture"],
        &capture,
        &["json", "capture", "ls", "end", "json"],
    ]
    .concat())?;
    let pane_object = format!("{{\"pane\":\"{pane}\",");
    assert!(
        json.starts_with(&format!("[{pane_object}"))
            && json.contains(&format!("}},{pane_object}"))
            && json.contains(",{\"workspaces\":"),
        "{json}"
    );
    Ok(())
}

/// A word refuses options it would not use, and makes nothing: a capture
/// waits for nothing, a wait starts and views nothing, a split takes its
/// tab's place, a run splits nothing and views nothing, a respawn keeps
/// its pane's name, size, place and ids.
#[test]
fn words_refuse_options_they_do_not_use() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&[], &["sh", "-c", "read x"]))?);
    let on = ["str", &pane, "pane", "opts"];
    let opts = ["opts"];
    let cat = ["str", "cat", "program"];
    for (before, unused, word) in [
        (&on[..], &["str", "x", "wait-text"][..], "capture"),
        (&on, &["100", "40", "size"], "capture"),
        (&on, &["str", "x", "name"], "wait"),
        (&on, &["10", "history"], "wait"),
        (&on, &["str", "nowhere", "in"], "split"),
        (&on, &["str", "nowhere", "in"], "respawn"),
        (&on, &["str", "x", "name"], "respawn"),
        (&on, &["100", "30", "size"], "respawn"),
        (&on, &["str", "/", "cwd"], "respawn"),
        (&on, &["str", "%99", "id"], "respawn"),
        (&on, &["vertical"], "respawn"),
        (&opts, &["str", "nowhere", "in"], "new-workspace"),
    ] {
        let refused = server.fux(&[before, unused, &[word]].concat())?;
        assert_eq!(refused.status, 1, "{unused:?} {word}: {}", refused.stdout);
        assert!(
            refused.stderr.starts_with("fux: unused: "),
            "{unused:?} {word}: {}",
            refused.stderr
        );
    }
    for unused in [
        &["horizontal"][..],
        &["vertical"],
        &["kill-first"],
        &["full"],
        &["alt-screen"],
        &["last-output"],
        &["joined"],
        &["0", "5", "rows"],
        &["json"],
    ] {
        let refused = server.fux(&[&opts[..], unused, &cat, &["run"]].concat())?;
        assert_eq!(refused.status, 1, "{unused:?} run: {}", refused.stdout);
        assert!(
            refused.stderr.starts_with("fux: unused: "),
            "{unused:?} run: {}",
            refused.stderr
        );
    }
    // Nothing was made: the two panes, and no workspace "nowhere".
    let ls = server.ok(&["ls"])?;
    assert!(!ls.contains("nowhere"), "{ls}");
    assert_eq!(ls.matches('%').count(), 2, "{ls}");
    // A capture's refusal of a wait says what it does: settles, then reads.
    let refused = server.fux(&[&on[..], &["str", "x", "wait-text", "capture"]].concat())?;
    assert!(refused.stderr.contains("settles"), "{}", refused.stderr);
    Ok(())
}

/// A `run` whose wait fails still prints the pane it started, then says
/// why it failed.
#[test]
fn a_failed_runs_pane_is_printed() -> Outcome {
    let server = Server::start("")?;
    let out = server.fux(&run(
        &["str", "NEVER", "wait-text"],
        &["sh", "-c", "exit 2"],
    ))?;
    assert_eq!(out.status, 3, "{}", out.stderr);
    let pane = id(&out.stdout);
    assert!(
        pane.starts_with('%') && out.stdout == format!("{pane}\n"),
        "{}",
        out.stdout
    );
    assert!(out.stderr.starts_with("fux: exited: "), "{}", out.stderr);
    Ok(())
}

/// A wait whose pane closes under it (its shell exited) fails so, with
/// status 4 and the code `closed`.
#[test]
fn a_wait_whose_pane_closes_says_so() -> Outcome {
    let server = Server::start("")?;
    let typed = ["opts", "str", "sleep 2; exit", "typed", "str", "w", "in"];
    let pane = id(&server.ok(&[&typed[..], &["new-tab"]].concat())?);
    let p = ["str", pane.as_str(), "pane"];
    let out = server.fux(&[&p[..], &["opts", "str", "NEVER", "wait-text", "wait"]].concat())?;
    assert_eq!(out.status, 4, "{}{}", out.stdout, out.stderr);
    assert!(out.stderr.starts_with("fux: closed: "), "{}", out.stderr);
    Ok(())
}

/// A later text wait does not take a line a full-screen program redrew the
/// same since the input for new: it answers the line that came after.
#[test]
fn a_later_wait_skips_a_line_redrawn_the_same() -> Outcome {
    let server = Server::start("")?;
    let editor = "import sys, time, tty\ntty.setraw(0)\nw = sys.stdout.write\n\
                  w('\\x1b[?1049h')\n\
                  draw = lambda: (w('\\x1b[H\\x1b[2JF5 preview\\r\\nbody'), sys.stdout.flush())\n\
                  draw(); sys.stdin.read(1); draw(); time.sleep(0.5)\n\
                  w('\\r\\nPREVIEW VALID'); sys.stdout.flush(); sys.stdin.read(1)\n";
    let pane = id(&server.ok(&run(
        &["str", "body", "wait-text"],
        &["python3", "-c", editor],
    ))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "x",
        "text",
        "0",
        "timeout",
        "send-keys",
    ];
    server.ok(&keys)?;
    let wait = [
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "PREVIEW",
        "wait-text",
        "5",
        "timeout",
        "wait",
    ];
    assert_eq!(server.ok(&wait)?, "PREVIEW VALID\n");
    Ok(())
}

/// The prompt on the row the keys went in on was there before them: a
/// program that echoes nothing and writes on that row after does not make
/// the prompt new, for the keys' own wait or a later one; the text written
/// after it on that row is new.
#[test]
fn the_prompt_on_the_typed_row_is_never_new() -> Outcome {
    let server = Server::start("")?;
    let quiet = "import sys, tty\ntty.setraw(0)\nw = sys.stdout.write\n\
                 w('READY prompt> '); sys.stdout.flush(); sys.stdin.read(1)\n\
                 w(' other output'); sys.stdout.flush(); sys.stdin.read(1)\n\
                 w(' READY suffix'); sys.stdout.flush(); sys.stdin.read(1)\n";
    let pane = id(&server.ok(&run(&[], &["python3", "-c", quiet]))?);
    let wait = |words: &[&str]| -> Result<Output, String> {
        server.fux(&[&["str", &pane, "pane"][..], words].concat())
    };
    let keys = wait(&[
        "keys",
        "str",
        "x",
        "text",
        "str",
        "READY",
        "wait-text",
        "2",
        "timeout",
        "send-keys",
    ])?;
    assert_eq!(keys.status, CANNOT_COME, "{}{}", keys.stdout, keys.stderr);
    let later = wait(&["opts", "str", "READY", "wait-text", "1", "timeout", "wait"])?;
    assert_eq!(
        later.status, CANNOT_COME,
        "{}{}",
        later.stdout, later.stderr
    );
    let after = wait(&[
        "keys",
        "str",
        "y",
        "text",
        "str",
        "READY",
        "wait-text",
        "2",
        "timeout",
        "send-keys",
    ])?;
    assert_eq!(after.status, 0, "{}{}", after.stdout, after.stderr);
    assert!(after.stdout.ends_with("READY suffix\n"), "{}", after.stdout);
    Ok(())
}

/// A prompt wider than the pane wraps from the rows above the keys': it is
/// still no new text, for the keys' wait, with Enter or not, or a later
/// one.
#[test]
fn a_wrapped_prompt_is_never_new() -> Outcome {
    let server = Server::start("")?;
    let prompt = "print('PROMPT READY READY>', end='', flush=True)\n\
                  while True:\n input(); print('RESULT OK', flush=True)\n";
    let pane = id(&server.ok(&run(&["12", "8", "size"], &["python3", "-c", prompt]))?);
    let keys = |input: &[&str]| -> Result<Output, String> {
        let wait = ["str", "READY", "wait-text", "2", "timeout", "send-keys"];
        server.fux(&[&["str", &pane, "pane", "keys"][..], input, &wait].concat())
    };
    let typed = keys(&["str", "abc", "text"])?;
    assert_eq!(typed.status, CANNOT_COME, "{}", typed.stdout);
    let enter = keys(&["str", "Enter", "key"])?;
    assert_eq!(enter.status, CANNOT_COME, "{}", enter.stdout);
    // Status 7 answers with the screen; a time limit (not Linux) answers on
    // stderr alone.
    if PROBED {
        assert!(enter.stdout.contains("RESULT OK"), "{}", enter.stdout);
    }
    let later = server.fux(&[
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "READY",
        "wait-text",
        "1",
        "timeout",
        "wait",
    ])?;
    assert_eq!(later.status, CANNOT_COME, "{}", later.stdout);
    Ok(())
}

/// After a line taller than the screen, an answer starts at the line typed:
/// the screen's top row, which goes on that line, did not change.
#[test]
fn an_answer_after_a_line_taller_than_the_screen_is_what_came() -> Outcome {
    let server = Server::start("")?;
    let shell = id(&server.ok(&run(
        &["40", "10", "size", "str", "PS1", "str", "QA> ", "env"],
        &["bash", "--norc", "-i"],
    ))?);
    let a: String = std::iter::repeat_n('a', 1000).collect();
    let long = format!("v='{a}'");
    line(&server, &shell, &long)?;
    let next = line(&server, &shell, "printf 'NEXT\\n'")?;
    assert!(
        body(&next).starts_with("QA> printf 'NEXT\\n'\nNEXT\n"),
        "{next}"
    );
    Ok(())
}

/// A scroll that cuts a wrapped line, pushing its first row into history,
/// moves no line under it: an answer starts at the line typed.
#[test]
fn a_wrapped_line_cut_by_a_scroll_moves_no_line() -> Outcome {
    let server = Server::start("")?;
    let shell = id(&server.ok(&run(
        &["20", "8", "size", "str", "PS1", "str", "QA> ", "env"],
        &["bash", "--norc", "-i"],
    ))?);
    line(
        &server,
        &shell,
        "a(){ printf '%s\\n' 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ M1 M2 M3 M4; }; \
         b(){ printf 'FR''ESH\\n'; }; clear",
    )?;
    // The prompt ends on the last row, the wrapped line from the second.
    line(&server, &shell, "a")?;
    let next = line(&server, &shell, "b")?;
    assert!(body(&next).starts_with("QA> b\nFRESH\n"), "{next}");
    Ok(())
}

/// A program that takes the alternate screen lets no line go: a text wait
/// on it does not say lines were let go.
#[test]
fn the_alternate_screen_lets_no_line_go() -> Outcome {
    let server = Server::start("")?;
    let full = "import sys, tty\ntty.setraw(0)\n\
                sys.stdout.write('\\x1b[?1049hMENU'); sys.stdout.flush(); sys.stdin.read(1)\n";
    let out = server.fux(&run(
        &[
            "40",
            "10",
            "size",
            "str",
            "READY",
            "wait-text",
            "2",
            "timeout",
        ],
        &["python3", "-c", full],
    ))?;
    assert_eq!(out.status, CANNOT_COME, "{}{}", out.stdout, out.stderr);
    let said = format!("{}{}", out.stdout, out.stderr);
    assert!(!said.contains("not kept"), "{said}");
    assert!(!said.contains("let go"), "{said}");
    Ok(())
}

/// What was typed is taken as a terminal shows it after its line editing:
/// a character, a word or the line taken back, a Tab a blank, a control
/// character as `^A`. Its echo holds no text that only the keys had.
#[test]
fn line_editing_is_in_the_echo() -> Outcome {
    let server = Server::start("")?;
    let lines = "import sys\nprint('Prompt> ', end='', flush=True)\n\
                 for line in sys.stdin: pass\n";
    let pane = id(&server.ok(&run(&[], &["python3", "-c", lines]))?);
    let typed = |input: &[&str]| -> Result<i32, String> {
        let words = [
            &["str", &pane, "pane", "keys"][..],
            input,
            &[
                "str",
                "Enter",
                "key",
                "str",
                "TOKEN",
                "wait-text",
                "1",
                "timeout",
                "send-keys",
            ],
        ]
        .concat();
        Ok(server.fux(&words)?.status)
    };
    let texts = [
        "TOKEX\x7fN",
        "hello\tTOKEN",
        "hello\x15TOKEN",
        "a b\x17TOKEN",
        "hello\x01 TOKEN",
    ];
    for text in texts {
        assert_eq!(typed(&["str", text, "text"])?, CANNOT_COME, "{text:?}");
    }
    for key in ["Tab", "C-u", "C-w", "BSpace"] {
        let input = [
            "str", "hello", "text", "str", key, "key", "str", "TOKEN", "text",
        ];
        assert_eq!(typed(&input)?, CANNOT_COME, "{key}");
    }
    Ok(())
}

/// A line deleted from a screen redrawn in place (CSI M) changes the rows
/// below it, though they keep their text: what came is said from there.
#[test]
fn a_deleted_line_is_answered() -> Outcome {
    let server = Server::start("")?;
    let delete = "import sys, tty\ntty.setraw(0)\nw = sys.stdout.write\n\
                  w('\\x1b[H\\x1b[2JTITLE\\r\\nROW1\\r\\nROW2\\r\\nROW3'); sys.stdout.flush()\n\
                  sys.stdin.read(1); w('\\x1b[2;1H\\x1b[M'); sys.stdout.flush()\n\
                  sys.stdin.read(1)\n";
    // Drawn first: where nothing says it reads (macOS), a slow start is
    // otherwise taken as settled before the screen is.
    let size = ["40", "10", "size", "str", "ROW3", "wait-text"];
    let pane = id(&server.ok(&run(&size, &["python3", "-c", delete]))?);
    let answer = server.ok(&["str", &pane, "pane", "str", "x", "type"])?;
    assert_eq!(body(&answer), "ROW2\nROW3\n", "{answer}");
    Ok(())
}

/// A text a program `run` just started never showed has not gone, and a
/// text shown stays: on a program reading its terminal neither can change
/// before you type, so `wait-gone` ends at once (status 7) with the screen;
/// where fux is not told the program reads (not Linux), at the limit, which
/// says what the pane does.
#[test]
fn a_new_programs_text_goes_once_shown() -> Outcome {
    let server = Server::start("")?;
    let out = server.fux(&run(
        &["str", "ABSENT", "wait-gone", "2", "timeout"],
        &["sh", "-c", "echo ready; read x"],
    ))?;
    assert_eq!(out.status, CANNOT_COME, "{}{}", out.stdout, out.stderr);
    let pane = id(&out.stdout);
    if PROBED {
        assert_eq!(out.stdout, format!("{pane} {READING}\nready\n"));
    } else {
        assert!(
            out.stderr.contains(&format!("; {pane} {READING}, ")),
            "{}",
            out.stderr
        );
    }
    let shown = server.fux(&[
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "ready",
        "wait-gone",
        "1",
        "timeout",
        "wait",
    ])?;
    assert_eq!(
        shown.status, CANNOT_COME,
        "{}{}",
        shown.stdout, shown.stderr
    );
    Ok(())
}

/// Enter alone types nothing: a program that echoes nothing and answers on
/// the prompt's row (`Password: ACCEPTED`) wrote output there, for the
/// keys' own wait and a later one.
#[test]
fn enter_alone_leaves_the_prompts_row_to_the_program() -> Outcome {
    let server = Server::start("")?;
    let quiet = "import sys, termios
a = termios.tcgetattr(0)
a[3] &= ~termios.ECHO
\
                 termios.tcsetattr(0, termios.TCSANOW, a)
\
                 print('Password: ', end='', flush=True); sys.stdin.readline()
\
                 print('ACCEPTED', flush=True); sys.stdin.readline()
";
    let pane = id(&server.ok(&run(
        &["str", "Password", "wait-text"],
        &["python3", "-c", quiet],
    ))?);
    let keys = |input: &[&str]| -> Result<Output, String> {
        server.fux(&[&["str", &pane, "pane", "keys"][..], input, &["send-keys"]].concat())
    };
    keys(&["str", "TOKEN", "text"])?;
    let enter = keys(&[
        "str",
        "Enter",
        "key",
        "str",
        "ACCEPTED",
        "wait-text",
        "2",
        "timeout",
    ])?;
    assert_eq!(enter.status, 0, "{}{}", enter.stdout, enter.stderr);
    assert_eq!(enter.stdout, "Password: ACCEPTED\n");
    let later = server.fux(&[
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "ACCEPTED",
        "wait-text",
        "1",
        "timeout",
        "wait",
    ])?;
    assert_eq!(later.status, 0, "{}{}", later.stdout, later.stderr);
    Ok(())
}

/// A home whose `.bashrc` holds `rc`, for a bash started with it.
fn bash_home(server: &Server, name: &str, rc: &str) -> Result<String, String> {
    let home = server.dir.join(name);
    std::fs::create_dir_all(&home).map_err(e)?;
    std::fs::write(home.join(".bashrc"), rc).map_err(e)?;
    Ok(home.to_string_lossy().into_owned())
}

/// With `set -T`, functions inherit the user's DEBUG trap: fux's setup
/// still marks the shell, and the trap's output shows none of it but the
/// call, nor anything of fux's at a prompt.
#[test]
fn bash_marks_hold_under_a_traced_debug_trap() -> Outcome {
    // Before bash 4.4 (macOS's 3.2) fux's marks need a DEBUG trap of their
    // own.
    if old_bash()? {
        return Ok(());
    }
    let server = Server::start("")?;
    let rc = "set -T\ntrap 'printf \"DBG %s\\n\" \"$BASH_COMMAND\"' DEBUG\n";
    let home = bash_home(&server, "home", rc)?;
    server.ok(&run(
        &["str", "t", "name", "str", "HOME", "str", &home, "env"],
        &["bash"],
    ))?;
    assert_eq!(done(&line(&server, "t", "false")?), Some("done 1"));
    line(&server, "t", "echo hi")?;
    assert_eq!(last_output(&server, "t")?, "DBG echo hi\nhi\n");
    let screen = server.ok(&["str", "t", "pane", "opts", "capture"])?;
    let fuxs: Vec<&str> = (screen.lines())
        .filter(|l| l.starts_with("DBG") && l.contains("fux"))
        .collect();
    assert!(fuxs.iter().all(|l| *l == "DBG __fux_marks"), "{screen}");
    Ok(())
}

/// Under `set -x` fux's hook at each prompt traces nothing: a command's
/// output holds its own trace alone, and no line of fux's shows.
#[test]
fn set_x_traces_nothing_of_the_marks() -> Outcome {
    // Before bash 4.4 the marks' own DEBUG trap is traced.
    if old_bash()? {
        return Ok(());
    }
    let server = Server::start("")?;
    server.ok(&run(&["str", "x", "name"], &["bash", "--norc", "-i"]))?;
    line(&server, "x", "set -x; false | true")?;
    line(&server, "x", "echo hi")?;
    assert_eq!(last_output(&server, "x")?, "+ echo hi\nhi\n");
    let screen = server.ok(&["str", "x", "pane", "opts", "capture"])?;
    assert!(!screen.contains("++"), "{screen}");
    Ok(())
}

/// A `PROMPT_COMMAND` the user's startup set (an array, as some system
/// files make it) is replaced by one set in the running shell, as without
/// fux.
#[test]
fn a_prompt_command_set_anew_replaces_the_startup_one() -> Outcome {
    let server = Server::start("")?;
    let home = bash_home(&server, "home", "PROMPT_COMMAND=('echo STARTUP-HOOK')\n")?;
    server.ok(&run(
        &["str", "p", "name", "str", "HOME", "str", &home, "env"],
        &["bash"],
    ))?;
    assert!(line(&server, "p", "true")?.contains("STARTUP-HOOK"));
    line(&server, "p", "PROMPT_COMMAND='echo NEW-HOOK'")?;
    let after = line(&server, "p", "echo x")?;
    assert!(
        after.contains("NEW-HOOK") && !after.contains("STARTUP-HOOK"),
        "{after}"
    );
    Ok(())
}

/// A line bash cannot parse never runs, yet its complaint is its output;
/// and a command's output is its rows whole, what it wrote right of where
/// it left the cursor too.
#[test]
fn last_output_holds_a_parse_error_and_whole_rows() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(&["str", "b", "name"], &["bash", "--norc", "-i"]))?;
    line(&server, "b", "printf 'ABCDE\\rXY'")?;
    assert_eq!(last_output(&server, "b")?, "XYCDE\n");
    let status = line(&server, "b", "if ; then")?;
    assert!(
        [Some("done 2"), Some("done 258")].contains(&done(&status)),
        "{status}"
    );
    let error = last_output(&server, "b")?;
    assert!(
        error.contains("syntax error") && !error.contains("if ; then"),
        "{error}"
    );
    Ok(())
}

/// The last output of a full-screen command, read with `main-screen` too,
/// is its screen: the cursor is said where it is on it.
#[test]
fn a_full_screen_last_output_keeps_its_cursor() -> Outcome {
    let server = Server::start("")?;
    let shell = id(&server.ok(&run(&["20", "6", "size"], &["bash", "--norc", "-i"]))?);
    let draw = "printf 'before\\n\\033[?1049h\\033[Habc\\ndef\\nxy%s\\033[2;2H' z; read x";
    server.ok(&typed(&shell, draw, &["str", "xyz", "wait-text"]))?;
    let json = server.ok(&[
        "str",
        &shell,
        "pane",
        "opts",
        "main-screen",
        "last-output",
        "json",
        "capture",
    ])?;
    assert!(json.contains("\"cursor\":[1,1]"), "{json}");
    Ok(())
}

/// While a command runs, its output so far is the last output, not the
/// command's before.
#[test]
fn last_output_of_a_running_command_is_its_output_so_far() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(&["str", "r", "name"], &["bash", "--norc", "-i"]))?;
    line(&server, "r", "echo BEFORE")?;
    let asks = "echo CURRENT; read -r answer";
    server.ok(&typed("r", asks, &["str", "^CURRENT", "wait-text"]))?;
    assert_eq!(last_output(&server, "r")?, "CURRENT\n");
    Ok(())
}

/// A pane renamed keeps its name when its program is started again.
#[test]
fn respawn_keeps_a_rename() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&[], &["sh", "-c", "exit 0"]))?);
    server.ok(&["str", &pane, "pane", "str", "keepme", "rename"])?;
    server.ok(&["str", &pane, "pane", "opts", "respawn"])?;
    let name = server.ok(&["str", &pane, "pane", "str", "name", "get"])?;
    assert_eq!(name, "keepme\n");
    Ok(())
}

/// A `PROMPT_COMMAND` hook from the user's startup sees the command's
/// `PIPESTATUS` and `$?`, as without fux.
#[test]
fn a_startup_hook_sees_the_commands_pipestatus() -> Outcome {
    let server = Server::start("")?;
    let rc = "PROMPT_COMMAND='echo \"P=${PIPESTATUS[*]} S=$?\"'\n";
    let home = bash_home(&server, "home", rc)?;
    server.ok(&run(
        &["str", "h", "name", "str", "HOME", "str", &home, "env"],
        &["bash"],
    ))?;
    let piped = line(&server, "h", "false | (exit 7)")?;
    assert!(piped.contains("P=1 7 S=7"), "{piped}");
    let passed = line(&server, "h", "false | true")?;
    assert!(passed.contains("P=1 0 S=0"), "{passed}");
    Ok(())
}

/// Keys typed before a shell draws its prompt are echoed twice, by the
/// terminal and then by the shell after its prompt: neither is output.
#[test]
fn keys_typed_before_the_prompt_are_echo_twice() -> Outcome {
    let server = Server::start("")?;
    let home = bash_home(&server, "slow", "sleep 0.5\n")?;
    let before = ["str", "HOME", "str", &home, "env", "0", "timeout"];
    let shell = id(&server.ok(&run(&before, &["bash"]))?);
    let wait = ["str", "TYPED_AHEAD", "wait-text", "3", "timeout"];
    let ahead = server.fux(&typed(&shell, ": TYPED_AHEAD", &wait))?;
    assert_eq!(
        ahead.status, CANNOT_COME,
        "{}{}",
        ahead.stdout, ahead.stderr
    );
    Ok(())
}
/// A program that echoes nothing and replies with the word typed, in
/// capitals, wrote it: the echo keeps its case.
#[test]
fn a_reply_that_starts_with_the_word_typed_is_output() -> Outcome {
    let server = Server::start("")?;
    let reply = "import os, tty, time\ntty.setraw(0); os.write(1, b'command: snapshot\\r\\n')\n\
                 os.read(0, 9); time.sleep(0.3); os.write(1, b'SNAPSHOT ticket=S-1\\r\\n')\n\
                 os.read(0, 1)\n";
    let pane = id(&server.ok(&run(
        &["str", "command:", "wait-text"],
        &["python3", "-c", reply],
    ))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "snapshot",
        "text",
        "str",
        "Enter",
        "key",
        "str",
        "SNAPSHOT ticket=",
        "wait-text",
        "send-keys",
    ];
    let answer = server.ok(&keys)?;
    assert!(answer.contains("SNAPSHOT ticket=S-1"), "{answer}");
    Ok(())
}

/// A line typed in one send-keys and edited in the next (BSpace, then
/// Enter) is the echo of the line as edited, not output.
#[test]
fn a_line_edited_in_a_later_send_is_echo() -> Outcome {
    let server = Server::start("")?;
    let read = "import sys\nsys.stdout.write('Input> '); sys.stdout.flush()\n\
                sys.stdin.readline(); sys.stdin.read()\n";
    let pane = id(&server.ok(&run(
        &["str", "Input>", "wait-text"],
        &["python3", "-c", read],
    ))?);
    let keys = ["str", &pane, "pane", "keys", "str", "NEEDLEX", "text"];
    server.ok(&[&keys[..], &["send-keys"]].concat())?;
    let edit = server.fux(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "BSpace",
        "key",
        "str",
        "Enter",
        "key",
        "str",
        "NEEDLE",
        "wait-text",
        "3",
        "timeout",
        "send-keys",
    ])?;
    assert_eq!(edit.status, CANNOT_COME, "{}{}", edit.stdout, edit.stderr);
    Ok(())
}

/// The keys' row cleared and drawn again with another prompt still shows
/// their echo, not output.
#[test]
fn a_typed_row_drawn_with_another_prompt_is_echo() -> Outcome {
    let server = Server::start("")?;
    let redraw = "import os, tty\ntty.setraw(0); os.write(1, b'Input> ')\n\
                  s = os.read(0, 6); os.write(1, s)\n\
                  os.write(1, b'\\r\\x1b[2KChanged> ' + s); os.read(0, 1)\n";
    let pane = id(&server.ok(&run(
        &["str", "Input>", "wait-text"],
        &["python3", "-c", redraw],
    ))?);
    let keys = server.fux(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "SECRET",
        "text",
        "str",
        "SECRET",
        "wait-text",
        "2",
        "timeout",
        "send-keys",
    ])?;
    assert_eq!(keys.status, CANNOT_COME, "{}{}", keys.stdout, keys.stderr);
    Ok(())
}

/// Keys typed before a shell draws its first prompt are echoed twice, by
/// the terminal and then by the shell after its prompt: neither is output.
/// A shell fux marks says so, Enter or not. One it did not set up (started
/// by another program, over ssh) says nothing: its line drawn again reads
/// as output that repeats the keys would, and only while it still edits
/// the line (no Enter) is it known for the echo, by the terminal taking
/// keys raw, where fux is told what the shell is blocked in (Linux, where
/// fux marks `bash --norc` too).
#[test]
fn keys_typed_before_a_prompt_are_echo_twice() -> Outcome {
    let server = Server::start("")?;
    let unmarked = "import os\n\
              env = dict(os.environ, PS1='PROMPT> ', PROMPT_COMMAND='sleep 0.3')\n\
              os.execvpe('bash', ['bash', '--noprofile', '--norc', '-i'], env)\n";
    let marked = ["bash", "--norc", "-i"];
    let vars = [
        "str",
        "PS1",
        "str",
        "PROMPT> ",
        "env",
        "str",
        "PROMPT_COMMAND",
    ];
    let vars = [&vars[..], &["str", "sleep 0.3", "env", "0", "timeout"]].concat();
    let wait = ["str", "TYPED_AHEAD", "wait-text", "3", "timeout"];
    let cases: [(&[&str], &[&str], bool); 3] = [
        (&["0", "timeout"], &["python3", "-c", unmarked], false),
        (&vars, &marked, false),
        (&vars, &marked, true),
    ];
    // Without marks, keys typed ahead are told by what the shell is blocked
    // in, which only Linux says; and fux marks `bash --norc` only there.
    for (options, program, enter) in cases.into_iter().filter(|_| PROBED) {
        let shell = id(&server.ok(&run(options, program))?);
        let line = [
            "str",
            &shell,
            "pane",
            "keys",
            "str",
            ": TYPED_AHEAD",
            "text",
        ];
        let end: &[&str] = if enter { &["str", "Enter", "key"] } else { &[] };
        let ahead = server.fux(&[&line[..], end, &wait, &["send-keys"]].concat())?;
        assert_eq!(
            ahead.status, CANNOT_COME,
            "{program:?} Enter {enter}: {}{}",
            ahead.stdout, ahead.stderr
        );
    }
    Ok(())
}

/// A line typed that is taller than the screen, wrapped from history onto
/// it, is read whole: its echo is no output.
#[test]
fn a_typed_line_taller_than_the_screen_is_echo() -> Outcome {
    let server = Server::start("")?;
    let read = "import sys\nsys.stdout.write('Prompt> '); sys.stdout.flush(); sys.stdin.readline()\n\
                sys.stdout.write('Next> '); sys.stdout.flush(); sys.stdin.readline()\n";
    let pane = id(&server.ok(&run(
        &["16", "12", "size", "str", "Prompt>", "wait-text"],
        &["python3", "-c", read],
    ))?);
    let long: String = std::iter::repeat_n("padding ", 45)
        .chain(["TOKEN"])
        .collect();
    let wait = ["str", "TOKEN", "wait-text", "3", "timeout"];
    let keys = server.fux(&typed(&pane, &long, &wait))?;
    assert_eq!(keys.status, CANNOT_COME, "{}{}", keys.stdout, keys.stderr);
    Ok(())
}

/// A line edited with the cursor keys (here Left, then BSpace) is the echo
/// of the line as edited, not output.
#[test]
fn a_line_edited_with_cursor_keys_is_echo() -> Outcome {
    let server = Server::start("")?;
    let read = "read -e -p 'Prompt> ' v; echo \"VALUE=$v\"; read x";
    let pane = id(&server.ok(&run(
        &["str", "Prompt>", "wait-text"],
        &["bash", "--norc", "-c", read],
    ))?);
    let keys = server.fux(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "NEEzzzDLE",
        "text",
        "str",
        "Left",
        "key",
        "3",
        "times",
        "str",
        "BSpace",
        "key",
        "3",
        "times",
        "str",
        "NEEDLE",
        "wait-text",
        "3",
        "timeout",
        "send-keys",
    ])?;
    assert_eq!(keys.status, CANNOT_COME, "{}{}", keys.stdout, keys.stderr);
    Ok(())
}

/// Keys typed while a shell command runs are echoed on a prompt's row only:
/// a row the command writes that ends with them is its output.
#[test]
fn output_that_ends_with_keys_typed_ahead_is_output() -> Outcome {
    let server = Server::start("")?;
    let shell = id(&server.ok(&run(&[], &["bash", "--norc", "-i"]))?);
    // Long enough that the keys go in while it runs (`docs/agents.md`: keys
    // typed as a command ends may have their echo come after its output).
    let script = "printf 'BEG''IN\\n'; sleep 2; printf '\\nYou said: NEE''DLE\\n'";
    server.ok(&typed(&shell, script, &["str", "^BEGIN", "wait-text"]))?;
    let keys = [
        "str",
        &shell,
        "pane",
        "keys",
        "str",
        "NEEDLE",
        "text",
        "str",
        "NEEDLE",
        "wait-text",
        "5",
        "timeout",
        "send-keys",
    ];
    let said = server.fux(&keys)?;
    assert_eq!(said.status, 0, "{}{}", said.stdout, said.stderr);
    assert!(said.stdout.contains("You said: NEEDLE"), "{}", said.stdout);
    Ok(())
}

/// A program that reads lines and asked for bracketed paste has the
/// paste's marks echoed by the terminal (`^[[200~`): they are its echo.
#[test]
fn a_paste_echoed_with_its_marks_is_echo() -> Outcome {
    let server = Server::start("")?;
    let read = "import os, sys\nos.write(1, b'\\x1b[?2004hPrompt> ')\n\
                sys.stdin.readline(); sys.stdin.read()\n";
    let pane = id(&server.ok(&run(
        &["str", "Prompt>", "wait-text"],
        &["python3", "-c", read],
    ))?);
    let keys = server.fux(&[
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "NEEDLE",
        "paste",
        "str",
        "NEEDLE",
        "wait-text",
        "3",
        "timeout",
        "send-keys",
    ])?;
    assert_eq!(keys.status, CANNOT_COME, "{}{}", keys.stdout, keys.stderr);
    Ok(())
}

/// Of several conditions met before a wait began, the first to come
/// answers, in whatever order they were given: a text printed before the
/// program exited, or before a bell; a bell rung before the text.
#[test]
fn the_condition_met_first_answers() -> Outcome {
    let server = Server::start("")?;
    let exits = id(&server
        .fux(&run(
            &["wait-exit"],
            &["sh", "-c", "echo READY; sleep 0.2; exit 5"],
        ))?
        .stdout);
    let wait = |pane: &str, conditions: &[&str]| -> Result<Output, String> {
        let words = [&["str", pane, "pane", "opts"][..], conditions, &["wait"]].concat();
        server.fux(&words)
    };
    let text = wait(&exits, &["wait-exit", "str", "READY", "wait-text"])?;
    assert_eq!((text.status, text.stdout.as_str()), (0, "READY\n"));
    let script = "echo READY; sleep 0.2; printf '\\a'; sleep 0.2; echo DONE; read x";
    let rings = id(&server.ok(&run(&["str", "DONE", "wait-text"], &["sh", "-c", script]))?);
    let text = wait(&rings, &["wait-bell", "str", "READY", "wait-text"])?;
    assert_eq!((text.status, text.stdout.as_str()), (0, "READY\n"));
    let bell = wait(&rings, &["str", "DONE", "wait-text", "wait-bell"])?;
    assert_eq!((bell.status, bell.stdout.as_str()), (0, "bell\n"));
    Ok(())
}

/// A wait that begins after a text and a bell on one line takes the text
/// as before the bell if it was there as the bell rang, the line written
/// on after; and of two texts on one line, the one further left, printed
/// first, whatever their order given.
#[test]
fn a_late_wait_orders_what_shares_a_line_by_where_it_is() -> Outcome {
    let server = Server::start("")?;
    let script = "printf FIRST; sleep 0.2; printf '\\a'; sleep 0.2; echo ' SECOND'; read x";
    let pane = id(&server.ok(&run(&["str", "SECOND", "wait-text"], &["sh", "-c", script]))?);
    let wait = |conditions: &[&str]| -> Result<String, String> {
        let words = [
            &["str", &pane, "pane", "opts"][..],
            conditions,
            &["wait", "json"],
        ]
        .concat();
        server.ok(&words)
    };
    let text = wait(&["wait-bell", "str", "FIRST", "wait-text"])?;
    assert!(text.contains(r#""met":"text""#), "{text}");
    let first = wait(&["str", "SECOND", "wait-text", "str", "FIRST", "wait-text"])?;
    assert!(first.contains(r#""text":"FIRST""#), "{first}");
    Ok(())
}

/// A bell and a text in one write: the text came after the bell, so a
/// wait for either answers the bell.
#[test]
fn a_bell_before_a_text_in_one_write_answers_first() -> Outcome {
    let server = Server::start("")?;
    let code = "import os, sys\nos.write(1, b'\\aSECOND-TEXT\\n'); sys.stdin.read()\n";
    let rang = server.ok(&[
        &run(
            &["str", "SECOND-TEXT", "wait-text", "wait-bell"],
            &["python3", "-c", code],
        )[..],
        &["json"],
    ]
    .concat())?;
    assert!(rang.contains(r#""met":"bell""#), "{rang}");
    Ok(())
}

/// QA wave 11, issue 1: keys typed while an earlier command still runs
/// (typed ahead) are the terminal's echo, then the shell's as it reads
/// them: a wait for a text they hold answers what the command prints, once
/// it runs, never the typed line.
#[test]
fn keys_typed_while_a_command_runs_are_echo() -> Outcome {
    let server = Server::start("")?;
    let shell = id(&server.ok(&run(&[], &["bash", "--norc", "-i"]))?);
    server.ok(&typed(&shell, "sleep 2", &["0", "timeout"]))?;
    let started = std::time::Instant::now();
    let done = server.ok(&typed(&shell, "echo DONE", &["str", "DONE", "wait-text"]))?;
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(1500),
        "{done}"
    );
    assert_eq!(done, "DONE\n");
    Ok(())
}

/// QA wave 11, issue 3: a command over several lines that bash cannot
/// parse: its last output is the complaint alone, not the continuation
/// prompts and the lines typed at them.
#[test]
fn a_command_bash_cannot_parse_over_lines_outputs_its_complaint() -> Outcome {
    let server = Server::start("")?;
    let shell = id(&server.ok(&run(&[], &["bash", "--norc", "-i"]))?);
    server.ok(&typed(&shell, "for i in 1 2", &["0", "timeout"]))?;
    server.ok(&typed(&shell, "do echo $i", &["0", "timeout"]))?;
    let status = line(&server, &shell, "done)")?;
    assert!(done(&status).is_some(), "{status}");
    let error = last_output(&server, &shell)?;
    assert!(
        error.contains("syntax error") && !error.contains("> ") && !error.contains("echo"),
        "{error}"
    );
    Ok(())
}

/// Sends `input` to `pane` with a wait for NEEDLE: its status.
fn needle(server: &Server, pane: &str, input: &[&str]) -> Result<i32, String> {
    let words = [
        &["str", pane, "pane", "keys"][..],
        input,
        &["str", "NEEDLE", "wait-text", "2", "timeout", "send-keys"],
    ]
    .concat();
    Ok(server.fux(&words)?.status)
}

/// QA wave 11, issue 4: text typed and then erased (C-u, then Enter, to a
/// program reading a line) is the echo, never output: a wait for it in
/// the same send-keys cannot be met.
#[test]
fn text_typed_then_erased_is_echo() -> Outcome {
    let server = Server::start("")?;
    let read = "x = input('Input> '); print('GOT', len(x)); input()";
    let python = id(&server.ok(&run(
        &["str", "Input>", "wait-text"],
        &["python3", "-c", read],
    ))?);
    let erased = [
        "str", "NEEDLE", "text", "str", "C-u", "key", "str", "Enter", "key",
    ];
    assert_eq!(needle(&server, &python, &erased)?, CANNOT_COME);
    Ok(())
}

/// QA wave 11, issue 5: a line edited with Left and Delete (readline) is
/// the echo, never output: a wait for the text it comes to cannot be met.
#[test]
fn text_edited_with_delete_is_echo() -> Outcome {
    let server = Server::start("")?;
    let edit = "read -e -p 'Edit> ' x; echo got; read y";
    let bash = id(&server.ok(&run(&["str", "Edit>", "wait-text"], &["bash", "-c", edit]))?);
    let edited = [
        "str", "NEEDXE", "text", "str", "Left", "key", "str", "Left", "key", "str", "DC", "key",
        "str", "L", "text",
    ];
    assert_eq!(needle(&server, &bash, &edited)?, CANNOT_COME);
    Ok(())
}

/// QA wave 12: a text a fresh program shows and takes away in one burst
/// (drawn over) has shown, so `wait-gone` is met; and `^` holds a text to
/// a line's start, not to where a run of output starts after a tab.
#[test]
fn a_text_shown_and_gone_in_a_burst_has_gone_and_anchors_hold_to_lines() -> Outcome {
    let server = Server::start("")?;
    let burst = "import sys; sys.stdout.write('READY\\rxxxxx'); sys.stdout.flush(); input()";
    let gone = server.fux(&run(
        &["str", "READY", "wait-gone", "5", "timeout"],
        &["python3", "-c", burst],
    ))?;
    assert_eq!(gone.status, 0, "{}{}", gone.stdout, gone.stderr);
    let tab = id(&server.ok(&run(
        &["str", "END", "wait-text"],
        &["sh", "-c", "printf 'FIRST\\tSECOND END\\n'; read x"],
    ))?);
    let anchored = [
        "str",
        &tab,
        "pane",
        "opts",
        "str",
        "^SECOND",
        "wait-text",
        "2",
        "timeout",
        "wait",
    ];
    assert_eq!(server.fux(&anchored)?.status, CANNOT_COME);
    Ok(())
}

/// QA wave 12: a line edited (Left, Delete, then the text it lacked) is
/// the echo for a later wait too, not only for the one sent with the keys.
#[test]
fn a_line_edited_is_echo_for_a_later_wait() -> Outcome {
    let server = Server::start("")?;
    let edit = "read -e -p 'Edit> ' x; echo got; read y";
    let bash = id(&server.ok(&run(&["str", "Edit>", "wait-text"], &["bash", "-c", edit]))?);
    let keys = [
        "str",
        &bash,
        "pane",
        "keys",
        "str",
        "NEEDLX",
        "text",
        "str",
        "Left",
        "key",
        "str",
        "DC",
        "key",
        "str",
        "E",
        "text",
        "send-keys",
    ];
    server.ok(&keys)?;
    let later = [
        "str",
        &bash,
        "pane",
        "opts",
        "str",
        "NEEDLE",
        "wait-text",
        "2",
        "timeout",
        "wait",
    ];
    assert_eq!(server.fux(&later)?.status, CANNOT_COME);
    Ok(())
}

/// QA wave 12: a program whose main thread sleeps while another reads its
/// terminal waits (it may go on alone), not busy.
#[test]
fn a_sleeping_main_thread_beside_a_reader_waits() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let program = "import os, time, threading\n\
                   threading.Thread(target=lambda: os.read(0, 100), daemon=True).start()\n\
                   print('READY', flush=True); time.sleep(60)\n";
    let out = server.fux(&run(
        &["str", "READY", "wait-text", "3", "timeout"],
        &["python3", "-c", program],
    ))?;
    let pane = id(&out.stdout);
    let settle = server.fux(&["str", &pane, "pane", "opts", "3", "timeout", "wait"])?;
    assert_eq!(settle.status, 0, "{}{}", settle.stdout, settle.stderr);
    assert!(
        settle.stdout.starts_with(&format!("{pane} waiting")),
        "{}",
        settle.stdout
    );
    Ok(())
}

/// QA wave 13: of several conditions, the first to come answers, a text
/// gone among them; and a later wait places a text written over by
/// another where it came, not where its successor did.
#[test]
fn the_first_condition_to_come_answers_a_text_gone_too() -> Outcome {
    let server = Server::start("")?;
    let swap = "import sys, time\nw = sys.stdout.write\nw('BUSY'); sys.stdout.flush(); time.sleep(0.3)\n\
                w('\\r\\x1b[K'); sys.stdout.flush(); time.sleep(0.3)\n\
                w('LATER\\r\\n'); sys.stdout.flush(); input()\n";
    let busy = id(&server.ok(&run(
        &["str", "LATER", "wait-text"],
        &["python3", "-c", swap],
    ))?);
    let both = [
        "str",
        &busy,
        "pane",
        "opts",
        "str",
        "LATER",
        "wait-text",
        "str",
        "BUSY",
        "wait-gone",
        "wait",
        "json",
    ];
    // BUSY went before LATER came: the text gone answers, with the pane's
    // state, where a text met would say `met`.
    let out = server.fux(&both)?;
    assert!(
        out.status == 0 && out.stdout.contains("\"state\"") && !out.stdout.contains("\"met\""),
        "{}{}",
        out.stdout,
        out.stderr
    );
    let over = "import sys, time\nw = sys.stdout.write\nw('FIRST'); sys.stdout.flush(); time.sleep(0.3)\n\
                w('\\rSECOND\\r\\n'); sys.stdout.flush(); input()\n";
    let pane = id(&server.ok(&run(
        &["str", "SECOND", "wait-text"],
        &["python3", "-c", over],
    ))?);
    let first = [
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "SECOND",
        "wait-text",
        "str",
        "FIRST",
        "wait-text",
        "wait",
    ];
    assert_eq!(
        server.ok(&first)?.trim_end(),
        "FIRST",
        "the text that came first"
    );
    Ok(())
}

/// QA wave 13: a line editor that draws the line again, the same, as
/// Enter takes it brings nothing new: a wait sent with the keys for the
/// text typed cannot be met by it.
#[test]
fn a_line_drawn_again_the_same_on_enter_is_no_output() -> Outcome {
    let server = Server::start("")?;
    let editor = "import os, tty\ntty.setraw(0)\nline = ''\nos.write(1, b'> ')\n\
                  while True:\n    c = os.read(0, 1).decode()\n\
                  \x20   if c in '\\r\\n':\n        os.write(1, ('\\r\\x1b[K> ' + line + '\\r\\n').encode()); line = ''; os.write(1, b'> '); continue\n\
                  \x20   line += c; os.write(1, ('\\r\\x1b[K> ' + line).encode())\n";
    let pane = id(&server.ok(&run(&["str", ">", "wait-text"], &["python3", "-c", editor]))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "NEEDLE",
        "text",
        "str",
        "Enter",
        "key",
        "str",
        "NEEDLE",
        "wait-text",
        "2",
        "timeout",
        "send-keys",
    ];
    assert_eq!(server.fux(&keys)?.status, CANNOT_COME);
    Ok(())
}

/// Replay: a shell command that ended after its own action answered is
/// still said done by a later wait, which counts from the last input.
#[test]
fn a_later_wait_says_a_command_ended_since_the_input() -> Outcome {
    let server = Server::start("")?;
    let shell = id(&server.ok(&run(&[], &["bash", "--norc", "-i"]))?);
    assert_eq!(done(&line(&server, &shell, "true")?), Some("done 0"));
    let keys = [
        "str",
        &shell,
        "pane",
        "keys",
        "str",
        "sleep 0.3; (exit 6)",
        "text",
        "str",
        "Enter",
        "key",
        "0",
        "timeout",
        "send-keys",
    ];
    server.fux(&keys)?;
    std::thread::sleep(std::time::Duration::from_millis(800));
    let later = server.ok(&["str", &shell, "pane", "opts", "wait"])?;
    assert_eq!(done(&later), Some("done 6"), "{later}");
    Ok(())
}

/// Replay: a resize after the input brings nothing new; a later wait's
/// answer starts where the input's reply does, not at the screen's top.
#[test]
fn a_resize_since_the_input_brings_nothing_new() -> Outcome {
    let server = Server::start("")?;
    let echo = "echo OLDLINE; while read l; do echo GOT$l; done";
    let pane = id(&server.ok(&run(&["str", "OLDLINE", "wait-text"], &["sh", "-c", echo]))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "abc",
        "text",
        "str",
        "Enter",
        "key",
        "str",
        "GOTabc",
        "wait-text",
        "send-keys",
    ];
    server.ok(&keys)?;
    server.ok(&[
        "str", &pane, "pane", "opts", "str", "sleep", "program", "str", "30", "arg", "0",
        "timeout", "split",
    ])?;
    let later = server.ok(&["str", &pane, "pane", "opts", "wait"])?;
    assert!(
        !later.contains("OLDLINE") && later.contains("GOTabc"),
        "{later}"
    );
    Ok(())
}

/// Replay: a screen erased and drawn again as it was is no change, and
/// one drawn again with a row changed is answered from that row.
#[test]
fn a_screen_drawn_again_as_it_was_is_no_change() -> Outcome {
    let server = Server::start("")?;
    let menu = "import sys, tty\ntty.setraw(0)\nw = sys.stdout.write\n\
                def draw(v): w('\\x1b[H\\x1b[2JMAIN MENU\\r\\n' + v); sys.stdout.flush()\n\
                draw('old value')\n\
                while True:\n    draw('new value' if sys.stdin.read(1) == 'b' else 'old value')\n";
    let pane = id(&server.ok(&run(
        &["str", "old value", "wait-text"],
        &["python3", "-c", menu],
    ))?);
    let same = server.ok(&["str", &pane, "pane", "str", "a", "type"])?;
    // Said where fux is told the program reads (`no change` is that).
    let said = if PROBED { " · no change" } else { "" };
    assert_eq!(head(&same), format!("{pane} {READING}{said}"), "{same}");
    assert_eq!(body(&same), "", "{same}");
    let changed = server.ok(&["str", &pane, "pane", "str", "b", "type"])?;
    assert_eq!(body(&changed), "new value\n", "{changed}");
    Ok(())
}

/// Replay: a word a line wraps onto the next row starts no word there: a
/// wait for it from a word's or a line's start is not met.
#[test]
fn a_word_wrapped_onto_a_row_starts_no_word_there() -> Outcome {
    let server = Server::start("")?;
    let wrap = "import sys, tty\ntty.setraw(0)\nsys.stdout.write('> '); sys.stdout.flush()\n\
                sys.stdin.read(1); sys.stdout.write('\\r\\nxxxxxxxxxxxxREADY'); sys.stdout.flush()\n\
                sys.stdin.read(1)\n";
    let size = ["12", "5", "size", "str", ">", "wait-text"];
    let pane = id(&server.ok(&run(&size, &["python3", "-c", wrap]))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "k",
        "text",
        "str",
        "READY",
        "wait-text",
        "2",
        "timeout",
        "send-keys",
        "json",
    ];
    let out = server.fux(&keys)?;
    let not_met = if PROBED { CANNOT_COME } else { 124 };
    assert_eq!(out.status, not_met, "{}{}", out.stdout, out.stderr);
    Ok(())
}

/// Replay: a full-screen program's keys go in at full speed, its screen
/// being all output: one read takes every key an action sends.
#[test]
fn a_full_screen_program_takes_an_actions_keys_at_once() -> Outcome {
    let server = Server::start("")?;
    let once = "import os, sys, time, tty\ntty.setraw(0)\nw = sys.stdout.write\n\
                w('\\x1b[?1049hREADY'); sys.stdout.flush(); time.sleep(0.5)\n\
                w('\\r\\nGOT ' + repr(os.read(0, 100))); sys.stdout.flush(); os.read(0, 1)\n";
    let pane = id(&server.ok(&run(
        &["str", "READY", "wait-text"],
        &["python3", "-c", once],
    ))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "abc",
        "text",
        "str",
        "Home",
        "key",
        "str",
        "End",
        "key",
        "str",
        "GOT",
        "wait-text",
        "send-keys",
    ];
    let got = server.ok(&keys)?;
    assert!(got.contains("GOT b'abc\\x1b[H\\x1b[F'"), "{got}");
    Ok(())
}

/// Replay: a wait sent with keys answers once all of them went in, though
/// the reply to the first meets it before the program takes the rest.
#[test]
fn a_keys_wait_answers_once_every_key_went_in() -> Outcome {
    let server = Server::start("")?;
    let raw = "import os, time, tty\ntty.setraw(0)\nos.write(1, b'> ')\n\
               os.read(0, 1); os.write(1, b'\\r\\nRX'); time.sleep(0.6)\n\
               os.read(0, 1); os.write(1, b'\\r\\nSECOND'); os.read(0, 1)\n";
    let pane = id(&server.ok(&run(&["str", ">", "wait-text"], &["python3", "-c", raw]))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "a",
        "text",
        "str",
        "Enter",
        "key",
        "str",
        "RX",
        "wait-text",
        "send-keys",
    ];
    let began = std::time::Instant::now();
    server.ok(&keys)?;
    // Enter goes in once the program reads again, after its sleep: where
    // fux is told it reads (macOS says a sleep and a read alike).
    assert!(
        !PROBED || began.elapsed() >= std::time::Duration::from_millis(400),
        "{:?}",
        began.elapsed()
    );
    Ok(())
}

/// Replay: `run`'s answer holds all a program printed, lines scrolled into
/// history too, as no snapshot of the screen came before them.
#[test]
fn a_runs_answer_holds_what_scrolled_away() -> Outcome {
    let server = Server::start("")?;
    let burst = "for i in $(seq 0 59); do echo BURST_$i; done; read x";
    let out = server.ok(&run(
        &["str", "BURST_59", "wait-text"],
        &["sh", "-c", burst],
    ))?;
    let later = server.ok(&["str", &id(&out), "pane", "opts", "wait"])?;
    assert!(
        later.contains("BURST_0\n") && later.contains("BURST_59"),
        "{later}"
    );
    Ok(())
}

/// Sealed Set C (TC-026): a line editor that draws its whole line again,
/// prompt and all, at each key: the prompt it draws again where it stood
/// is no news; a wait for it is met by the next prompt, after the reply.
#[test]
fn a_prompt_drawn_again_with_the_typed_line_is_no_news() -> Outcome {
    let server = Server::start("")?;
    let desk = "import os, time, tty\ntty.setraw(0)\nline = ''\nos.write(1, b'desk> ')\n\
                while True:\n    c = os.read(0, 1).decode()\n\
                \x20   if c in '\\r\\n':\n        os.write(1, b'\\r\\n'); time.sleep(0.5)\n\
                \x20       os.write(1, ('OK ' + line + '\\r\\ndesk> ').encode()); line = ''; continue\n\
                \x20   line += c; os.write(1, ('\\r\\x1b[Kdesk> ' + line).encode())\n";
    let pane = id(&server.ok(&run(
        &["str", "desk>", "wait-text"],
        &["python3", "-c", desk],
    ))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "inspect",
        "text",
        "str",
        "Enter",
        "key",
        "str",
        "desk>",
        "wait-text",
        "send-keys",
    ];
    let began = std::time::Instant::now();
    server.ok(&keys)?;
    assert!(
        began.elapsed() >= std::time::Duration::from_millis(400),
        "{:?}",
        began.elapsed()
    );
    Ok(())
}

/// Set B (TB-053): a thread waiting on a timer through a futex (Python's
/// `Event.wait(0.1)`) goes on alone: beside a thread reading the terminal,
/// the program is `waiting`, and a text wait runs to its limit, never 7.
#[test]
fn a_thread_on_a_timed_futex_goes_on_alone() -> Outcome {
    let server = Server::start("")?;
    let ticker = "import threading\nstop = threading.Event()\n\
                  def tick():\n    while not stop.wait(0.1): pass\n\
                  threading.Thread(target=tick, daemon=True).start()\nprint('READY', flush=True)\ninput()\n";
    let started = ["str", "READY", "wait-text"];
    let pane = id(&server.ok(&run(&started, &["python3", "-c", ticker]))?);
    let never = [
        "str",
        &pane,
        "pane",
        "opts",
        "str",
        "NEVER",
        "wait-text",
        "2",
        "timeout",
        "wait",
    ];
    let out = server.fux(&never)?;
    assert_eq!(out.status, 124, "{}{}", out.stdout, out.stderr);
    Ok(())
}

/// Codex review: a wide character before a run on its row takes two cells
/// and is one character: the run is still found where it went, and a wait
/// for its text at the line's end is met.
#[test]
fn a_run_after_a_wide_character_is_found_where_it_went() -> Outcome {
    let server = Server::start("")?;
    let wide = "import os, tty\ntty.setraw(0)\nos.write(1, '界 '.encode())\nos.read(0, 1)\n\
                os.write(1, b'READY')\nos.read(0, 1)\n";
    let pane = id(&server.ok(&run(&["str", "界", "wait-text"], &["python3", "-c", wide]))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "k",
        "text",
        "str",
        "READY$",
        "wait-text",
        "3",
        "timeout",
        "send-keys",
    ];
    let out = server.fux(&keys)?;
    assert_eq!(out.status, 0, "{}{}", out.stdout, out.stderr);
    Ok(())
}

/// Set B review (TB-463): a wait met on a long line wrapped over many rows
/// answers the whole line, not the rows read around the text to find it.
#[test]
fn a_met_wait_answers_the_whole_wrapped_line() -> Outcome {
    let server = Server::start("")?;
    let long = "import os, tty\ntty.setraw(0)\nos.write(1, b'> ')\nos.read(0, 1)\n\
                os.write(1, b'\\r\\nCALIBRATION ' + b'x' * 250 + b' LANE=TAIL\\r\\n')\nos.read(0, 1)\n";
    let size = ["80", "24", "size", "str", ">", "wait-text"];
    let pane = id(&server.ok(&run(&size, &["python3", "-c", long]))?);
    let keys = [
        "str",
        &pane,
        "pane",
        "keys",
        "str",
        "k",
        "text",
        "str",
        "CALIBRATION",
        "wait-text",
        "send-keys",
    ];
    let out = server.ok(&keys)?;
    assert!(out.contains("LANE=TAIL"), "{out}");
    Ok(())
}
