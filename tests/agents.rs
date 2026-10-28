//! Driving programs from the command line, as a script or an agent does:
//! `run` starts one out of every client's way and keeps its pane once it
//! exits; an action acts at once and answers its pane's version from
//! before it acted; a wait answers once that version differs (its
//! program's end too) or the pane closes; `send-keys` and `send-mouse` send what a
//! person would, sizes are set, and a headless client stands in for a
//! terminal.
mod support;
use std::time::{Duration, Instant};
use support::*;

/// The pane an answer is about: the first word of its first line.
fn id(out: &str) -> String {
    let first = out.split_whitespace().next().unwrap_or_default();
    first.split(':').next().unwrap_or_default().to_owned()
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
/// program, then `run`; at 80 by 24 unless `before` gives a size, as no
/// client is attached to lay it out on.
fn run<'a>(before: &[&'a str], argv: &[&'a str]) -> Vec<&'a str> {
    let size: &[&str] = if before.contains(&"size") {
        &[]
    } else {
        &["80", "24", "size"]
    };
    [&["agent-opts"][..], size, before, &program(argv), &["run"]].concat()
}

/// `send-keys` to `pane` typing `line`, then Enter.
fn typed<'a>(pane: &'a str, line: &'a str) -> Vec<&'a str> {
    vec![
        "str",
        pane,
        "pane",
        "str",
        line,
        "text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ]
}

/// The version an action's answer gives, from before it acted: its
/// first word.
fn version(answer: &str) -> String {
    let first = answer.split_whitespace().next();
    first.unwrap_or_default().to_owned()
}

/// Waits from the version an action answered (`answer`) until its pane
/// shows `text`: its capture then.
fn shows_after(server: &Server, answer: &str, text: &str) -> Result<String, String> {
    server.until(&id(answer), &version(answer), |c| c.contains(text))
}

/// The block of `docs/agents.md` fenced as `lang` first after `after`: as
/// written, its indent taken off.
fn block(after: &str, lang: &str) -> Result<String, String> {
    let docs = include_str!("../docs/agents.md");
    let (_, from) = docs.split_once(after).ok_or(format!("no {after:?}"))?;
    let fence = format!("```{lang}\n");
    let (_, block) = from.split_once(&fence).ok_or(format!("no {lang} block"))?;
    let (block, _) = block.split_once("```").ok_or("no end")?;
    let lines = block.lines().map(|l| l.strip_prefix("  ").unwrap_or(l));
    Ok(lines.map(|l| format!("{l}\n")).collect())
}

/// The marks `docs/agents.md` shows for `shell`, in its startup file.
fn marks(shell: &str) -> Result<String, String> {
    block("## shells:", shell)
}

/// A prompt hook of a user's own, for `shell`, run after the marks' end:
/// it says the status it sees.
fn later_hook(shell: &str) -> &'static str {
    match shell {
        "bash" => "after() { echo \"hook saw $?\"; }\nPROMPT_COMMAND=after\n",
        "zsh" => "after() { echo \"hook saw $?\" }\nprecmd_functions=(after)\n",
        // At its prompt: fish runs its handlers of one event in no order
        // it promises, and the marks end a command at fish_postexec.
        _ => "function after --on-event fish_prompt; echo \"hook saw $status\"; end\n",
    }
}

/// Whether the `bash` here is 4.4 or later, which has `PS0`, as the
/// README's marks need (macOS's own is 3.2).
fn bash_has_ps0() -> Result<bool, String> {
    let version = std::process::Command::new("bash")
        .args(["-c", "echo $((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1]))"])
        .output()
        .map_err(e)?;
    let version: u32 = String::from_utf8_lossy(&version.stdout)
        .trim()
        .parse()
        .map_err(e)?;
    Ok(version >= 404)
}

/// A bash pane named `name` that marks its commands as the docs say, after
/// a prompt hook of its own.
fn marked_bash(server: &Server, name: &str) -> Result<(), String> {
    let rc = server.dir.join(format!("{name}.bashrc"));
    let (hook, marks) = (later_hook("bash"), marks("bash")?);
    std::fs::write(&rc, format!("PS1='$ '\n{hook}{marks}")).map_err(e)?;
    let rc = rc.to_string_lossy().into_owned();
    let argv = ["bash", "--noprofile", "--rcfile", &rc, "-i"];
    let made = server.ok(&run(&["str", name, "name"], &argv))?;
    server.until(name, &version(&made), |c| c.ends_with("$\n"))?;
    Ok(())
}

/// Types `line` into shell pane `pane` and waits, as the docs' recipe
/// does, until a shell command ends there: the version's fourth count,
/// `DONE`, grows.
fn line_done(server: &Server, pane: &str, line: &str) -> Outcome {
    let since = version(&server.ok(&typed(pane, line))?);
    let done = |v: &str| count(v, 3).and_then(|d| d.parse::<u64>().ok());
    let (before, deadline) = (done(&since), after(PATIENCE));
    let mut at = since;
    while done(&at) <= before {
        at = server.changed(pane, &at, deadline)?;
    }
    Ok(())
}

/// A program `run` starts keeps its pane once it exits: its last screen,
/// and its status, which a wait and `ls` report (a signal's too); one that
/// cannot start is refused. It takes no more input. The program starts at
/// its `size`, in the `agent` workspace, which no client is moved to.
#[test]
fn a_run_program_keeps_its_pane_screen_and_status() -> Outcome {
    let server = Server::start("")?;
    let script = "stty size; echo started; exit 3";
    let started = server.ok(&run(&["40", "10", "size"], &["sh", "-c", script]))?;
    let pane = id(&started);
    assert_eq!(server.exits(&pane)?, "3");
    // Waited on from its start, it has exited: answered at once; from now,
    // nothing changes.
    let since = version(&started);
    let from_start = ["str", &since, "changed", "wait", "json"];
    let json = server.ok(&from_start)?;
    assert!(json.contains(r#""exited":3,"status":0"#), "{json}");
    let now = server.fux(&["str", &pane, "pane", "1", "timeout", "wait"])?;
    assert_eq!(now.status, 124, "{}", now.stdout);
    let screen = server.ok(&["str", &pane, "pane", "capture"])?;
    assert_eq!(
        screen, "10 40\nstarted\n",
        "trimmed, and sized from the start"
    );
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("+2 agent"), "{ls}");
    assert!(ls.contains(&format!("{pane} sh 40x10 exited 3")), "{ls}");
    let refused = server.fux(&["str", &pane, "pane", "str", "x", "text", "send-keys"])?;
    assert_eq!(refused.status, 1);
    assert!(
        refused.stderr.starts_with("fux: not_running: ") && refused.stderr.contains("exited 3"),
        "{}",
        refused.stderr
    );
    let missing = server.fux(&run(&[], &["/no/such/program"]))?;
    assert_eq!(missing.status, 1);
    assert!(
        missing.stderr.contains("No such file"),
        "{}",
        missing.stderr
    );
    // One a signal ends is said to be killed by it.
    let started = server.ok(&run(&[], &["sleep", "30"]))?;
    let pane = id(&started);
    server.ok(&["str", &pane, "pane", "str", "KILL", "signal"])?;
    server.exits(&pane)?;
    let since = version(&started);
    let waited = ["str", &since, "changed", "wait"];
    let text = server.ok(&waited)?;
    assert!(text.ends_with(" killed by SIGKILL\n"), "{text}");
    let json = server.ok(&[&waited[..], &["json"]].concat())?;
    assert!(json.contains(r#""signal":9,"status":0"#), "{json}");
    Ok(())
}

/// An action acts at once and answers its pane's version from just before
/// it acted; a wait from that version answers once the version differs,
/// with what changed, however late it is asked: nothing between is
/// missed. A wait from now on a pane that does not change times out (124).
#[test]
fn a_wait_from_an_action_s_version_misses_nothing() -> Outcome {
    let server = Server::start("")?;
    let started = server.ok(&run(
        &[],
        &["sh", "-c", "echo ready; read x; echo got; sleep 30"],
    ))?;
    let pane = id(&started);
    server.until(&pane, &version(&started), |c| c.contains("ready"))?;
    let began = Instant::now();
    let sent = server.ok(&typed(&pane, "x"))?;
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    let since = version(&sent);
    assert_eq!(sent, format!("{since}\n"));
    assert!(since.starts_with(&format!("{pane}:")), "{since}");
    // Asked once the program answered: met at once, from the version held.
    server.until(&pane, &since, |c| c.contains("got"))?;
    let changed = server.ok(&["str", &since, "changed", "wait"])?;
    assert!(
        changed.starts_with(&format!("{pane}:")) && changed.ends_with(" screen\n"),
        "{changed}"
    );
    let json = server.ok(&["str", &since, "changed", "wait", "json"])?;
    assert!(
        json.contains(r#""changed":["screen"],"status":0"#),
        "{json}"
    );
    let still = server.fux(&["str", &pane, "pane", "1", "timeout", "wait"])?;
    assert_eq!(still.status, 124, "{}{}", still.stdout, still.stderr);
    assert!(still.stdout.ends_with(" timeout\n"), "{}", still.stdout);
    let bad = server.fux(&["str", "0.1.2.0.0", "changed", "wait"])?;
    assert!(bad.stderr.starts_with("fux: bad_value: "), "{}", bad.stderr);
    // Counts this pane never had (a program started more often, or more
    // output read), or another server run's, are refused.
    let server_run = since.rsplit(':').next().unwrap_or_default();
    for other in [
        format!("{pane}:3.0.0.0.0:{server_run}"),
        format!("{pane}:0.99999.0.0.0:{server_run}"),
        format!("{pane}:0.1.0.0.0:1-0"),
    ] {
        let other = server.fux(&["str", &other, "changed", "wait"])?;
        let refused = other.stderr.starts_with("fux: bad_value: ");
        assert!(refused, "{}", other.stderr);
    }
    // In one program, a wait on the action's answer waits from the version
    // it holds, a timeout given between them too: asked again once the
    // change surely came, it answers at once, where one from now would
    // time out.
    let echoes = id(&server.ok(&run(&[], &["sh", "-c", "read x; sleep 30"]))?);
    let twice = [
        "dup", "5", "timeout", "wait", "drop", "1", "timeout", "wait",
    ];
    let answer = server.ok(&[&typed(&echoes, "x")[..], &twice].concat())?;
    assert!(answer.ends_with(" screen\n"), "{answer}");
    Ok(())
}

/// A version counts bells and notifications rung, and a wait says one
/// rang; a program started again is said so, its counts afresh.
#[test]
fn bells_and_respawns_change_the_version() -> Outcome {
    let server = Server::start("")?;
    let script = r"read x; printf '\a'; exec cat";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", script]))?);
    let mut at = version(&server.ok(&typed(&pane, ""))?);
    loop {
        let waited = ["str", &at, "changed", "10", "timeout", "wait"];
        let out = server.ok(&waited)?;
        if out.contains(" bell") {
            break;
        }
        at = version(&out);
    }
    assert_eq!(count(&server.version(&pane)?, 2), Some("1"));
    let at = server.version(&pane)?;
    let respawned = server.ok(&["str", &pane, "pane", "kill-first", "respawn"])?;
    assert_eq!(count(&respawned, 0), Some("1"), "{respawned}");
    let answer = server.ok(&["str", &at, "changed", "wait"])?;
    assert!(answer.contains(" respawned"), "{answer}");
    Ok(())
}

/// A shell's OSC 133 marks count the commands that end in its pane's
/// version, `last_status` says how the last ended and `last-output` reads
/// its output alone; a prompt hook after the marks sees the command's
/// status; a line that runs nothing ends no command. Where no command was
/// marked, `last-output` says so and the count stays. The marks are the
/// docs', for each shell here.
#[test]
fn shell_marks_count_commands_and_give_their_output() -> Outcome {
    let server = Server::start("")?;
    let bash = bash_has_ps0()?;
    if bash {
        marked_bash(&server, "b")?;
    }
    let status = |pane: &str| server.ok(&["str", pane, "pane", "str", "last_status", "get"]);
    let check = |pane: &str| -> Outcome {
        line_done(&server, pane, "echo one; false")?;
        assert_eq!(status(pane)?, "1\n");
        let output = server.ok(&["str", pane, "pane", "last-output", "capture"])?;
        assert_eq!(output, "one\n");
        server.shows(pane, "hook saw 1")?;
        // An empty line and a syntax error (in bash and zsh) run nothing:
        // the next command is the one that ends. A comment runs nothing in bash; interactive
        // zsh runs `#` as a command (unless interactive_comments), and fish
        // marks a comment line as a command.
        let done = |v: &str| count(v, 3).and_then(|d| d.parse::<u64>().ok());
        let mut ended = Vec::new();
        // fish, given `)`, does not end the command typed after it as the
        // others do: its syntax errors are left unsaid.
        let lines: &[&str] = match pane {
            "fish" => &["", "# nothing"],
            _ => &["", "# nothing", ")"],
        };
        let last = |pane: &str| server.ok(&["str", pane, "pane", "last-output", "capture"]);
        for (n, &nothing) in lines.iter().enumerate() {
            let before = done(&server.version(pane)?);
            server.ok(&typed(pane, nothing))?;
            // Counted once the echo has ended, its own output the last a
            // command left and no command running: the line before it, read
            // first, has ended what it ends by then. (The echo's wait alone
            // may end at the end that line makes, and a running command's
            // output is its last output too.)
            line_done(&server, pane, &format!("echo two {n}"))?;
            let echoed = format!("two {n}\n");
            let running =
                |pane: &str| server.ok(&["str", pane, "pane", "str", "command_running", "get"]);
            eventually("the echo's end", || {
                Ok(last(pane)? == echoed && running(pane)? == "false\n")
            })?;
            let after = done(&server.version(pane)?);
            if after != before.map(|d| d.saturating_add(1)) {
                ended.push(nothing);
            }
        }
        let comment: &[&str] = match pane {
            "zsh" | "fish" => &["# nothing"],
            _ => &[],
        };
        assert_eq!(ended, comment, "{pane}: the lines that ended a command");
        assert_eq!(status(pane)?, "0\n");
        Ok(())
    };
    if bash {
        check("b")?;
    }
    // A shell the tests may have: its marks as the docs have them, each
    // read from its own startup file, where the docs put them.
    let shells = [
        ("zsh", "ZDOTDIR", ".zshrc"),
        ("fish", "XDG_CONFIG_HOME", "fish/config.fish"),
    ];
    for (shell, home, rc) in shells {
        if std::env::var_os("FUX_TEST_SHELLS").is_none() {
            break;
        }
        let dir = server.dir.join(shell);
        let rc = dir.join(rc);
        std::fs::create_dir_all(rc.parent().ok_or("a directory")?).map_err(e)?;
        std::fs::write(&rc, format!("{}{}", later_hook(shell), marks(shell)?)).map_err(e)?;
        let dir = dir.to_string_lossy().into_owned();
        let words = [
            "agent-opts",
            "80",
            "24",
            "size",
            "str",
            shell,
            "name",
            "str",
            home,
            "str",
            &dir,
            "env",
        ];
        server.ok(&[&words[..], &program(&[shell, "-i"]), &["run"]].concat())?;
        check(shell)?;
    }
    let plain = server.ok(&run(&[], &["sh"]))?;
    let refused = server.fux(&["str", &id(&plain), "pane", "last-output", "capture"])?;
    assert!(
        refused.stderr.starts_with("fux: no_marks: "),
        "{}",
        refused.stderr
    );
    let echoed = server.ok(&typed(&id(&plain), "echo un''marked"))?;
    shows_after(&server, &echoed, "unmarked")?;
    let done = server.version(&id(&plain))?;
    assert_eq!(count(&done, 3), Some("0"), "{done}");
    Ok(())
}

/// Keys reach the program as they are sent, in order: a paste bracketed
/// as it asked, a count repeating only the key before it, bytes as they
/// are, a click encoded as it asked. The program says all it has read so
/// far after each read: its terminal may split what one action sent.
#[test]
fn input_reaches_the_program_as_sent() -> Outcome {
    const READS: &str = r#"
import os, sys, tty
tty.setraw(0)
os.write(1, b"\x1b[?2004h\x1b[?1000h\x1b[?1006hready\r\n")
seen = b""
while not seen.endswith(b"q"):
    seen += os.read(0, 256)
    os.write(1, ("seen " + seen.hex() + "\r\n").encode())
"#;
    let server = Server::start("")?;
    let path = server.dir.join("reads.py");
    std::fs::write(&path, READS).map_err(e)?;
    let script = path.to_string_lossy().into_owned();
    let started = server.ok(&run(&[], &["python3", "-I", &script]))?;
    let pane = id(&started);
    shows_after(&server, &started, "ready")?;
    // All the program read so far, as it said it.
    let seen = std::cell::RefCell::new(String::new());
    let sent = |keys: &[&str], read: &str| -> Outcome {
        seen.borrow_mut().push_str(read);
        let said = format!("seen {}\n", seen.borrow());
        let send = [&["str", pane.as_str(), "pane"][..], keys, &["send-keys"]];
        shows_after(&server, &server.ok(&send.concat())?, &said)?;
        Ok(())
    };
    sent(&["str", "Escape", "key"], "1b")?;
    sent(&["str", "hi", "text", "str", "Enter", "key"], "68690d")?;
    sent(&["str", "a b", "paste"], "1b5b3230307e6120621b5b3230317e")?;
    sent(&["str", "c3a9", "bytes"], "c3a9")?;
    sent(
        &[
            "0", "3", "range", "each", "drop", "str", "BSpace", "key", "end", "str", "x", "text",
        ],
        "7f7f7f78",
    )?;
    let click = ["str", "click", "2", "4"];
    seen.borrow_mut()
        .push_str("1b5b3c303b353b334d1b5b3c303b353b336d");
    let said = format!("seen {}\n", seen.borrow());
    let clicked = [&["str", pane.as_str(), "pane"][..], &click, &["send-mouse"]];
    shows_after(&server, &server.ok(&clicked.concat())?, &said)?;
    server.ok(&[
        "str",
        pane.as_str(),
        "pane",
        "str",
        "q",
        "text",
        "send-keys",
    ])?;
    assert_eq!(server.exits(&pane)?, "0");
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
    let left = id(left);
    let left = left.as_str();
    let size = |pane: &str| -> Result<String, String> {
        let fields = server.ok(&[
            "str", pane, "pane", "dup", "str", "cols", "get", "swap", "str", "rows", "get",
        ])?;
        let (cols, rows) = fields.trim().split_once('\n').ok_or("pane dimensions")?;
        Ok(format!("{cols}x{rows}"))
    };
    assert_eq!(size(left)?, "100x20");
    server.ok(&["str", left, "pane", "100", "22", "resize"])?;
    assert_eq!(size(left)?, "100x22", "a pane's tab");
    server.ok(&["str", tab, "tab", "100", "20", "resize"])?;
    let right = id(&server.ok(&["str", left, "pane", "str", "horizontal", "axis", "split"])?);
    server.ok(&[
        "str",
        &right,
        "pane",
        "25",
        "percent",
        "str",
        "horizontal",
        "resize-pane",
    ])?;
    assert_eq!(size(&right)?, "25x20");
    assert_eq!(size(left)?, "74x20");
    let refused = server.fux(&[
        "str",
        left,
        "pane",
        "100",
        "str",
        "horizontal",
        "resize-pane",
    ])?;
    assert!(
        refused.stderr.starts_with("fux: size: "),
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

/// A headless client stands in for a terminal of a size set from the
/// command line: it prints its id, shows what it is sent, and goes on a
/// signal. A terminal's client sets its own size.
#[test]
fn a_headless_client_is_sized_from_the_command_line() -> Outcome {
    use std::io::BufRead;
    let server = Server::start("")?;
    let mut client = std::process::Command::new(FUX)
        .args(["opts", "50", "10", "size", "attach"])
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
    server.ok(&["str", "c1", "client", "60", "12", "resize"])?;
    assert!(server.ok(&["ls"])?.contains("client c1 60x12"));
    let capture = ["str", "c1", "client", "capture"];
    assert_eq!(
        server.ok(&[&capture[..], &["kind"]].concat())?.trim(),
        "client-capture"
    );
    eventually("a paint", || Ok(server.ok(&capture)?.contains("main")))?;
    // Keys typed at it are a person's: the prefix and `c` open copy mode.
    let keys = [
        "str",
        "c1",
        "client",
        "str",
        "C-b",
        "key",
        "str",
        "c",
        "text",
        "send-keys",
    ];
    server.ok(&keys)?;
    eventually("copy mode", || Ok(server.ok(&capture)?.contains("COPY")))?;
    server.ok(&[
        "str",
        "c1",
        "client",
        "str",
        "q",
        "text",
        "send-keys",
        "drop",
    ])?;
    let mut terminal = server.attach(10, 40)?;
    terminal.ready()?;
    let refused = server.fux(&["str", "c2", "client", "60", "12", "resize"])?;
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
    // P1 9: square pixels (DEC STD 070, 7.1). The cursor stays on the
    // picture's last row (DEC STD 070, 11.3; xterm "requires only a single
    // newline"), so a line goes first lest the text cover it.
    let asked = r"printf '\033[c'; read -r _; printf '\033P9q#1;2;100;0;0!20~-!20~\033\\'; echo; echo drawn";
    shows_after(
        &server,
        &server.ok(&typed("%1", asked))?,
        "62;4;6;21;22;28c",
    )?;
    let enter = ["str", "%1", "pane", "str", "Enter", "key", "send-keys"];
    shows_after(&server, &server.ok(&enter)?, "drawn")?;
    eventually("the picture sent", || {
        client.pump()?;
        Ok(painted(&client, b"\x1bP0;1;0q\"1;1;20;12"))
    })?;
    let json = server.ok(&["str", "%1", "pane", "capture", "json"])?;
    assert!(
        json.contains(r#""col":0,"rows":1,"cropped":0,"cols":2,"width":20,"height":12}]"#),
        "{json}"
    );
    // A placement change clears the screen, so unchanged pictures must be sent again.
    client.painted.clear();
    let mut presentation = fux::script::default_state()
        .map_err(e)?
        .preferences
        .presentation();
    presentation.cell_placement = true;
    client.act(fux::act::Act::Presentation(presentation))?;
    eventually("the picture resent after placement changes", || {
        client.pump()?;
        Ok(painted(&client, b"\x1bP0;1;0q\"1;1;20;12"))
    })?;
    Ok(())
}

/// `respawn` starts a run program again in its pane, found by its given
/// name; `all-history` reads all history,
/// `main-screen` what a full-screen program hides, `alt-screen` its last
/// screen; a mistyped key name is refused.
#[test]
fn run_programs_are_restarted_and_read_whole() -> Outcome {
    let server = Server::start("")?;
    let script = "echo before; seq 500; printf '\\033[?1049h'; echo \"full $V\"; read go";
    let before = [
        "str", "job", "name", "str", "V", "str", "a", "env", "40", "10", "size",
    ];
    let started = server.ok(&run(&before, &["sh", "-c", script]))?;
    let pane = id(&started);
    shows_after(&server, &started, "full a")?;
    let main = [
        "str",
        &pane,
        "pane",
        "main-screen",
        "all-history",
        "capture",
    ];
    let main = server.ok(&main)?;
    assert!(main.starts_with("before\n1\n"), "{main}");
    assert!(main.contains("\n500\n"), "{main}");
    let refused = server.fux(&["str", &pane, "pane", "str", "Entr", "key", "send-keys"])?;
    assert_eq!(refused.status, 1);
    assert!(refused.stderr.contains("Entr"), "{}", refused.stderr);
    let busy = server.fux(&["str", &pane, "pane", "respawn"])?;
    assert_eq!(busy.status, 1, "it runs: {}", busy.stderr);
    let again = [
        "str",
        "job",
        "pane",
        "kill-first",
        "str",
        "V",
        "str",
        "b",
        "env",
        "respawn",
    ];
    let again = server.ok(&again)?;
    assert!(again.starts_with(&format!("{pane}:1.")), "{again}");
    shows_after(&server, &again, "full b")?;
    // A full-screen program's last screen, after it left it and exited.
    let menu = r"printf '\033[?1049hpicked beta'; printf '\033[?1049l'";
    let menu = id(&server.ok(&run(&[], &["sh", "-c", menu]))?);
    server.exits(&menu)?;
    assert!(
        !server
            .ok(&["str", &menu, "pane", "capture"])?
            .contains("picked")
    );
    assert_eq!(
        server.ok(&["str", &menu, "pane", "alt-screen", "capture"])?,
        "picked beta\n"
    );
    Ok(())
}

/// Pane and tab names are independent, id spellings are refused, and a
/// click outside a pane or a signal to its default shell is refused.
#[test]
fn names_clicks_and_signals_are_checked() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(&["str", "ed", "name"], &["cat"]))?;
    assert_eq!(
        server.fux(&run(&["str", "@1", "name"], &["cat"]))?.status,
        1
    );
    server.ok(&[
        "opts", "80", "24", "size", "str", "+1", "in", "str", "ed", "name", "new-tab",
    ])?;
    assert_eq!(server.ok(&["str", "ed", "pane"])?, "%2\n");
    assert_eq!(server.ok(&["str", "ed", "tab"])?, "@3\n");
    // A common spelling is a key's name too; an unknown name is refused with the vocabulary hint.
    server.ok(&["str", "ed", "pane", "str", "Backspace", "key", "send-keys"])?;
    let typo = server.fux(&["str", "ed", "pane", "str", "C-Bakspace", "key", "send-keys"])?;
    assert!(
        typo.stderr
            .contains("is no key's name (key-names lists them)"),
        "{}",
        typo.stderr
    );
    server.ok(&["str", "ed", "pane", "tab", "40", "12", "resize"])?;
    assert!(
        server.ok(&["ls"])?.contains(" 40x12"),
        "resized by the named pane's tab"
    );
    let clicks = "printf '\\033[?1000h\\033[?1006hready'; exec cat";
    let before = ["str", "m", "name", "20", "5", "size"];
    shows_after(
        &server,
        &server.ok(&run(&before, &["sh", "-c", clicks]))?,
        "ready",
    )?;
    let outside = ["str", "m", "pane", "str", "click", "5", "0", "send-mouse"];
    let outside = server.fux(&outside)?;
    assert!(
        outside.stderr.starts_with("fux: mouse: "),
        "{}",
        outside.stderr
    );
    let signalled = server.fux(&["str", "%1", "pane", "str", "HUP", "signal"])?;
    assert!(
        signalled.stderr.starts_with("fux: only_shell: "),
        "{}",
        signalled.stderr
    );
    Ok(())
}

/// A row range takes each line it touches whole; a column range
/// cuts a wide glyph to a blank, and reaches the last column.
#[test]
fn a_row_range_takes_joined_lines_whole() -> Outcome {
    let server = Server::start("")?;
    let long = "echo aaaaaaaaaaaaaaaaaaaaBBBBBBBBBBBBBBBBBBBBcc; echo next; exec cat";
    let started = server.ok(&run(&["20", "6", "size"], &["sh", "-c", long]))?;
    let pane = id(&started);
    shows_after(&server, &started, "next")?;
    let line = server.ok(&["str", &pane, "pane", "1", "2", "rows", "capture"])?;
    assert_eq!(line, "aaaaaaaaaaaaaaaaaaaaBBBBBBBBBBBBBBBBBBBBcc\n");
    let plain = server.ok(&["str", &pane, "pane", "capture"])?;
    assert!(
        plain.contains("aaaaaaaaaaaaaaaaaaaaBBBBBBBBBBBBBBBBBBBBcc\nnext"),
        "{plain}"
    );
    let rows = server.ok(&["str", &pane, "pane", "wrapped", "capture"])?;
    assert!(
        rows.contains("aaaaaaaaaaaaaaaaaaaa\nBBBBBBBBBBBBBBBBBBBB\ncc\n"),
        "{rows}"
    );
    // A column range cuts a wide glyph to a blank, a column a cell.
    let wide = "printf 'A界BC              Z'; exec cat";
    let wide = server.ok(&run(&["20", "6", "size"], &["sh", "-c", wide]))?;
    shows_after(&server, &wide, "Z")?;
    let wide = id(&wide);
    let cut = |first: &str, last: &str| {
        server.ok(&[
            "str", &wide, "pane", "0", "1", "rows", first, last, "cols", "capture",
        ])
    };
    assert_eq!(cut("2", "5")?, " BC\n");
    assert_eq!(cut("0", "2")?, "A\n");
    assert_eq!(cut("19", "20")?, "Z\n");
    assert_eq!(cut("19", "25")?, "Z\n");
    assert_eq!(cut("2", "2")?, "");
    // The cursor is the screen's: on the empty row after the last line;
    // and every row kept was read.
    let json = server.ok(&["str", &pane, "pane", "capture", "json"])?;
    assert!(
        json.contains(r#""screen_cursor":[4,0],"#) && json.contains(r#""above":0"#),
        "{json}"
    );
    Ok(())
}

/// A wait on a workspace waits on each of its panes, and answers a line for
/// each: here none changed, so each timed out, the whole 124. (`session`'s
/// tests begin a wait before what it waits for: the answer of the pane
/// that changed, and of one that closed.)
#[test]
fn a_wait_on_a_workspace_answers_each_of_its_panes() -> Outcome {
    let server = Server::start("")?;
    let one = id(&server.ok(&run(&[], &["sleep", "30"]))?);
    let two = id(&server.ok(&run(&[], &["sleep", "30"]))?);
    let timed = server.fux(&["str", "agent", "workspace", "1", "timeout", "wait"])?;
    assert_eq!(timed.status, 124, "{}", timed.stderr);
    for pane in [&one, &two] {
        let line = timed
            .stdout
            .lines()
            .find(|l| l.starts_with(&format!("{pane}:")));
        assert!(
            line.is_some_and(|l| l.ends_with(" timeout")),
            "{}",
            timed.stdout
        );
    }
    // A tab's action's answer holds no one pane's version, though it has
    // one pane: not waited on, and an array as JSON.
    let keys = ["str", &one, "pane", "tab", "str", "y", "text", "send-keys"];
    let again = server.fux(&[&keys[..], &["wait"]].concat())?;
    assert!(again.stderr.starts_with("fux: type: "), "{}", again.stderr);
    let json = server.ok(&[&keys[..], &["json"]].concat())?;
    assert!(
        json.starts_with(&format!("[{{\"pane\":\"{one}\"")),
        "{json}"
    );
    Ok(())
}

/// An action's answer, and a wait's, is its pane: a word that wants a pane
/// takes it, never a pane named by what the program wrote (`%13 fake`).
#[test]
fn an_answer_is_its_pane() -> Outcome {
    let server = Server::start("")?;
    let script = "read x; echo '%13 fake'; exec cat";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", script]))?);
    let since = server.version(&pane)?;
    let sent = [&typed(&pane, "go")[..], &["str", "name", "get"]];
    assert_eq!(server.ok(&sent.concat())?.trim(), "null");
    server.shows(&pane, "%13 fake")?;
    let waited = ["str", &since, "changed", "wait", "str", "name", "get"];
    assert_eq!(server.ok(&waited)?.trim(), "null");
    Ok(())
}

/// Input as large as one send-keys sends (900 KiB, far past a typed
/// paste's 64 KiB) goes in one, all of it, from a program read from a
/// file; more is refused as too long, before anything is sent.
#[test]
fn send_keys_takes_input_up_to_its_limit() -> Outcome {
    let server = Server::start("")?;
    let count = "stty -echo; head -c 921600 | wc -c; cat > /dev/null";
    let sink = id(&server.ok(&run(&[], &["sh", "-c", count]))?);
    let line: String = std::iter::repeat_n('a', 99).chain(['\n']).collect();
    let text: String = std::iter::repeat_n(line, 9216).collect();
    let file = server.dir.join("paste.fux");
    std::fs::write(&file, format!("{sink} str '{text}' paste send-keys")).map_err(e)?;
    shows_after(
        &server,
        &server.ok(&["--file", &file.to_string_lossy()])?,
        "921600",
    )?;
    let long: String = std::iter::repeat_n('b', 1000).collect();
    let many = format!("{sink} 0 1000 range each drop str {long} text end send-keys");
    let refused = server.fux(&[&many])?;
    assert_eq!(refused.status, 1);
    assert!(refused.stderr.contains("too_long"), "{}", refused.stderr);
    // Keys count as the request carries them, their names and more: so
    // many that the request could not be sent are too long too.
    let keys = format!("{sink} 0 90000 range each drop str C-M-F12 key end send-keys");
    let refused = server.fux(&[&keys])?;
    assert_eq!(refused.status, 1);
    assert!(refused.stderr.contains("too_long"), "{}", refused.stderr);
    Ok(())
}

/// `json` on a saved capture converts what was captured: never a later
/// read of the pane, which may have moved on or closed.
#[test]
fn json_of_a_saved_capture_is_that_capture() -> Outcome {
    let server = Server::start("")?;
    let script = "echo BEFORE; read x; echo LA\"\"TER; read y";
    let started = server.ok(&run(&[], &["sh", "-c", script]))?;
    let pane = id(&started);
    shows_after(&server, &started, "BEFORE")?;
    let json = server.ok(&[
        "str",
        &pane,
        "pane",
        "let",
        "P",
        "P",
        "capture",
        "let",
        "C",
        "P",
        "str",
        "Enter",
        "key",
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

/// In a list, an answer is as `json` gives it alone: a capture, another,
/// and a listing are their objects.
#[test]
fn an_answer_in_a_list_is_its_object() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&[], &["sh", "-c", "read x"]))?);
    let capture = ["str", &pane, "pane"];
    let json = server.ok(&[
        &["list"][..],
        &capture,
        &["capture"],
        &capture,
        &["capture", "ls", "end", "json"],
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
    let on = ["str", &pane, "pane"];
    let opts = ["opts"];
    let cat = ["str", "cat", "program"];
    let since = server.version(&pane)?;
    for (before, unused, word) in [
        // The checker cannot know a list element's kind; runtime option
        // validation still refuses an explicit wait baseline on capture.
        (
            &[][..],
            &["list", "str", &since, "changed", "end", "0", "at"][..],
            "capture",
        ),
        (&on, &["5", "timeout"], "capture"),
        (&on, &["100", "40", "size"], "capture"),
        (&on, &["str", "x", "name"], "wait"),
        (&on, &["10", "history"], "wait"),
        (&on, &["str", "nowhere", "in"], "split"),
        (&on, &["str", "nowhere", "in"], "respawn"),
        (&on, &["str", "x", "name"], "respawn"),
        (&on, &["100", "30", "size"], "respawn"),
        (&on, &["str", "/", "cwd"], "respawn"),
        (&on, &["str", "vertical", "axis"], "respawn"),
        (&opts, &["str", "nowhere", "in"], "new-workspace"),
        (&opts, &["5", "timeout"], "new-tab"),
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
        &["str", "horizontal", "axis"][..],
        &["str", "vertical", "axis"],
        &["kill-first"],
        &["full"],
        &["alt-screen"],
        &["last-output"],
        &["wrapped"],
        &["0", "5", "rows"],
    ] {
        let refused = server.fux(&[&opts[..], unused, &cat, &["run"]].concat())?;
        assert_eq!(refused.status, 1, "{unused:?} run: {}", refused.stdout);
        assert!(
            refused.stderr.starts_with(
                if matches!(
                    unused,
                    ["str", "horizontal", "axis"] | ["str", "vertical", "axis"] | ["kill-first"]
                ) {
                    "fux: unused: "
                } else {
                    "fux: type: "
                }
            ),
            "{unused:?} run: {}",
            refused.stderr
        );
    }
    // Nothing was made: the two panes, and no workspace "nowhere".
    let ls = server.ok(&["ls"])?;
    assert!(!ls.contains("nowhere"), "{ls}");
    assert_eq!(
        ls.lines().filter(|line| line.starts_with("    %")).count(),
        2,
        "{ls}"
    );
    Ok(())
}

/// A failed wait answers as text, whatever follows it, and says why on
/// one line of stderr; nothing after it runs.
#[test]
fn a_failed_wait_answers_as_text() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&[], &["sleep", "30"]))?);
    let p = ["str", pane.as_str(), "pane"];
    let out = server.fux(&[&p[..], &["1", "timeout", "wait", "json"]].concat())?;
    assert_eq!(out.status, 124, "{}", out.stderr);
    assert!(out.stdout.starts_with(&format!("{pane}:")) && out.stdout.ends_with(" timeout\n"));
    assert_eq!(out.stderr.lines().count(), 1, "{}", out.stderr);
    assert!(out.stderr.starts_with("fux: timeout: "), "{}", out.stderr);
    // The same when the wait is a defined word's last step.
    let wait = [&["define", "w"], &p[..], &["1", "timeout", "wait", "end"]];
    let again = server.fux(&[&wait.concat()[..], &["w", "json"]].concat())?;
    assert_eq!(again.status, 124, "{}", again.stderr);
    assert!(again.stdout.ends_with(" timeout\n"), "{}", again.stdout);
    Ok(())
}

/// While a command runs, its output so far is the last output; a
/// full-screen one's is its screen, the cursor where it is on it.
#[test]
fn last_output_is_the_running_command_s_so_far() -> Outcome {
    if !bash_has_ps0()? {
        return Ok(());
    }
    let server = Server::start("")?;
    marked_bash(&server, "r")?;
    line_done(&server, "r", "echo BEFORE")?;
    let asks = server.ok(&typed("r", "echo CURRENT; read -r answer"))?;
    shows_after(&server, &asks, "\nCURRENT\n")?;
    let output = server.ok(&["str", "r", "pane", "last-output", "capture"])?;
    assert_eq!(output, "CURRENT\n");
    line_done(&server, "r", "")?;
    let draw = "printf 'before\\n\\033[?1049h\\033[Habc\\ndef\\nxy%s\\033[2;2H' z; read x";
    shows_after(&server, &server.ok(&typed("r", draw))?, "xyz")?;
    let json = server.ok(&["str", "r", "pane", "last-output", "capture", "json"])?;
    assert!(json.contains("\"screen_cursor\":[1,1]"), "{json}");
    Ok(())
}

/// A given name matching the old program survives respawn. A wait
/// from the ended program's version tells the new one's end too.
#[test]
fn respawn_keeps_a_given_name_matching_the_old_program() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&["str", "sh", "name"], &["sh", "-c", "exit 0"]))?);
    server.exits(&pane)?;
    let ended = server.version(&pane)?;
    server.ok(&["str", &pane, "pane", "str", "true", "program", "respawn"])?;
    let name = server.ok(&["str", &pane, "pane", "str", "name", "get"])?;
    assert_eq!(name, "sh\n");
    server.exits(&pane)?;
    let answer = server.ok(&["str", &ended, "changed", "wait"])?;
    assert!(answer.ends_with(" respawned exited 0\n"), "{answer}");
    Ok(())
}

