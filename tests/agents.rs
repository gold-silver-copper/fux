//! Driving programs from the command line, as a script or an agent does:
//! `run` starts one out of every client's way and keeps its pane once it
//! exits; an action acts and answers at once; a wait answers once what it
//! names happens after it began (a text written, any output, quiet, an
//! exit, a bell, a shell command's end); `send-keys` and `send-mouse` send
//! what a person would, sizes are set, and a headless client stands in for
//! a terminal.
mod support;
use std::time::{Duration, Instant};
use support::*;

/// The pane an answer is about: the first word of its first line.
fn id(out: &str) -> String {
    out.split_whitespace().next().unwrap_or_default().to_owned()
}

/// An answer's first line: the pane and what happened.
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

/// `send-keys` to `pane` typing `line`, then Enter, after `wait` (words).
fn typed<'a>(pane: &'a str, line: &'a str, wait: &[&'a str]) -> Vec<&'a str> {
    [
        &["str", pane, "pane"][..],
        wait,
        &["str", line, "text", "str", "Enter", "key", "send-keys"],
    ]
    .concat()
}

/// The marks the README shows for bash, which a bash pane here sends.
const BASH_MARKS: &str = r#"PS0='\e]133;C\a'
PROMPT_COMMAND='printf "\e]133;D;%s\a\e]133;A\a" "$?"'${PROMPT_COMMAND:+;$PROMPT_COMMAND}"#;

/// The marks the README shows for zsh.
const ZSH_MARKS: &str = r#"autoload -Uz add-zsh-hook
mark_start() { print -n '\e]133;C\a' }
mark_end() { print -n "\e]133;D;$?\a\e]133;A\a" }
add-zsh-hook preexec mark_start
add-zsh-hook precmd mark_end"#;

/// The marks the README shows for fish.
const FISH_MARKS: &str = r#"function mark_start --on-event fish_preexec; printf '\e]133;C\a'; end
function mark_end --on-event fish_postexec; printf '\e]133;D;%s\a' $status; end
function mark_prompt --on-event fish_prompt; printf '\e]133;A\a'; end"#;

/// A bash pane named `name` that marks its commands as the README says.
fn marked_bash(server: &Server, name: &str) -> Result<(), String> {
    let rc = server.dir.join(format!("{name}.bashrc"));
    std::fs::write(&rc, format!("PS1='$ '\n{BASH_MARKS}\n")).map_err(e)?;
    let rc = rc.to_string_lossy().into_owned();
    let argv = ["bash", "--noprofile", "--rcfile", &rc, "-i"];
    server.ok(&run(
        &["str", name, "name", "str", "$ ", "wait-text"],
        &argv,
    ))?;
    Ok(())
}

/// A program `run` starts keeps its pane once it exits: its last screen,
/// and its status, which `wait-exit` and `ls` report; one that cannot
/// start is refused. It takes no more input. The program starts at its
/// `size`, in the `agent` workspace, which no client is moved to.
#[test]
fn a_run_program_keeps_its_pane_screen_and_status() -> Outcome {
    let server = Server::start("")?;
    let script = "stty size; echo started; exit 3";
    let started = server.ok(&run(
        &["40", "10", "size", "wait-exit"],
        &["sh", "-c", script],
    ))?;
    let pane = id(&started);
    assert_eq!(started, format!("{pane} exited 3\n"));
    // Waited for again, the exit has come already: met at once, status 0.
    let json = server.ok(&["str", &pane, "pane", "wait-exit", "wait", "json"])?;
    let met = format!(r#"{{"pane":"{pane}","met":"exit","exited":3,"status":0}}"#);
    assert_eq!(json.trim_end(), met);
    assert_eq!(
        server.ok(&["str", &pane, "pane", "str", "exited", "get"])?,
        "3\n"
    );
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
    // Given no wait words, run answers at once with its pane.
    let at_once = server.ok(&run(&[], &["sleep", "30"]))?;
    assert!(
        at_once.starts_with('%') && at_once.lines().count() == 1,
        "{at_once}"
    );
    let missing = server.fux(&run(&[], &["/no/such/program"]))?;
    assert_eq!(missing.status, 1);
    assert!(
        missing.stderr.contains("No such file"),
        "{}",
        missing.stderr
    );
    Ok(())
}

/// Actions act and answer at once, with their pane, whatever the program
/// does next; a wait names what it waits for: a timeout alone, or nothing,
/// is refused.
#[test]
fn actions_answer_at_once_and_waits_name_what_they_wait_for() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&[], &["sh", "-c", "sleep 30"]))?);
    let began = Instant::now();
    let sent = server.ok(&["str", &pane, "pane", "str", "x", "text", "send-keys"])?;
    assert_eq!(sent, format!("{pane}\n"));
    let json = server.ok(&[
        "str",
        &pane,
        "pane",
        "str",
        "y",
        "text",
        "send-keys",
        "json",
    ])?;
    assert_eq!(
        json.trim_end(),
        format!(r#"{{"pane":"{pane}","status":0}}"#)
    );
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    for words in [
        &["wait"][..],
        &["5", "timeout", "wait"],
        &["5", "timeout", "str", "x", "text", "send-keys"],
    ] {
        let refused = server.fux(&[&["str", pane.as_str(), "pane"][..], words].concat())?;
        assert!(
            refused.stderr.starts_with("fux: no_wait: "),
            "{words:?}: {}",
            refused.stderr
        );
    }
    Ok(())
}

