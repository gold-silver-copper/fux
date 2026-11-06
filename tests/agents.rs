//! Driving programs from the command line, as a script or an agent does:
//! `new-tab` from the command line starts one out of every client's way,
//! and keeps its pane once it exits; an action acts at once and answers
//! its pane's version from before it acted; a wait answers once that version differs (its
//! program's end too) or the pane closes; `send-keys` and `send-mouse` send what a
//! person would, sizes are set, and a headless client stands in for a
//! terminal.
// They script `/bin/sh` over a Unix socket (WINDOWS.md, "What PR 8 found").
#![cfg(unix)]
mod support;
use std::time::{Duration, Instant};
use support::*;

/// The pane an answer is about: the first word of its first line.
fn id(out: &str) -> String {
    let first = out.split_whitespace().next().unwrap_or_default();
    first.split(':').next().unwrap_or_default().to_owned()
}

/// Construct a program request in grift; every executable argument stays data.
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
/// Each explicit option transform includes a size; the empty fixture uses 80 by 24.
fn launch(before: &[&str], argv: &[&str]) -> Vec<String> {
    assert!(
        before.len() <= 1,
        "fixture options must be one grift transform"
    );
    let builder = before.first().map_or_else(
        || "(size (in (options) \"agent\") 80 24)".to_owned(),
        |transform| format!("({transform} (in (options) \"agent\"))"),
    );
    vec![format!("(new-tab {})", program(&builder, argv))]
}
fn named(name: &str, before: &[&str], argv: &[&str]) -> Vec<String> {
    let creation = launch(before, argv).into_iter().next().unwrap_or_default();
    vec![format!(
        "(define! result {creation}) (rename result {}) result",
        fux::script::quoted(name)
    )]
}
fn typed(pane: &str, line: &str) -> Vec<String> {
    vec![format!(
        "(line (pane {}) {})",
        fux::script::quoted(pane),
        fux::script::quoted(line)
    )]
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
    let made = server.ok(&named(name, &[], &argv))?;
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

/// A program `new-tab` starts keeps its pane once it exits: its last screen,
/// and its status, which a wait and `ls` report (a signal's too); one that
/// cannot start is refused. It takes no more input. The program starts at
/// its `size`, in the `agent` workspace, which no client is moved to.
#[test]
fn a_program_keeps_its_pane_screen_and_status() -> Outcome {
    let server = Server::start("")?;
    let script = "stty size; echo started; exit 3";
    let started = server.ok(&launch(
        &["(lambda! (argument-2) (size argument-2 40 10))"],
        &["sh", "-c", script],
    ))?;
    let pane = id(&started);
    assert_eq!(server.exits(&pane)?, "3");
    // Waited on from its start, it has exited: answered at once; from now,
    // nothing changes.
    let since = version(&started);
    let from_start = [&format!(
        "(data-text (wait (changed {})))",
        fux::script::quoted(&since)
    )];
    let json = server.ok(&from_start)?;
    for (field, value) in [("exited", "3"), ("signal", "none"), ("status", "0")] {
        assert_eq!(
            read_data(&json, &format!("(field data (quote! {field}))"))?,
            value
        );
    }
    let now = server.fux(&[&format!(
        "(wait (timeout (pane {}) 1))",
        fux::script::quoted(&pane)
    )])?;
    assert_eq!(now.status, 124, "{}", now.stdout);
    let screen = server.ok(&[&format!("(capture (pane {}))", fux::script::quoted(&pane))])?;
    assert_eq!(
        screen, "10 40\nstarted\n",
        "trimmed, and sized from the start"
    );
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(ls.contains("+2 agent"), "{ls}");
    assert!(ls.contains(&format!("{pane} sh 40x10 exited 3")), "{ls}");
    let refused = server.fux(&[&format!(
        "(send-keys (text (pane {}) \"x\"))",
        fux::script::quoted(&pane)
    )])?;
    assert_eq!(refused.status, 1);
    assert!(
        refused.stderr.starts_with("fux: not-running: ") && refused.stderr.contains("exited 3"),
        "{}",
        refused.stderr
    );
    let missing = server.fux(&launch(&[], &["/no/such/program"]))?;
    assert_eq!(missing.status, 1);
    assert!(
        missing.stderr.contains("No such file"),
        "{}",
        missing.stderr
    );
    // One a signal ends is said to be killed by it.
    let started = server.ok(&launch(&[], &["sleep", "30"]))?;
    let pane = id(&started);
    server.ok(&[&format!(
        "(signal (pane {}) \"KILL\")",
        fux::script::quoted(&pane)
    )])?;
    server.exits(&pane)?;
    let since = version(&started);
    let waited = [&format!("(wait (changed {}))", fux::script::quoted(&since))];
    let text = server.ok(&waited)?;
    assert!(text.ends_with(" killed by SIGKILL\n"), "{text}");
    let data=server.ok(&[format!("(define! observed (wait (changed {}))) (display (list (list (quote! exited) (get observed \"exited\")) (list (quote! signal) (get observed \"signal\")) (list (quote! status) (get observed \"status\"))))",fux::script::quoted(&since))])?;
    assert_eq!(read_data(&data, "(field data (quote! exited))")?, "none");
    assert_eq!(read_data(&data, "(field data (quote! signal))")?, "9");
    assert_eq!(read_data(&data, "(field data (quote! status))")?, "0");
    Ok(())
}

/// An action acts at once and answers its pane's version from just before
/// it acted; a wait from that version answers once the version differs,
/// with what changed, however late it is asked: nothing between is
/// missed. A wait from now on a pane that does not change times out (124).
#[test]
fn a_wait_from_an_action_s_version_misses_nothing() -> Outcome {
    let server = Server::start("")?;
    let started = server.ok(&launch(
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
    let changed = server.ok(&[&format!("(wait (changed {}))", fux::script::quoted(&since))])?;
    assert!(
        changed.starts_with(&format!("{pane}:")) && changed.ends_with(" screen\n"),
        "{changed}"
    );
    let data = server.ok(&[&format!(
        "(data-text (wait (changed {})))",
        fux::script::quoted(&since)
    )])?;
    assert_eq!(read_data(&data, "(field data (quote! status))")?, "0");
    assert_eq!(
        read_data(&data, "(first (field data (quote! changed)))")?,
        "screen"
    );
    let still = server.fux(&[&format!(
        "(wait (timeout (pane {}) 1))",
        fux::script::quoted(&pane)
    )])?;
    assert_eq!(still.status, 124, "{}{}", still.stdout, still.stderr);
    assert!(
        still.stderr.contains("timeout:") && still.stderr.contains(&pane),
        "{}",
        still.stderr
    );
    let bad = server.fux(&["(wait (changed \"0.1.2.0.0\"))"])?;
    assert!(bad.stderr.starts_with("fux: bad-value: "), "{}", bad.stderr);
    // Counts this pane never had (a program started more often, or more
    // output read), or another server run's, are refused.
    let server_run = since.rsplit(':').next().unwrap_or_default();
    for other in [
        format!("{pane}:3.0.0.0.0:{server_run}"),
        format!("{pane}:0.99999.0.0.0:{server_run}"),
        format!("{pane}:0.1.0.0.0:1-0"),
    ] {
        let other = server.fux(&[&format!("(wait (changed {}))", fux::script::quoted(&other))])?;
        let refused = other.stderr.starts_with("fux: bad-value: ");
        assert!(refused, "{}", other.stderr);
    }
    // In one program, a wait on the action's answer waits from the version
    // it holds, a timeout given between them too: asked again once the
    // change surely came, it answers at once, where one from now would
    // time out.
    let echoes = id(&server.ok(&launch(&[], &["sh", "-c", "read x; sleep 30"]))?);
    let action = typed(&echoes, "x").into_iter().next().unwrap_or_default();
    let answer = server.ok(&[format!(
        "(define! initial {action}) (wait (timeout initial 5)) (wait (timeout initial 1))"
    )])?;
    assert!(answer.ends_with(" screen\n"), "{answer}");
    Ok(())
}

/// A version counts bells and notifications rung, and a wait says one
/// rang; a program started again is said so, its counts afresh.
#[test]
fn bells_and_respawns_change_the_version() -> Outcome {
    let server = Server::start("")?;
    let script = r"read x; printf '\a'; exec cat";
    let pane = id(&server.ok(&launch(&[], &["sh", "-c", script]))?);
    let mut at = version(&server.ok(&typed(&pane, ""))?);
    loop {
        let waited = [&format!(
            "(wait (timeout (changed {}) 10))",
            fux::script::quoted(&at)
        )];
        let out = server.ok(&waited)?;
        if out.contains(" bell") {
            break;
        }
        at = version(&out);
    }
    assert_eq!(count(&server.version(&pane)?, 2), Some("1"));
    let at = server.version(&pane)?;
    let respawned = server.ok(&[&format!(
        "(respawn (kill-first (pane {})))",
        fux::script::quoted(&pane)
    )])?;
    assert_eq!(count(&respawned, 0), Some("1"), "{respawned}");
    let answer = server.ok(&[&format!("(wait (changed {}))", fux::script::quoted(&at))])?;
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
    let status = |pane: &str| {
        server.ok(&[&format!("(sequence! (define! value-56 (pane {})) \"last_status\" (get value-56 \"last-status\"))", fux::script::quoted(pane))])
    };
    let check = |pane: &str| -> Outcome {
        line_done(&server, pane, "echo one; false")?;
        assert_eq!(status(pane)?, "1\n");
        let output = server.ok(&[&format!(
            "(capture (last-output (pane {})))",
            fux::script::quoted(pane)
        )])?;
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
        let last = |pane: &str| {
            server.ok(&[&format!(
                "(capture (last-output (pane {})))",
                fux::script::quoted(pane)
            )])
        };
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
            let running = |pane: &str| {
                server.ok(&[&format!("(sequence! (define! value-71 (pane {})) \"command_running\" (get value-71 \"command-running\"))", fux::script::quoted(pane))])
            };
            eventually("the echo's end", || {
                Ok(last(pane)? == echoed && running(pane)? == "#false\n")
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
        let home = format!("{home}={}", dir.to_string_lossy());
        server.ok(&named(shell, &[], &["env", &home, shell, "-i"]))?;
        check(shell)?;
    }
    let plain = server.ok(&launch(&[], &["sh"]))?;
    let refused = server.fux(&[&format!(
        "(capture (last-output (pane {})))",
        fux::script::quoted(&id(&plain))
    )])?;
    assert!(
        refused.stderr.starts_with("fux: no-marks: "),
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
    const READS: &str = r#"import os,tty
tty.setraw(0)
os.write(1,b"\x1b[?2004h\x1b[?1000h\x1b[?1006hready\r\n")
seen=b""
while not seen.endswith(b"q"):
    seen+=os.read(0,256)
    os.write(1,("seen "+seen.hex()+"\r\n").encode())
"#;
    let server = Server::start("")?;
    let path = server.dir.join("reads.py");
    std::fs::write(&path, READS).map_err(e)?;
    let started = server.ok(&launch(&[], &["python3", "-I", &path.to_string_lossy()]))?;
    let pane = id(&started);
    shows_after(&server, &started, "ready")?;
    let target = support::reference(&pane);
    let seen = std::cell::RefCell::new(String::new());
    let sent = |input: &str, read: &str| -> Outcome {
        seen.borrow_mut().push_str(read);
        let said = format!("seen {}\n", seen.borrow());
        shows_after(
            &server,
            &server.ok(&[&format!("(send-keys {input})")])?,
            &said,
        )?;
        Ok(())
    };
    sent(&format!("(key {target} \"Escape\")"), "1b")?;
    sent(&format!("(key (text {target} \"hi\") \"Enter\")"), "68690d")?;
    sent(
        &format!("(paste {target} \"a b\")"),
        "1b5b3230307e6120621b5b3230317e",
    )?;
    sent(&format!("(bytes {target} #bytes(195 169))"), "c3a9")?;
    sent(
        &format!("(text (key (key (key {target} \"BSpace\") \"BSpace\") \"BSpace\") \"x\")"),
        "7f7f7f78",
    )?;
    seen.borrow_mut()
        .push_str("1b5b3c303b353b334d1b5b3c303b353b336d");
    let said = format!("seen {}\n", seen.borrow());
    shows_after(
        &server,
        &server.ok(&[&format!("(send-mouse {target} \"click\" 2 4)")])?,
        &said,
    )?;
    server.ok(&[&format!("(send-keys (text {target} \"q\"))")])?;
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
    let made = server.words("(sequence! (define! value-9 (new-tab (size (in (options) (workspace \"+1\")) 100 20))) (list value-9 (tab value-9)))")?;
    let (left, tab) = made.trim().split_once('\n').ok_or("a pane and its tab")?;
    let left = id(left);
    let left = left.as_str();
    let size = |pane: &str| -> Result<String, String> {
        let fields = server.ok(&[&format!("(sequence! (define! value-87 (pane {})) \"cols\" (define! value-89 (get value-87 \"columns\")) \"rows\" (list value-89 (get value-87 \"rows\")))", fux::script::quoted(pane))])?;
        let (cols, rows) = fields.trim().split_once('\n').ok_or("pane dimensions")?;
        Ok(format!("{cols}x{rows}"))
    };
    assert_eq!(size(left)?, "100x20");
    server.ok(&[&format!(
        "(sequence! (resize (pane {}) 100 22) #inert)",
        fux::script::quoted(left)
    )])?;
    assert_eq!(size(left)?, "100x22", "a pane's tab");
    server.ok(&[&format!(
        "(sequence! (resize (tab {}) 100 20) #inert)",
        fux::script::quoted(tab)
    )])?;
    let right = id(&server.ok(&[&format!(
        "(split (axis (pane {}) \"horizontal\"))",
        fux::script::quoted(left)
    )])?);
    server.ok(&[&format!(
        "(sequence! (resize-pane (pane {}) (percent 25) \"horizontal\") #inert)",
        fux::script::quoted(&right)
    )])?;
    assert_eq!(size(&right)?, "25x20");
    assert_eq!(size(left)?, "74x20");
    let refused = server.fux(&[&format!(
        "(sequence! (resize-pane (pane {}) 100 \"horizontal\") #inert)",
        fux::script::quoted(left)
    )])?;
    assert!(
        refused.stderr.starts_with("fux: size: "),
        "{}",
        refused.stderr
    );
    assert_eq!(size(left)?, "74x20", "nothing changed");
    server.ok(&[&format!(
        "(sequence! (resize (tab {}) 40 10) #inert)",
        fux::script::quoted(tab)
    )])?;
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
        .args(["(sequence! (attach (size (options) 50 10)) #inert)"])
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
    assert!(
        server
            .ok(&["(listing (list-session))"])?
            .contains("client c1 50x10")
    );
    server.ok(&["(sequence! (resize (client \"c1\") 60 12) #inert)"])?;
    assert!(
        server
            .ok(&["(listing (list-session))"])?
            .contains("client c1 60x12")
    );
    let capture = ["(capture (client \"c1\"))"];
    assert_eq!(
        server.ok(&["(kind (capture (client \"c1\")))"])?.trim(),
        "client-capture"
    );
    eventually("a paint", || Ok(server.ok(&capture)?.contains('$')))?;
    // Keys typed at it are a person's: the prefix opens the column.
    server.ok(&["(send-keys (key (client \"c1\") \"C-b\"))"])?;
    eventually("the column", || {
        Ok(server.ok(&capture)?.contains("split side by side"))
    })?;
    server.words("(sequence! (send-keys (key (client \"c1\") \"Escape\")) #inert)")?;
    let mut terminal = server.attach(10, 40)?;
    terminal.ready()?;
    let refused = server.fux(&["(sequence! (resize (client \"c2\") 60 12) #inert)"])?;
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
    let enter = ["(send-keys (key (pane \"%1\") \"Enter\"))"];
    shows_after(&server, &server.ok(&enter)?, "drawn")?;
    eventually("the picture sent", || {
        client.pump()?;
        Ok(painted(&client, b"\x1bP0;1;0q\"1;1;20;12"))
    })?;
    let data = server.ok(&["(datum (capture (pane 1)))"])?;
    for (field, value) in [
        ("column", "0"),
        ("rows", "1"),
        ("cropped", "0"),
        ("columns", "2"),
        ("width", "20"),
        ("height", "12"),
    ] {
        assert_eq!(
            read_data(
                &data,
                &format!("(field (first (field data (quote! pictures))) (quote! {field}))")
            )?,
            value
        );
    }
    // A placement change clears the screen, so unchanged pictures must be sent again.
    client.painted.clear();
    let mut presentation = fux::script::default_state()
        .map_err(e)?
        .preferences
        .presentation();
    presentation.cell_placement = true;
    client.act(fux_wire::act::Act::Presentation(presentation))?;
    eventually("the picture resent after placement changes", || {
        client.pump()?;
        Ok(painted(&client, b"\x1bP0;1;0q\"1;1;20;12"))
    })?;
    Ok(())
}

/// `respawn` starts a pane's program again in it, found by its given
/// name; `all-history` reads all history,
/// `main-screen` what a full-screen program hides, `alt-screen` its last
/// screen; a mistyped key name is refused.
#[test]
fn programs_are_restarted_and_read_whole() -> Outcome {
    let server = Server::start("")?;
    let script = "echo before; seq 500; printf '\\033[?1049h'; echo \"full $V\"; read go";
    let before = ["(lambda! (builder) (size builder 40 10))"];
    let started = server.ok(&named("job", &before, &["env", "V=a", "sh", "-c", script]))?;
    let pane = id(&started);
    shows_after(&server, &started, "full a")?;
    let main = server.ok(&[format!(
        "(capture (all-history (main-screen (pane {}))))",
        fux::script::quoted(&pane)
    )])?;
    assert!(main.starts_with("before\n1\n"), "{main}");
    assert!(main.contains("\n500\n"), "{main}");
    let refused = server.fux(&[&format!(
        "(send-keys (key (pane {}) \"Entr\"))",
        fux::script::quoted(&pane)
    )])?;
    assert_eq!(refused.status, 1);
    assert!(refused.stderr.contains("Entr"), "{}", refused.stderr);
    let busy = server.fux(&[&format!("(respawn (pane {}))", fux::script::quoted(&pane))])?;
    assert_eq!(busy.status, 1, "it runs: {}", busy.stderr);
    let again = server.ok(&[format!(
        "(respawn (kill-first {}))",
        program("(pane \"job\")", &["env", "V=b", "sh", "-c", script])
    )])?;
    assert!(again.starts_with(&format!("{pane}:1.")), "{again}");
    shows_after(&server, &again, "full b")?;
    // A full-screen program's last screen, after it left it and exited.
    let menu = r"printf '\033[?1049hpicked beta'; printf '\033[?1049l'";
    let menu = id(&server.ok(&launch(&[], &["sh", "-c", menu]))?);
    server.exits(&menu)?;
    assert!(
        !server
            .ok(&[&format!("(capture (pane {}))", fux::script::quoted(&menu))])?
            .contains("picked")
    );
    assert_eq!(
        server.ok(&[&format!(
            "(capture (alternate-screen (pane {})))",
            fux::script::quoted(&menu)
        )])?,
        "picked beta\n"
    );
    Ok(())
}

/// Pane and tab names are independent, id spellings are refused, and a
/// click outside a pane or a signal to its default shell is refused.
#[test]
fn names_clicks_and_signals_are_checked() -> Outcome {
    let server = Server::start("")?;
    server.ok(&named("ed", &[], &["cat"]))?;
    assert_eq!(
        server
            .fux(&launch(
                &["(lambda! (argument-5) (name (size argument-5 80 24) \"@1\"))"],
                &["cat"]
            ))?
            .status,
        1
    );
    // A name is one thing's: the pane has it, so no tab may.
    let tab = ["(new-tab (name (in (size (options) 80 24) \"+1\") \"ed\"))"];
    let taken = server.fux(&tab)?;
    assert!(
        taken.stderr.starts_with("fux: name-taken: "),
        "{}",
        taken.stderr
    );
    assert_eq!(server.ok(&["(pane \"ed\")"])?, "%2\n");
    // A common spelling is a key's name too; an unknown name is refused with the vocabulary hint.
    server.ok(&["(send-keys (key (pane \"ed\") \"Backspace\"))"])?;
    let typo = server.fux(&["(send-keys (key (pane \"ed\") \"C-Bakspace\"))"])?;
    assert!(
        typo.stderr
            .contains("is no key's name (key-names lists them)"),
        "{}",
        typo.stderr
    );
    server.ok(&["(sequence! (resize (tab (pane \"ed\")) 40 12) #inert)"])?;
    assert!(
        server.ok(&["(listing (list-session))"])?.contains(" 40x12"),
        "resized by the named pane's tab"
    );
    let clicks = "printf '\\033[?1000h\\033[?1006hready'; exec cat";
    shows_after(
        &server,
        &server.ok(&named(
            "m",
            &["(lambda! (argument-10) (size argument-10 20 5))"],
            &["sh", "-c", clicks],
        ))?,
        "ready",
    )?;
    let outside = ["(send-mouse (pane \"m\") \"click\" 5 0)"];
    let outside = server.fux(&outside)?;
    assert!(
        outside.stderr.starts_with("fux: mouse: "),
        "{}",
        outside.stderr
    );
    let signalled = server.fux(&["(signal (pane \"%1\") \"HUP\")"])?;
    assert!(
        signalled.stderr.starts_with("fux: only-shell: "),
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
    let started = server.ok(&launch(
        &["(lambda! (argument-13) (size argument-13 20 6))"],
        &["sh", "-c", long],
    ))?;
    let pane = id(&started);
    shows_after(&server, &started, "next")?;
    let line = server.ok(&[&format!(
        "(capture (rows (pane {}) 1 2))",
        fux::script::quoted(&pane)
    )])?;
    assert_eq!(line, "aaaaaaaaaaaaaaaaaaaaBBBBBBBBBBBBBBBBBBBBcc\n");
    let plain = server.ok(&[&format!("(capture (pane {}))", fux::script::quoted(&pane))])?;
    assert!(
        plain.contains("aaaaaaaaaaaaaaaaaaaaBBBBBBBBBBBBBBBBBBBBcc\nnext"),
        "{plain}"
    );
    let rows = server.ok(&[&format!(
        "(capture (wrapped (pane {})))",
        fux::script::quoted(&pane)
    )])?;
    assert!(
        rows.contains("aaaaaaaaaaaaaaaaaaaa\nBBBBBBBBBBBBBBBBBBBB\ncc\n"),
        "{rows}"
    );
    // A column range cuts a wide glyph to a blank, a column a cell.
    let wide = "printf 'A界BC              Z'; exec cat";
    let wide = server.ok(&launch(
        &["(lambda! (argument-16) (size argument-16 20 6))"],
        &["sh", "-c", wide],
    ))?;
    shows_after(&server, &wide, "Z")?;
    let wide = id(&wide);
    let cut = |first: &str, last: &str| {
        server.ok(&[&format!(
            "(capture (columns (rows (pane {}) 0 1) {} {}))",
            fux::script::quoted(&wide),
            support::reference(first),
            support::reference(last)
        )])
    };
    assert_eq!(cut("2", "5")?, " BC\n");
    assert_eq!(cut("0", "2")?, "A\n");
    assert_eq!(cut("19", "20")?, "Z\n");
    assert_eq!(cut("19", "25")?, "Z\n");
    assert_eq!(cut("2", "2")?, "");
    // The cursor is the screen's: on the empty row after the last line;
    // and every row kept was read.
    let data = server.ok(&[&format!("(datum (capture {}))", support::reference(&pane))])?;
    assert_eq!(
        read_data(&data, "(field data (quote! screen-cursor))")?,
        "(4 0)"
    );
    assert_eq!(read_data(&data, "(field data (quote! above))")?, "0");
    Ok(())
}

/// A wait on a workspace waits on each of its panes, and answers a line for
/// each: here none changed, so each timed out, the whole 124. (`session`'s
/// tests begin a wait before what it waits for: the answer of the pane
/// that changed, and of one that closed.)
#[test]
fn a_wait_on_a_workspace_answers_each_of_its_panes() -> Outcome {
    let server = Server::start("")?;
    let one = id(&server.ok(&launch(&[], &["sleep", "30"]))?);
    let two = id(&server.ok(&launch(&[], &["sleep", "30"]))?);
    let out = server.fux(&[r#"(wait (timeout (workspace "agent") 1))"#])?;
    assert_eq!(out.status, 124);
    assert!(out.stdout.is_empty());
    for pane in [&one, &two] {
        assert!(
            out.stderr.contains(&format!("{pane}: unchanged for 1s")),
            "{}",
            out.stderr
        );
    }
    assert_eq!(
        out.stderr.matches("(status 124)").count(),
        2,
        "{}",
        out.stderr
    );
    Ok(())
}

/// An action's answer, and a wait's, is its pane: a word that wants a pane
/// takes it, never a pane named by what the program wrote (`%13 fake`).
#[test]
fn an_answer_selects_its_pane_explicitly_for_live_fields() -> Outcome {
    let server = Server::start("")?;
    let script = "read x; echo '%13 fake'; exec cat";
    let pane = id(&server.ok(&launch(&[], &["sh", "-c", script]))?);
    let since = server.version(&pane)?;
    let action = typed(&pane, "go").into_iter().next().unwrap_or_default();
    assert_eq!(
        server
            .ok(&[format!("(get (pane {action}) \"name\")")])?
            .trim(),
        "none"
    );
    server.shows(&pane, "%13 fake")?;
    let waited = [&format!(
        "(sequence! (define! value-66 (pane (wait (changed {})))) \"name\" (get value-66 \"name\"))",
        fux::script::quoted(&since)
    )];
    assert_eq!(server.ok(&waited)?.trim(), "none");
    Ok(())
}

/// Input of any size goes in one request, in as many frames as it takes,
/// and arrives as one paste: its start marker, all of the text, its end.
#[test]
fn send_keys_sends_input_of_any_size_as_one_paste() -> Outcome {
    let server = Server::start("")?;
    let line: String = std::iter::repeat_n('a', 99).chain(['\n']).collect();
    let text: String = std::iter::repeat_n(line, 25_600).collect();
    let crs = text.replace('\n', "\r");
    let pasted = [&b"\x1b[200~"[..], crs.as_bytes(), b"\x1b[201~"].concat();
    let got = server.dir.join("got");
    let take = format!(
        "stty raw -echo; printf '\\033[?2004hready'; head -c {} > '{}'; printf done",
        pasted.len(),
        got.display()
    );
    let sink = id(&server.ok(&launch(&[], &["sh", "-c", &take]))?);
    server.shows(&sink, "ready")?;
    let sent = server.piped(
        std::path::Path::new(fux()),
        &[&format!(
            "(send-keys (paste {} (input)))",
            support::reference(&sink)
        )],
        &text,
    )?;
    assert_eq!(sent.status, 0, "{}", sent.stderr);
    server.shows(&sink, "done")?;
    assert!(
        std::fs::read(&got).map_err(e)? == pasted,
        "not one whole paste"
    );
    Ok(())
}

/// `json` on a saved capture converts what was captured: never a later
/// read of the pane, which may have moved on or closed.
#[test]
fn json_of_a_saved_capture_is_that_capture() -> Outcome {
    let server = Server::start("")?;
    let script = "echo BEFORE; read x; echo LA\"\"TER; read y";
    let started = server.ok(&launch(&[], &["sh", "-c", script]))?;
    let pane = id(&started);
    shows_after(&server, &started, "BEFORE")?;
    let json = server.ok(&[&format!("(sequence! (define! variables-221 (context)) (bind variables-221 (quote! P) (pane {})) (bind variables-221 (quote! C) (capture (evaluate (quote! P) variables-221))) (send-keys (key (evaluate (quote! P) variables-221) \"Enter\")) (kill (evaluate (quote! P) variables-221)) (evaluate (quote! C) variables-221))", fux::script::quoted(&pane))])?;
    assert!(json.contains("BEFORE") && !json.contains("LATER"), "{json}");
    Ok(())
}

/// In a list, an answer is as `json` gives it alone: a capture, another,
/// and a listing are their objects.
#[test]
fn an_answer_in_a_list_is_its_object() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&launch(&[], &["sh", "-c", "read x"]))?);
    let p = support::reference(&pane);
    let data = server.ok(&[&format!(
        "(data-text (list (capture {p}) (capture {p}) (list-session)))"
    )])?;
    assert_eq!(read_data(&data, "(length data)")?, "3");
    assert_eq!(read_data(&data, "(field (first data) (quote! id))")?, pane);
    assert_eq!(read_data(&data, "(field (second data) (quote! id))")?, pane);
    assert_eq!(
        read_data(&data, "(field (element-at data 2) (quote! projection))")?,
        "facts"
    );
    assert!(!read_data(&data, "(field (element-at data 2) (quote! run))")?.is_empty());
    Ok(())
}

/// A word refuses options it would not use, and makes nothing: a capture
/// waits for nothing, a wait starts and views nothing, a split takes its
/// tab's place, a new tab splits nothing, a respawn keeps
/// its pane's name, size, place and ids.
#[test]
fn words_refuse_options_they_do_not_use() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&launch(&[], &["sh", "-c", "read x"]))?);
    let p = support::reference(&pane);
    let since = fux::script::quoted(server.version(&pane)?.trim());
    let cases = [
        (format!("(first (list (changed {since})))"), "capture"),
        (format!("(timeout {p} 5)"), "capture"),
        (format!("(size {p} 100 40)"), "capture"),
        (format!("(name {p} \"x\")"), "wait"),
        (format!("(history {p} 10)"), "wait"),
        (format!("(in {p} \"nowhere\")"), "split"),
        (format!("(in {p} \"nowhere\")"), "respawn"),
        (format!("(name {p} \"x\")"), "respawn"),
        (format!("(size {p} 100 30)"), "respawn"),
        (format!("(directory {p} \"/\")"), "respawn"),
        (format!("(axis {p} \"vertical\")"), "respawn"),
        ("(timeout (options) 5)".to_owned(), "new-tab"),
    ];
    for (builder, operation) in cases {
        let out = server.fux(&[&format!("({operation} {builder})")])?;
        assert_eq!(out.status, 1, "{builder} {operation}: {}", out.stderr);
        assert!(out.stderr.starts_with("fux: unused: "), "{}", out.stderr);
    }
    for builder in [
        r#"(axis (options) "horizontal")"#,
        r#"(axis (options) "vertical")"#,
        "(kill-first (options))",
        "(full (options))",
        "(alternate-screen (options))",
        "(last-output (options))",
        "(wrapped (options))",
        "(rows (options) 0 5)",
    ] {
        let out = server.fux(&[&format!("(new-tab (program {builder} \"cat\"))")])?;
        assert_eq!(out.status, 1, "{}", out.stderr);
        assert!(out.stderr.starts_with("fux: unused: "), "{}", out.stderr);
    }
    assert!(
        !server
            .ok(&["(listing (list-session))"])?
            .contains("nowhere")
    );
    assert_eq!(server.panes()?.len(), 2);
    Ok(())
}

/// A failed wait prints nothing, whatever follows it: stderr says why on
/// one line, then its answer as text; nothing after it runs.
#[test]
fn a_failed_wait_answers_as_text_on_stderr() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&launch(&[], &["sleep", "30"]))?);
    let p = support::reference(&pane);
    for program in [
        format!("(data-text (wait (timeout {p} 1)))"),
        format!("(define! w (lambda! () (wait (timeout {p} 1)))) (data-text (w))"),
    ] {
        let out = server.fux(&[&program])?;
        assert_eq!(out.status, 124, "{}", out.stderr);
        assert!(out.stdout.is_empty());
        assert!(
            out.stderr.starts_with("fux: timeout:")
                && out.stderr.contains(&pane)
                && out.stderr.contains("(status 124)"),
            "{}",
            out.stderr
        );
    }
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
    let output = server.ok(&["(capture (last-output (pane \"r\")))"])?;
    assert_eq!(output, "CURRENT\n");
    line_done(&server, "r", "")?;
    let draw = "printf 'before\\n\\033[?1049h\\033[Habc\\ndef\\nxy%s\\033[2;2H' z; read x";
    shows_after(&server, &server.ok(&typed("r", draw))?, "xyz")?;
    let data = server.ok(&["(datum (capture (last-output (pane \"r\"))))"])?;
    assert_eq!(
        read_data(&data, "(field data (quote! screen-cursor))")?,
        "(1 1)"
    );
    Ok(())
}

