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
    server.ok(&[&p[..], &["str", "never shown", "wait-gone", "wait"]].concat())?;
    let quiet = server.ok(&[&p[..], &["wait"]].concat())?;
    assert_eq!(
        quiet,
        format!("{pane} reading\none\ntwo\n"),
        "what came since it started: no keys went in"
    );
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
    assert_eq!(echoed, format!("{pane} reading\nhello\ngot hello\n"));
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
        assert!(answer.starts_with(&format!("{pane} reading")), "{answer}");
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
    assert_eq!(started, format!("{pane} reading\nready\n"));
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
    assert_eq!(unseen.stdout, format!("{pane} reading · no change\n"));
    let p = ["str", pane.as_str(), "pane", "opts"];
    let before = server.fux(&[&p[..], &["str", "ready", "wait-text", "wait"]].concat())?;
    assert_eq!(
        (before.status, before.stdout.as_str()),
        (7, format!("{pane} reading\nready\n").as_str()),
        "it came before the keys"
    );
    let at_work = keys(&["str", "Enter", "key", "1", "timeout"])?;
    assert_eq!(
        (at_work.status, at_work.stdout.as_str()),
        (124, format!("{pane} busy\nlater\n").as_str())
    );
    let since = server.ok(&[&p[..], &["str", "later", "wait-text", "wait"]].concat())?;
    assert_eq!(since, "later\n", "it came after the keys");
    let now = server.fux(&[&p[..], &["0", "timeout", "wait"]].concat())?;
    assert_eq!(
        (now.status, now.stdout.as_str()),
        (0, format!("{pane} busy\nlater\n").as_str()),
        "what came since the keys"
    );
    assert_eq!(
        server.ok(&["str", &pane, "pane", "str", "state", "get"])?,
        "busy\n"
    );
    Ok(())
}

/// A shell's OSC 133 marks tell when a command ends and how (the answer's
/// `· done S`), and which output was its; a bell or a desktop notification is a pane asking for
/// a person, which `wait-bell` hears and the JSON counts.
#[test]
fn shell_marks_and_bells_are_heard() -> Outcome {
    let server = Server::start("")?;
    let script = r"read go; printf '\033]133;C\007'; echo out1; echo out2; printf '\033]133;D;7\007'; echo prompt; read go; printf '\a\033]9;tests passed\007'; exec cat";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", script]))?);
    let enter = ["str", pane.as_str(), "pane", "keys", "str", "Enter", "key"];
    let done = server.ok(&[&enter[..], &["send-keys"]].concat())?;
    assert_eq!(head(&done), format!("{pane} reading · done 7"));
    assert!(body(&done).contains("out1\nout2\nprompt\n"), "{done}");
    assert_eq!(last_output(&server, &pane)?, "out1\nout2\n");
    server.ok(&[&enter[..], &["wait-bell", "send-keys"]].concat())?;
    let json = server.ok(&["str", &pane, "pane", "opts", "capture", "json"])?;
    for field in [
        r#""command_running":false"#,
        r#""last_status":7"#,
        r#""notification":"tests passed""#,
    ] {
        assert!(json.contains(field), "{field} in {json}");
    }
    assert!(!json.contains(r#""bells":0"#), "{json}");
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
    assert_eq!(server.ok(&again)?, format!("{pane}\n"));
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
    assert!(ignored.ends_with("reading · no change\n"), "{ignored}");
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
    assert!(ignored.ends_with("reading · no change\n"), "{ignored}");
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
    let screen = server.ok(&["str", &new, "pane", "opts", "capture"])?;
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

/// A bash that reads no file of fux's (`--norc`) keeps `PIPESTATUS`, which
/// bash 5.3 empties for good once it imports a PROMPT_COMMAND, and shows its
/// own prompt with nothing of fux's left in its variables.
#[test]
fn a_bare_bash_keeps_its_pipestatus_and_prompt() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(
        &["str", "n", "name", "str", "PS1", "str", "mine> ", "env"],
        &["bash", "--norc", "-i"],
    ))?;
    line(&server, "n", "false | true; echo \"<${PIPESTATUS[*]}>\"")?;
    assert_eq!(last_output(&server, "n")?, "<1 0>\n");
    line(&server, "n", "echo \"[${__fux_pc-}${__fux_ps1-}]\"")?;
    assert_eq!(last_output(&server, "n")?, "[]\n");
    let screen = server.ok(&["str", "n", "pane", "opts", "capture"])?;
    assert!(
        screen.contains("mine> ") && !screen.contains("PROMPT_COMMAND"),
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
    let at_work = server.fux(&typed("sh", "sleep 30", &["1", "timeout"]))?;
    assert_eq!(at_work.status, 124, "{}", at_work.stderr);
    assert!(
        head(&at_work.stdout).ends_with(" busy"),
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
        &["str", small.as_str(), "pane", "opts", "vertical"][..],
        &program(&["sleep", "60"]),
        &["split"],
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