/// `wait-text` meets a text the program writes after the wait began,
/// exactly, escape sequences aside, the echo of keys typed included, and
/// answers the line it was written on; a text written before is not met,
/// and the wait times out (124), saying so.
#[test]
fn a_text_is_met_once_written_after_the_wait_began() -> Outcome {
    let server = Server::start("")?;
    let script = r#"printf 'READY\n'; read x; printf 'got \033[1mBO\033[0mLD %s\n' "$x"; exec cat"#;
    let pane = id(&server.ok(&run(&["str", "READY", "wait-text"], &["sh", "-c", script]))?);
    let p = ["str", pane.as_str(), "pane"];
    // READY came before this wait: it waits to its limit.
    let late = server.fux(
        &[
            &p[..],
            &["1", "timeout", "str", "READY", "wait-text", "wait"],
        ]
        .concat(),
    )?;
    assert_eq!(late.status, 124, "{}{}", late.stdout, late.stderr);
    assert_eq!(late.stdout, format!("{pane} timeout\n"));
    assert!(
        late.stderr.starts_with("fux: timeout: ") && late.stderr.contains("timed out after 1s"),
        "{}",
        late.stderr
    );
    let got = server.ok(&typed(&pane, "x", &["str", "BOLD x", "wait-text"]))?;
    assert_eq!(got, format!("{pane} text \"BOLD x\"\ngot BOLD x\n"));
    // `cat` under the kernel's echo: the echo of keys typed is written too.
    let echo = server.ok(&[
        &p[..],
        &["str", "abc", "wait-text", "str", "abc", "text", "send-keys"],
    ]
    .concat())?;
    assert_eq!(head(&echo), format!("{pane} text \"abc\""));
    // Case and blanks as written.
    let exact = [
        "1",
        "timeout",
        "str",
        "ABC",
        "wait-text",
        "str",
        "d",
        "text",
        "send-keys",
    ];
    assert_eq!(server.fux(&[&p[..], &exact].concat())?.status, 124);
    Ok(())
}

/// `wait-change` is met by any output; `wait-quiet` once the program wrote
/// nothing for so long, counted from the wait's start or its last output;
/// `wait-exit` by its exit, and a wait for anything else fails once it
/// exits (3), saying how.
#[test]
fn change_quiet_and_exit_are_met_as_they_happen() -> Outcome {
    let server = Server::start("")?;
    let ticks = "read x; for i in 1 2 3 4; do echo tick; sleep 0.3; done; read y";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", ticks]))?);
    let p = ["str", pane.as_str(), "pane"];
    let changed =
        server.ok(&[&p[..], &["wait-change", "str", "Enter", "key", "send-keys"]].concat())?;
    assert_eq!(changed, format!("{pane} changed\n"));
    let began = Instant::now();
    let quiet = server.ok(&[&p[..], &["1", "wait-quiet", "wait"]].concat())?;
    assert_eq!(quiet, format!("{pane} quiet\n"));
    assert!(
        began.elapsed() >= Duration::from_secs(1),
        "{:?}",
        began.elapsed()
    );
    let ended = server.fux(
        &[
            &p[..],
            &[
                "str",
                "NEVER",
                "wait-text",
                "str",
                "Enter",
                "key",
                "send-keys",
            ],
        ]
        .concat(),
    )?;
    assert_eq!(
        (ended.status, ended.stdout.as_str()),
        (3, format!("{pane} exited 0\n").as_str())
    );
    assert!(
        ended.stderr.starts_with("fux: exited: ") && ended.stderr.contains("its program exited 0"),
        "{}",
        ended.stderr
    );
    Ok(())
}