/// A given name matching the old program survives respawn. A wait
/// from the ended program's version tells the new one's end too.
#[test]
fn respawn_keeps_a_given_name_matching_the_old_program() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&named("sh", &[], &["sh", "-c", "exit 0"]))?);
    server.exits(&pane)?;
    let ended = server.version(&pane)?;
    server.ok(&[&format!(
        "(respawn (program (pane {}) \"true\"))",
        fux::script::quoted(&pane)
    )])?;
    let name = server.ok(&[&format!(
        "(sequence! (define! value-254 (pane {})) \"name\" (get value-254 \"name\"))",
        fux::script::quoted(&pane)
    )])?;
    assert_eq!(name, "sh\n");
    server.exits(&pane)?;
    let answer = server.ok(&[&format!("(wait (changed {}))", fux::script::quoted(&ended))])?;
    assert!(answer.ends_with(" respawned exited 0\n"), "{answer}");
    Ok(())
}

/// A pane's `FUX` is the fux running it, so a key's program reaches fux
/// with no `fux` on PATH.
#[test]
fn a_pane_gets_the_fux_running_it() -> Outcome {
    let server = Server::start("")?;
    let want = format!("WANT={}", fux());
    let script = "PATH=/nonexistent; \"$FUX\" here capture >/dev/null && [ \"$FUX\" -ef \"$WANT\" ] && echo same";
    let pane = id(&server.ok(&launch(&[], &["env", &want, "/bin/sh", "-c", script]))?);
    server.exits(&pane)?;
    let capture = server.ok(&[&format!("(capture (pane {}))", fux::script::quoted(&pane))])?;
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
    let cases = [
        (
            "### Quiet after a request",
            "read x; echo one; sleep 0.3; echo two; sleep 30",
            "two\n",
        ),
        (
            "### Wait for a reply",
            "read q; sleep 1; echo the answer is 42; sleep 30",
            "answered\n",
        ),
        (
            "### Read a marked command",
            r#"read x; printf '\033]133;C\007hi\n\033]133;D;5\007'; sleep 30"#,
            "5\nhi\n",
        ),
        (
            "### Read an unmarked command",
            "exec sh -i",
            "END-OF-RUN 0\n",
        ),
        ("### Read a program exit", "sleep 1; exit 3", "3\n"),
        ("### Fall back after a deadline", "stty -echo; sleep 30", ""),
    ];
    for (i, (heading, script, said)) in cases.into_iter().enumerate() {
        let pane = id(&server.ok(&launch(&[], &["sh", "-c", script]))?);
        let program = block(heading, "grift")?;
        let file = server.dir.join(format!("recipe-{i}.grift"));
        std::fs::write(&file, &program).map_err(e)?;
        let out = server.fux(&["--file", &file.to_string_lossy(), &pane])?;
        assert_eq!(out.status, 0, "{heading}: {}{}", out.stdout, out.stderr);
        assert!(out.stdout.contains(said), "{heading}: {}", out.stdout);
        if i == 5 {
            server.exits(&pane)?;
        }
    }
    for (i, program) in include_str!("../docs/agents.md")
        .split("```grift\n")
        .skip(1)
        .enumerate()
    {
        let program = program.split("```").next().unwrap_or_default();
        let mut g: grift::Grift = grift::Grift::new().map_err(|_| "grift initialization")?;
        g.read_in(&format!("guide-{i}"), program)
            .map_err(|_| "guide program does not read")?;
    }
    Ok(())
}