/// `env` gives a program a pane, tab or workspace as fux writes it, so a
/// key's program can act on the pane it came from.
#[test]
fn env_takes_a_pane_tab_or_workspace() -> Outcome {
    let server = Server::start("")?;
    let named = id(&server.ok(&run(&["str", "ed", "name"], &["sleep", "30"]))?);
    let before = [
        "str", "P", "%1", "env", "str", "T", "@1", "env", "str", "W", "+1", "env", "str", "N",
        "str", "ed", "pane", "env",
    ];
    let script = "echo \"$P $T $W $N\"";
    let pane = id(&server.ok(&run(&before, &["sh", "-c", script]))?);
    server.exits(&pane)?;
    let capture = server.ok(&["str", &pane, "pane", "capture"])?;
    assert_eq!(capture, format!("%1 @1 +1 {named}\n"));
    Ok(())
}

/// A pane's `FUX` is the fux running it, so a key's program reaches fux
/// with no `fux` on PATH.
#[test]
fn a_pane_gets_the_fux_running_it() -> Outcome {
    let server = Server::start("")?;
    let before = ["str", "WANT", "str", fux(), "env"];
    let script = "PATH=/nonexistent; \"$FUX\" here capture >/dev/null && [ \"$FUX\" -ef \"$WANT\" ] && echo same";
    let pane = id(&server.ok(&run(&before, &["/bin/sh", "-c", script]))?);
    server.exits(&pane)?;
    let capture = server.ok(&["str", &pane, "pane", "capture"])?;
    assert_eq!(capture, "same\n");
    Ok(())
}