/// `wait-done` waits for a shell command to end as the shell's OSC 133
/// marks say, with its status, and `last-output` reads its output alone;
/// where no command was marked, `last-output` says so and `wait-done`
/// times out saying so. The marks are the README's, for each shell here.
#[test]
fn shell_marks_tell_a_command_s_end_and_output() -> Outcome {
    let readme = include_str!("../README.md");
    for marks in [BASH_MARKS, ZSH_MARKS, FISH_MARKS] {
        assert!(readme.contains(marks), "the README shows {marks}");
    }
    let server = Server::start("")?;
    marked_bash(&server, "b")?;
    let done = |pane: &str, line: &str| {
        server.ok(&[
            "str",
            pane,
            "pane",
            "10",
            "timeout",
            "wait-done",
            "str",
            line,
            "line",
        ])
    };
    let check = |pane: &str| -> Outcome {
        assert_eq!(
            done(pane, "echo one; false")?,
            format!("{} done 1\n", id(&server.ok(&["str", pane, "pane"])?))
        );
        let output = server.ok(&["str", pane, "pane", "last-output", "capture"])?;
        assert_eq!(output, "one\n");
        let ok = done(pane, "echo two")?;
        assert!(ok.ends_with(" done 0\n"), "{ok}");
        Ok(())
    };
    check("b")?;
    // A shell the tests may have: its marks as the README has them.
    // Each read from its own startup file, where the README puts them.
    let shells = [
        ("zsh", ZSH_MARKS, "ZDOTDIR", ".zshrc"),
        ("fish", FISH_MARKS, "XDG_CONFIG_HOME", "fish/config.fish"),
    ];
    for (shell, marks, home, rc) in shells {
        if std::env::var_os("FUX_TEST_SHELLS").is_none() {
            break;
        }
        let dir = server.dir.join(shell);
        let rc = dir.join(rc);
        std::fs::create_dir_all(rc.parent().ok_or("a directory")?).map_err(e)?;
        std::fs::write(&rc, format!("{marks}\n")).map_err(e)?;
        let dir = dir.to_string_lossy().into_owned();
        let words = [
            "opts", "str", shell, "name", "str", home, "str", &dir, "env",
        ];
        server.ok(&[&words[..], &program(&[shell, "-i"]), &["run"]].concat())?;
        check(shell)?;
    }
    let plain = id(&server.ok(&run(&[], &["sh"]))?);
    let refused = server.fux(&["str", &plain, "pane", "last-output", "capture"])?;
    assert!(
        refused.stderr.starts_with("fux: no_marks: "),
        "{}",
        refused.stderr
    );
    let waited = server.fux(&[
        "str",
        &plain,
        "pane",
        "1",
        "timeout",
        "wait-done",
        "str",
        "true",
        "line",
    ])?;
    assert_eq!(waited.status, 124);
    assert!(
        waited.stderr.contains("no shell command was marked"),
        "{}",
        waited.stderr
    );
    Ok(())
}