/// Keys to a program that closed its terminal but runs on are refused, as
/// to one that exited: nothing reads them, and `hung_up` says so, as the
/// journal's one `hung_up` told. Linux says it closed it (the terminal
/// hangs up); macOS, where fux holds the terminal open, does not.
#[cfg(target_os = "linux")]
#[test]
fn keys_to_a_program_that_closed_its_terminal_are_refused() -> Outcome {
    let server = Server::start("")?;
    let cursor = server.ok(&[r#"(get (next-events (all) "" 0) "next")"#])?;
    let pane = id(&server.ok(&launch(
        &[],
        &["sh", "-c", "exec >/dev/null 2>&1 <&-; sleep 30"],
    ))?);
    let p = support::reference(&pane);
    eventually("the keys refused", || {
        Ok(server
            .fux(&[&format!("(send-keys (text {p} \"x\"))")])?
            .stderr
            .starts_with("fux: not-running: "))
    })?;
    assert_eq!(
        server
            .ok(&[&format!("(get (get {p} \"program\") \"state\")")])?
            .trim(),
        "hung_up"
    );
    let events=server.ok(&[&format!("(define! batch (next-events {p} {} 0)) (data-text (filter (lambda! (e) (same? (get e \"event\") \"hung-up\")) (get batch \"events\")))",fux::script::quoted(cursor.trim()))])?;
    assert_eq!(read_data(&events, "(length data)")?, "1");
    assert_eq!(
        read_data(
            &events,
            "(field (field (first data) (quote! origin)) (quote! kind))"
        )?,
        "program"
    );
    Ok(())
}

/// A request's input and signal are journaled when they are accepted: the
/// pane, its program's generation, how the input came (`keys`, `paste`,
/// `mouse`), its bytes and the version `expect` admitted it at, or the
/// signal; never what the input says.
#[test]
fn requests_journal_their_input_and_signals_never_the_text() -> Outcome {
    let server = Server::start("")?;
    let cursor = server.ok(&["(sequence! (define! value-285 (next-events (all) \"\" 0)) \"next\" (get value-285 \"next\"))"])?;
    let clicks = "printf '\\033[?1000h\\033[?1006hready'; exec cat";
    let started = server.ok(&named(
        "m",
        &["(lambda! (argument-19) (size argument-19 20 5))"],
        &["sh", "-c", clicks],
    ))?;
    shows_after(&server, &started, "ready")?;
    let seen = server.ok(&["(version (capture (pane \"m\")))"])?;
    let seen = seen.trim();
    server.ok(&[&format!(
        "(send-keys (text (expect {}) \"secret\"))",
        fux::script::quoted(seen)
    )])?;
    server.ok(&["(send-keys (paste (pane \"m\") \"secret\"))"])?;
    server.ok(&["(send-mouse (pane \"m\") \"click\" 0 0)"])?;
    server.ok(&["(signal (pane \"m\") \"INT\")"])?;
    let events=server.ok(&[&format!("(define! batch (next-events (pane \"m\") {} 0)) (data-text (filter (lambda! (e) (member? (get e \"event\") (quote! (\"input\" \"signalled\")))) (get batch \"events\")))",fux::script::quoted(cursor.trim()))])?;
    assert_eq!(read_data(&events, "(length data)")?, "4");
    let pane = id(&started);
    for i in 0..4 {
        for (field, value) in [
            ("pane", pane.as_str()),
            ("tab", "@2"),
            ("workspace", "+2"),
            ("generation", "0"),
        ] {
            assert_eq!(
                read_data(
                    &events,
                    &format!("(field (element-at data {i}) (quote! {field}))")
                )?,
                value
            );
        }
    }
    for (i, kind, bytes) in [(0, "keys", "6"), (1, "paste", "6"), (2, "mouse", "18")] {
        assert_eq!(
            read_data(
                &events,
                &format!("(field (element-at data {i}) (quote! kind))")
            )?,
            kind
        );
        assert_eq!(
            read_data(
                &events,
                &format!("(field (element-at data {i}) (quote! bytes))")
            )?,
            bytes
        );
    }
    assert_eq!(
        read_data(&events, "(field (first data) (quote! expected))")?,
        seen
    );
    assert_eq!(
        read_data(&events, "(field (element-at data 3) (quote! signal))")?,
        "2"
    );
    assert!(!events.contains("secret"), "{events}");
    Ok(())
}

/// Every promised input kind reaches the condition, and the matched capture
/// keeps its text and version for local reads and guarded input.
#[test]
fn conditions_accept_panes_answers_and_pane_captures() -> Outcome {
    let server = Server::start("")?;
    let started = server.ok(&launch(
        &[],
        &["sh", "-c", "echo BOOT; sleep 0.1; echo READY; read x"],
    ))?;
    let pane = id(&started);
    let p = support::reference(&pane);
    for source in [
        p.clone(),
        format!("(send-keys (text {p} \"\"))"),
        format!("(capture {p})"),
    ] {
        for condition in [
            format!("(shows {source} \"READY\" 3)"),
            format!("(quiet {source} 1)"),
        ] {
            let program =
                format!("(define! seen {condition}) (print (string seen)) (version seen)");
            let out = server.ok(&[&program])?;
            assert!(
                out.contains("READY\n") && out.contains(&format!("{pane}:")),
                "{program}: {out}"
            );
        }
    }
    Ok(())
}

/// A held capture is a pure observation, even after its pane is closed.
#[test]
fn observed_retains_a_capture_without_refreshing_it() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&launch(&[], &["sh", "-c", "echo READY; read x"]))?);
    let code = format!(
        "(sequence! (define! variables-56 (context)) (bind variables-56 (quote! seen) (shows {} \"READY\" 3)) (kill {}) (string (shows (observed (evaluate (quote! seen) variables-56)) \"READY\" 1)))",
        support::reference(&format!("{pane}")),
        support::reference(&format!("{pane}"))
    );
    assert_eq!(server.ok(&[&code])?, "READY\n");
    Ok(())
}