/// The docs' recipes run as written, as programs: a quiet screen, a
/// reply's text, a shell command's end and a program's exit, and a
/// deadline's fallback, each in a pane of its own; and every program the
/// guide shows checks.
#[test]
fn the_docs_recipes_run_as_written() -> Outcome {
    let server = Server::start("")?;
    let pane = |script: &str| -> Result<String, String> {
        Ok(id(&server.ok(&run(&[], &["sh", "-c", script]))?))
    };
    let recipe = |after: &str| block(after, "fux");
    let talks = pane("read x; echo one; sleep 0.3; echo two; sleep 30")?;
    let quiet = recipe("- **Quiet for N seconds**")?
        .replace("%2", &talks)
        .replace("5 quiet", "1 quiet");
    let asks = pane("read q; sleep 1; echo the answer is 42; sleep 30")?;
    let text = recipe("- **A text the reply writes:**")?.replace("%2", &asks);
    // A command's end, marked, once a line comes.
    let marks = r#"read x; printf '\033]133;C\007hi\n\033]133;D;5\007'; sleep 30"#;
    let ended = recipe("- **A shell command's end, with marks**")?.replace("%3", &pane(marks)?);
    let unmarked = recipe("- **A shell command's end, without marks:**")?
        .replace("%3", &pane("exec sh -i")?)
        .replace("make test", "true");
    let exit = recipe("- **A program's exit:**")?.replace(
        "str cargo program str test arg",
        "str sh program str -c arg str 'sleep 1; exit 3' arg",
    );
    let deaf = pane("stty -echo; sleep 30")?;
    let fallback = recipe("- **A deadline, a fallback, a retry:**")?
        .replace("%2", &deaf)
        .replace("10 timeout", "1 timeout");
    for (i, (recipe, said)) in [
        (quiet, "two\n"),
        (text, "answered\n"),
        (ended, "5\nhi\n"),
        (unmarked, "END-OF-RUN 0\n"),
        (exit, "3\n"),
        (fallback, ""),
    ]
    .into_iter()
    .enumerate()
    {
        let file = server.dir.join(format!("recipe-{i}.fux"));
        std::fs::write(&file, &recipe).map_err(e)?;
        let out = server.fux(&["--file", &file.to_string_lossy()])?;
        assert_eq!(out.status, 0, "{recipe}\n{}{}", out.stdout, out.stderr);
        assert!(out.stdout.contains(said), "{recipe}\n{}", out.stdout);
    }
    // C-c after nothing came of the key.
    server.exits(&deaf)?;
    let docs = include_str!("../docs/agents.md");
    for (i, program) in docs.split("```fux\n").skip(1).enumerate() {
        let program = program.split("```").next().unwrap_or_default();
        let file = server.dir.join(format!("shown-{i}.fux"));
        std::fs::write(&file, program).map_err(e)?;
        server.ok(&["str", &file.to_string_lossy(), "str", "program", "check"])?;
    }
    Ok(())
}

/// Keys to a program that closed its terminal but runs on are refused, as
/// to one that exited: nothing reads them, and `hung_up` says so. Linux
/// says it closed it (the terminal hangs up); macOS, where fux holds the
/// terminal open, does not.
#[cfg(target_os = "linux")]
#[test]
fn keys_to_a_program_that_closed_its_terminal_are_refused() -> Outcome {
    let server = Server::start("")?;
    let closes = "exec >/dev/null 2>&1 <&-; sleep 30";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", closes]))?);
    eventually("the keys refused", || {
        let keys = server.fux(&["str", &pane, "pane", "str", "x", "text", "send-keys"])?;
        Ok(keys.stderr.starts_with("fux: not_running: "))
    })?;
    let hung_up = server.ok(&["str", &pane, "pane", "str", "hung_up", "get"])?;
    assert_eq!(hung_up, "true\n");
    Ok(())
}