/// A bell, or a notification with its text, meets `wait-bell`.
#[test]
fn bells_and_notifications_meet_a_bell_wait() -> Outcome {
    let server = Server::start("")?;
    let script = r"read x; printf '\a'; read y; printf '\033]9;built\a'; exec cat";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", script]))?);
    let ring = [
        "str",
        pane.as_str(),
        "pane",
        "wait-bell",
        "str",
        "Enter",
        "key",
        "send-keys",
    ];
    assert_eq!(server.ok(&ring)?, format!("{pane} bell\n"));
    let json = server.ok(&[&ring[..], &["json"]].concat())?;
    let said = format!(r#"{{"pane":"{pane}","met":"bell","notification":"built","status":0}}"#);
    assert_eq!(json.trim_end(), said);
    Ok(())
}

/// Keys reach the program as they are sent, in order: a paste bracketed
/// as it asked, a count repeating only the key before it, bytes as they
/// are, a click encoded as it asked.
#[test]
fn input_reaches_the_program_as_sent() -> Outcome {
    const READS: &str = r#"
import os, sys, tty
tty.setraw(0)
os.write(1, b"\x1b[?2004h\x1b[?1000h\x1b[?1006hready\r\n")
while True:
    got = os.read(0, 256)
    os.write(1, ("read " + got.hex() + "\r\n").encode())
    if got.endswith(b"q"):
        break
"#;
    let server = Server::start("")?;
    let path = server.dir.join("reads.py");
    std::fs::write(&path, READS).map_err(e)?;
    let script = path.to_string_lossy().into_owned();
    let pane = id(&server.ok(&run(
        &["str", "ready", "wait-text"],
        &["python3", "-I", &script],
    ))?);
    // What the program read, as it said it.
    let sent = |keys: &[&str], read: &str| -> Outcome {
        let said = format!("read {read}");
        let wait = ["str", said.as_str(), "wait-text"];
        let send = [
            &["str", pane.as_str(), "pane"][..],
            &wait,
            keys,
            &["send-keys"],
        ];
        let answer = server.ok(&send.concat())?;
        assert_eq!(body(&answer), format!("{said}\n"), "{keys:?}");
        Ok(())
    };
    sent(&["str", "Escape", "key"], "1b")?;
    sent(&["str", "hi", "text", "str", "Enter", "key"], "68690d")?;
    sent(&["str", "a b", "paste"], "1b5b3230307e6120621b5b3230317e")?;
    sent(&["str", "c3a9", "bytes"], "c3a9")?;
    sent(
        &["str", "BSpace", "key", "3", "times", "str", "x", "text"],
        "7f7f7f78",
    )?;
    let click = ["str", "click", "2", "4", "mouse"];
    let said = "read 1b5b3c303b353b334d1b5b3c303b353b336d";
    let clicked = [
        &["str", pane.as_str(), "pane", "str", said, "wait-text"][..],
        &click,
        &["send-mouse"],
    ];
    assert_eq!(body(&server.ok(&clicked.concat())?), format!("{said}\n"));
    let quit = [
        "str",
        pane.as_str(),
        "pane",
        "wait-exit",
        "str",
        "q",
        "text",
        "send-keys",
    ];
    assert_eq!(server.ok(&quit)?, format!("{pane} exited 0\n"));
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
    let right = id(&server.ok(&["str", left, "pane", "horizontal", "split"])?);
    server.ok(&["str", &right, "pane", "25", "percent", "resize-x"])?;
    assert_eq!(size(&right)?, "25x20");
    assert_eq!(size(left)?, "74x20");
    let refused = server.fux(&["str", left, "pane", "100", "resize-x"])?;
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
    let capture = ["str", "c1", "client", "capture-client"];
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
    // P1 9: square pixels (DEC STD 070, 7.1). The cursor stays on the
    // picture's last row (DEC STD 070, 11.3; xterm "requires only a single
    // newline"), so a line goes first lest the text cover it.
    let asked = r"printf '\033[c'; read -r _; printf '\033P9q#1;2;100;0;0!20~-!20~\033\\'; echo; echo drawn";
    server.ok(&typed(
        "%1",
        asked,
        &["str", "62;4;6;21;22;28c", "wait-text"],
    ))?;
    server.ok(&[
        "str",
        "%1",
        "pane",
        "str",
        "drawn",
        "wait-text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    eventually("the picture sent", || {
        client.pump()?;
        Ok(painted(&client, b"\x1bP0;1;0q\"1;1;20;12"))
    })?;
    let json = server.ok(&["str", "%1", "pane", "capture", "json"])?;
    assert!(
        json.contains(r#""col":0,"rows":1,"cropped":0,"cols":2,"width":20,"height":12}]"#),
        "{json}"
    );
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
        "str",
        "job",
        "name",
        "str",
        "V",
        "str",
        "a",
        "env",
        "str",
        &log_arg,
        "log-to",
        "40",
        "10",
        "size",
        "str",
        "full a",
        "wait-text",
    ];
    let pane = id(&server.ok(&run(&before, &["sh", "-c", script]))?);
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
        "str",
        "full b",
        "wait-text",
        "respawn",
    ];
    let again = server.ok(&again)?;
    assert_eq!(again, format!("{pane} text \"full b\"\nfull b\n"));
    let empty = server.fux(&["str", &pane, "pane", "str", "", "wait-text", "wait"])?;
    assert_eq!(
        empty.status, 1,
        "an empty text is refused: {}",
        empty.stderr
    );
    let kept = std::fs::read_to_string(&log).map_err(e)?;
    assert!(kept.contains("full a") && kept.contains("full b"), "{kept}");
    // A full-screen program's last screen, after it left it and exited.
    let menu = r"printf '\033[?1049hpicked beta'; printf '\033[?1049l'";
    let menu = id(&server.ok(&run(&["wait-exit"], &["sh", "-c", menu]))?);
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

/// A name that reads as an id, or one taken, is refused; a tab's name finds
/// it; a click outside the pane is refused; a shell pane's shell is not
/// signalled.
#[test]
fn names_clicks_and_signals_are_checked() -> Outcome {
    let server = Server::start("")?;
    server.ok(&run(&["str", "ed", "name"], &["cat"]))?;
    assert_eq!(
        server.fux(&run(&["str", "@1", "name"], &["cat"]))?.status,
        1
    );
    let taken = server.fux(&["opts", "str", "ed", "name", "new-tab"])?;
    assert_eq!(taken.status, 1, "a name taken");
    // A common spelling is a key's name too; a typo is told the closest.
    server.ok(&["str", "ed", "pane", "str", "Backspace", "key", "send-keys"])?;
    let typo = server.fux(&["str", "ed", "pane", "str", "C-Bakspace", "key", "send-keys"])?;
    assert!(
        typo.stderr.contains("the closest is `C-BSpace`"),
        "{}",
        typo.stderr
    );
    server.ok(&["str", "ed", "tab", "40", "12", "resize"])?;
    assert!(
        server.ok(&["ls"])?.contains(" 40x12"),
        "resized by its tab's name"
    );
    let clicks = "printf '\\033[?1000h\\033[?1006hready'; exec cat";
    let before = [
        "str",
        "m",
        "name",
        "20",
        "5",
        "size",
        "str",
        "ready",
        "wait-text",
    ];
    server.ok(&run(&before, &["sh", "-c", clicks]))?;
    let outside = [
        "str",
        "m",
        "pane",
        "str",
        "click",
        "5",
        "0",
        "mouse",
        "send-mouse",
    ];
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
    let before = ["20", "6", "size", "str", "next", "wait-text"];
    let pane = id(&server.ok(&run(&before, &["sh", "-c", long]))?);
    let line = server.ok(&["str", &pane, "pane", "1", "1", "rows", "capture"])?;
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
    let before = ["20", "6", "size", "str", "Z", "wait-text"];
    let wide = id(&server.ok(&run(
        &before,
        &["sh", "-c", "printf 'A界BC              Z'; exec cat"],
    ))?);
    let cut = |first: &str, last: &str| {
        server.ok(&[
            "str", &wide, "pane", "0", "0", "rows", first, last, "cols", "capture",
        ])
    };
    assert_eq!(cut("2", "4")?, " BC\n");
    assert_eq!(cut("0", "1")?, "A\n");
    assert_eq!(cut("19", "19")?, "Z\n");
    // The cursor is the screen's: on the empty row after the last line;
    // and every row kept was read.
    let json = server.ok(&["str", &pane, "pane", "capture", "json"])?;
    assert!(
        json.contains(r#""cursor":[4,0],"#) && json.contains(r#""above":0"#),
        "{json}"
    );
    Ok(())
}

/// A wait on a workspace waits in each pane: met once every pane meets it,
/// a line of each.
#[test]
fn a_condition_on_a_workspace_is_met_in_each_pane() -> Outcome {
    let server = Server::start("")?;
    let mut panes = Vec::new();
    for pause in ["0", "0.5", "1"] {
        let made = server.ok(&run(
            &["str", "g", "in"],
            &["sh", "-c", &format!("sleep {pause}; echo E")],
        ))?;
        panes.push(id(&made));
    }
    let exited = server.ok(&["str", "g", "workspace", "wait-exit", "wait"])?;
    for pane in &panes {
        assert!(exited.contains(&format!("{pane} exited 0\n")), "{exited}");
    }
    Ok(())
}

/// A met wait's answer is its pane: a word that wants a pane takes it,
/// never a pane named by what the program wrote (a line `%13 fake`).
#[test]
fn a_met_waits_answer_is_its_pane() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(
        &[],
        &["sh", "-c", "read x; echo '%13 fake'; exec cat"],
    ))?);
    let met = [
        &typed(&pane, "go", &["str", "fake", "wait-text"])[..],
        &["str", "name", "get"],
    ];
    assert_eq!(server.ok(&met.concat())?.trim(), "sh");
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
        "str",
        "921600",
        "wait-text",
        "str",
        &path,
        "paste-file",
        "send-keys",
    ];
    let pasted = server.fux(&paste)?;
    assert_eq!(pasted.status, 0, "{}{}", pasted.stdout, pasted.stderr);
    let long: String = std::iter::repeat_n('b', 1000).collect();
    let many = [
        "str",
        &sink,
        "pane",
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
    let mut keys = vec!["str", &sink, "pane"];
    for _ in 0..90 {
        keys.extend(["str", "C-M-F12", "key", "1000", "times"]);
    }
    keys.push("send-keys");
    let refused = server.fux(&keys)?;
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
        "str",
        "LATER",
        "wait-text",
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
    for (before, unused, word) in [
        (&on[..], &["str", "x", "wait-text"][..], "capture"),
        (&on, &["5", "timeout"], "capture"),
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
        (&opts, &["wait-exit"], "new-tab"),
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
        &["wrapped"],
        &["0", "5", "rows"],
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
    Ok(())
}

/// A failed wait answers as text, whatever follows it, and says why on
/// one line of stderr; nothing after it runs.
#[test]
fn a_failed_wait_answers_as_text() -> Outcome {
    let server = Server::start("")?;
    let started = server.ok(&run(&["wait-exit"], &["sh", "-c", "echo LAST; exit 2"]))?;
    let pane = id(&started);
    let p = ["str", pane.as_str(), "pane"];
    let out = server.fux(&[&p[..], &["str", "NEVER", "wait-text", "wait", "json"]].concat())?;
    assert_eq!(out.status, 3, "{}", out.stderr);
    assert_eq!(out.stdout, format!("{pane} exited 2\n"), "{}", out.stderr);
    assert_eq!(out.stderr.lines().count(), 1, "{}", out.stderr);
    assert!(out.stderr.starts_with("fux: exited: "), "{}", out.stderr);
    // The same when the wait is a defined word's last step.
    let wait = [
        &["define", "w"],
        &p[..],
        &["str", "NEVER", "wait-text", "wait", "end"],
    ];
    let out = server.fux(&[&wait.concat()[..], &["w", "json"]].concat())?;
    assert_eq!(out.stdout, format!("{pane} exited 2\n"), "{}", out.stderr);
    Ok(())
}

/// A wait whose pane closes under it (its shell exited) fails so, with
/// status 4 and the code `closed`; one whose program ends to be started
/// again fails with status 3.
#[test]
fn a_wait_whose_pane_closes_or_restarts_says_so() -> Outcome {
    let server = Server::start("")?;
    let typed = [
        "opts",
        "str",
        "sleep 1; exit",
        "typed",
        "str",
        "w",
        "in",
        "new-tab",
    ];
    let pane = id(&server.ok(&typed)?);
    let p = ["str", pane.as_str(), "pane"];
    let out = server.fux(&[&p[..], &["str", "NEVER", "wait-text", "wait"]].concat())?;
    assert_eq!(out.status, 4, "{}{}", out.stdout, out.stderr);
    assert!(out.stderr.starts_with("fux: closed: "), "{}", out.stderr);
    let sleeper = id(&server.ok(&run(&[], &["sleep", "30"]))?);
    let waits = std::process::Command::new(FUX)
        .args(["str", &sleeper, "pane", "str", "NEVER", "wait-text", "wait"])
        .env("FUX_SOCKET", &server.socket)
        .env_remove("FUX_PANE")
        .output();
    let restart = || server.ok(&["str", &sleeper, "pane", "kill-first", "respawn"]);
    let (waited, restarted) = std::thread::scope(|s| {
        let waited = s.spawn(|| waits);
        std::thread::sleep(Duration::from_millis(300));
        (waited.join(), restart())
    });
    restarted?;
    let waited = waited.map_err(|_| "the wait panicked")?.map_err(e)?;
    let stderr = String::from_utf8_lossy(&waited.stderr);
    assert_eq!(waited.status.code(), Some(3), "{stderr}");
    assert!(stderr.starts_with("fux: exited: "), "{stderr}");
    Ok(())
}

/// While a command runs, its output so far is the last output; a
/// full-screen one's is its screen, the cursor where it is on it.
#[test]
fn last_output_is_the_running_command_s_so_far() -> Outcome {
    let server = Server::start("")?;
    marked_bash(&server, "r")?;
    server.ok(&[
        "str",
        "r",
        "pane",
        "wait-done",
        "str",
        "echo BEFORE",
        "line",
    ])?;
    server.ok(&typed(
        "r",
        "echo CURRENT; read -r answer",
        &["str", "CURRENT\n", "wait-text"],
    ))?;
    let output = server.ok(&["str", "r", "pane", "last-output", "capture"])?;
    assert_eq!(output, "CURRENT\n");
    server.ok(&[
        "str",
        "r",
        "pane",
        "wait-done",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    let draw = "printf 'before\\n\\033[?1049h\\033[Habc\\ndef\\nxy%s\\033[2;2H' z; read x";
    server.ok(&typed("r", draw, &["str", "xyz", "wait-text"]))?;
    let json = server.ok(&["str", "r", "pane", "last-output", "capture", "json"])?;
    assert!(json.contains("\"cursor\":[1,1]"), "{json}");
    Ok(())
}

/// A pane renamed keeps its name when its program is started again.
#[test]
fn respawn_keeps_a_rename() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&run(&["wait-exit"], &["sh", "-c", "exit 0"]))?);
    server.ok(&["str", &pane, "pane", "str", "keepme", "rename"])?;
    server.ok(&["str", &pane, "pane", "respawn"])?;
    let name = server.ok(&["str", &pane, "pane", "str", "name", "get"])?;
    assert_eq!(name, "keepme\n");
    Ok(())
}