/// An ended observation satisfies until-ended immediately on every supported
/// input kind; its status survives another call to the condition.
#[test]
fn until_ended_checks_before_waiting_and_keeps_status() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&launch(&[], &["sh", "-c", "exit 7"]))?);
    server.exits(&pane)?;
    for source in [
        pane.clone(),
        format!("(capture {})", support::reference(&format!("{pane}"))),
    ] {
        let code = format!(
            "(sequence! (define! value-69 (until-ended (until-ended {} 1) 1)) \"program\" (define! value-71 (get value-69 \"program\")) \"exited\" (get value-71 \"exited\"))",
            support::reference(&format!("{source}"))
        );
        assert_eq!(server.ok(&[&code])?, "7\n");
    }
    let code = "(sequence! (define! value-42 (until-ended (wait (timeout (new-tab (argument (argument (program (size (in (options) \"agent\") 80 24) \"sh\") \"-c\") \"sleep 0.1; exit 7\")) 1)) 1)) \"program\" (define! value-44 (get value-42 \"program\")) \"exited\" (get value-44 \"exited\"))";
    assert_eq!(server.ok(&[code])?, "7\n");
    Ok(())
}

/// Growth validates its counter, arithmetic and generation before comparing
/// counters from separate invocations. Zero growth accepts every baseline kind.
#[test]
fn grows_validates_its_baseline_and_requested_growth() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&launch(
        &[],
        &["sh", "-c", "read x; printf '\\007'; read y"],
    ))?);
    for source in [
        pane.clone(),
        format!("(capture {})", support::reference(&format!("{pane}"))),
        format!(
            "(send-keys (text {} \"\"))",
            support::reference(&format!("{pane}"))
        ),
    ] {
        server.ok(&[&format!(
            "(sequence! (grows {} \"bells\" 0 1) #inert)",
            support::reference(&format!("{source}"))
        )])?;
    }
    for (count, amount) in [("respawns", 0), ("nosuch", 0), ("bells", -1)] {
        let refused = server.fux(&[&format!(
            "(sequence! (grows (capture {}) {} {} 1) #inert)",
            support::reference(&format!("{pane}")),
            fux::script::quoted(&format!("{count}")),
            support::reference(&format!("{amount}"))
        )])?;
        assert_eq!(refused.status, 1, "{}", refused.stdout);
    }
    let bell = format!(
        "(sequence! (define! value-91 (grows (line {} \"go\") \"bells\" 1 3)) \"bells\" (get value-91 \"bells\"))",
        support::reference(&format!("{pane}"))
    );
    assert_eq!(server.ok(&[&bell])?, "1\n");
    let overflow = server.fux(&[&format!(
        "(grows (capture {}) \"bells\" {} 1)",
        support::reference(&pane),
        i64::MAX
    )])?;
    assert_eq!(overflow.status, 1);
    let generation = format!(
        "(sequence! (define! variables-94 (context)) (bind variables-94 (quote! seen) (capture {})) (respawn (kill-first {})) (grows (evaluate (quote! seen) variables-94) \"screen\" 1 1) #inert)",
        support::reference(&format!("{pane}")),
        support::reference(&format!("{pane}"))
    );
    let refused = server.fux(&[&generation])?;
    assert!(
        refused.stderr.contains("program respawned"),
        "{}",
        refused.stderr
    );
    Ok(())
}

/// Quiet catches only unchanged timeouts; losing the pane must reach the caller.
#[test]
fn quiet_propagates_closure_instead_of_reporting_stability() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&launch(&[], &["sh", "-c", "echo READY; read x"]))?);
    let code = format!(
        "(sequence! (define! variables-101 (context)) (bind variables-101 (quote! seen) (shows {} \"READY\" 3)) (kill {}) (quiet (evaluate (quote! seen) variables-101) 1) #inert)",
        support::reference(&format!("{pane}")),
        support::reference(&format!("{pane}"))
    );
    let closed = server.fux(&[&code])?;
    assert_eq!(closed.status, 4, "{}", closed.stderr);
    Ok(())
}