/// Of several conditions, the first met answers, whatever order they were
/// given in.
#[test]
fn the_condition_met_first_answers() -> Outcome {
    let server = Server::start("")?;
    let script = "read x; echo READY; sleep 0.3; exit 5";
    let pane = id(&server.ok(&run(&[], &["sh", "-c", script]))?);
    let both = [
        "str",
        &pane,
        "pane",
        "wait-exit",
        "str",
        "READY",
        "wait-text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ];
    assert_eq!(server.ok(&both)?, format!("{pane} text \"READY\"\nREADY\n"));
    let exit = [
        "str",
        &pane,
        "pane",
        "str",
        "NEVER",
        "wait-text",
        "wait-exit",
        "wait",
    ];
    assert_eq!(server.ok(&exit)?, format!("{pane} exited 5\n"));
    Ok(())
}

/// `env` gives a program a pane, tab or workspace as fux writes it, so a
/// key's program can act on the pane it came from.
#[test]
fn env_takes_a_pane_tab_or_workspace() -> Outcome {
    let server = Server::start("")?;
    let before = [
        "str",
        "P",
        "%1",
        "env",
        "str",
        "T",
        "@1",
        "env",
        "str",
        "W",
        "+1",
        "env",
        "wait-exit",
    ];
    let pane = id(&server.ok(&run(&before, &["sh", "-c", "echo \"$P $T $W\""]))?);
    assert_eq!(server.ok(&["str", &pane, "pane", "capture"])?, "%1 @1 +1\n");
    Ok(())
}
