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
    // As timeout(1), a wait for the exit exits with the program's status.
    let ended = server.fux(&["wait", "-t", &pane, "--exit"])?;
    assert_eq!((ended.status, ended.stdout.as_str()), (3, "exited 3\n"));
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
    assert!(refused.stderr.contains("exited 3"), "{}", refused.stderr);
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
    assert_eq!(late.status, 124, "a timeout is told from a failure");
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
    server.ok(&["resize", "-t", left, "100x22"])?;
    assert_eq!(size(left)?, "100x22", "a pane's tab");
    server.ok(&["resize", "-t", tab, "100x20"])?;
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
    // Keys typed at it are a person's: the prefix and `c` open copy mode.
    server.ok(&["send-keys", "-c", "c1", "C-b", "c"])?;
    let screen = server.ok(&["capture-client", "-c", "c1"])?;
    assert!(screen.contains("COPY"), "{screen}");
    server.ok(&["send-keys", "-c", "c1", "q"])?;
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
        out.stderr.contains("always prints the screen"),
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

/// A pane's pictures reach a client whose terminal draws sixel, clipped
/// to the pane, after the program was told the terminal draws them; and
/// `--json` says where they are.
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
    server.ok(&["send-keys", "-t", "%1", "--text", asked, "Enter"])?;
    server.ok(&["wait", "-t", "%1", "--text", "62;4;22c"])?;
    server.ok(&["send-keys", "-t", "%1", "Enter", "--wait-idle", "200"])?;
    eventually("the picture sent", || {
        client.pump()?;
        Ok(painted(&client, b"\x1bP0;1;0q\"1;1;20;12"))
    })?;
    let json = server.ok(&["capture-pane", "-t", "%1", "--json"])?;
    assert!(
        json.contains(r#""col":0,"rows":1,"cols":2,"width":20,"height":12}]"#),
        "{json}"
    );
    Ok(())
}

/// A wait misses nothing and never waits out its timeout for nothing: text
/// that scrolled past since it began is found and its line printed, a
/// program that exits first ends it (status 3), a timeout is 124, and it
/// watches several panes at once. send-keys arms its wait before the keys
/// go in, so what they bring cannot come too soon to be seen.
#[test]
fn waits_see_what_scrolled_past_and_end_when_the_program_does() -> Outcome {
    let server = Server::start("")?;
    let script = "read go; echo 'the needle line'; seq 300; read go; exit 2";
    let pane = id(&server.ok(&["run", "--size", "40x10", "--", "sh", "-c", script])?);
    let found = server.ok(&["send-keys", "-t", &pane, "Enter", "--wait-text", "needle"])?;
    assert_eq!(found, "the needle line\n", "found though it scrolled away");
    let json = server.ok(&["wait", "-t", &pane, "--text", "300", "--json"])?;
    assert!(json.contains(r#""met":"text""#), "{json}");
    let ended = server.fux(&["send-keys", "-t", &pane, "Enter", "--wait-text", "never"])?;
    assert_eq!(ended.status, 3, "{}", ended.stderr);
    assert!(ended.stderr.contains("exited 2"), "{}", ended.stderr);
    let ended = server.fux(&["wait", "-t", &pane, "--exit"])?;
    assert_eq!((ended.status, ended.stdout.as_str()), (2, "exited 2\n"));
    // run waits from the program's first output: a flood cannot outrun it.
    let flood = "echo 'a needle'; seq 300; exec cat";
    let started = server.ok(&["run", "--wait-text", "needle", "--", "sh", "-c", flood])?;
    let (first, line) = started
        .split_once('\n')
        .ok_or(format!("two lines: {started}"))?;
    assert!(first.starts_with('%') && line == "a needle\n", "{started}");
    // Several panes: the first to show the text answers, and says which.
    let quiet = id(&server.ok(&["run", "--", "cat"])?);
    let talker = id(&server.ok(&["run", "--", "sh", "-c", "read go; echo ready; exec cat"])?);
    let late = server.fux(&["wait", "-t", &quiet, "--text", "ready", "--timeout", "0.3"])?;
    assert_eq!(late.status, 124);
    let both = std::process::Command::new(FUX)
        .args(["wait", "-t", &quiet, "-t", &talker, "--text", "ready"])
        .env("FUX_SOCKET", &server.socket)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(e)?;
    server.ok(&["send-keys", "-t", &quiet, "--text", "x"])?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    server.ok(&["send-keys", "-t", &talker, "Enter"])?;
    let out = both.wait_with_output().map_err(e)?;
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{talker} ready\n")
    );
    Ok(())
}

/// A pane's revision grows as its screen changes: `wait --change --since`
/// answers at once for a change already made; keys that change nothing
/// are told by `--wait-idle` (status 5), not taken for an answer.
#[test]
fn revisions_tell_changes_apart_from_none() -> Outcome {
    let server = Server::start("")?;
    let pane = id(&server.ok(&["run", "--", "sh", "-c", "stty -echo; exec cat"])?);
    server.ok(&["wait", "-t", &pane, "--idle", "100"])?;
    let revision = |json: &str| -> Result<u64, String> {
        let at = json
            .find(r#""revision":"#)
            .ok_or(format!("no revision: {json}"))?;
        let digits: String = json
            .get(at.saturating_add(11)..)
            .unwrap_or_default()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().map_err(e)
    };
    let before = revision(&server.ok(&["capture-pane", "-t", &pane, "--json"])?)?;
    let unchanged = server.fux(&["send-keys", "-t", &pane, "x", "--wait-idle", "200"])?;
    assert_eq!(unchanged.status, 5, "{}", unchanged.stderr);
    server.ok(&["send-keys", "-t", &pane, "Enter"])?;
    let since = before.to_string();
    let changed = server.ok(&["wait", "-t", &pane, "--change", "--since", &since, "--json"])?;
    assert!(revision(&changed)? > before, "{changed}");
    Ok(())
}

/// A shell's OSC 133 marks tell when a command ends and how, and which
/// output was its; a bell or a desktop notification is a pane asking for
/// a person, which `wait --bell` hears and `--json` counts.
#[test]
fn shell_marks_and_bells_are_heard() -> Outcome {
    let server = Server::start("")?;
    let script = r"read go; printf '\033]133;C\007'; echo out1; echo out2; printf '\033]133;D;7\007'; echo prompt; read go; printf '\a\033]9;tests passed\007'; exec cat";
    let pane = id(&server.ok(&["run", "--", "sh", "-c", script])?);
    let done = server.ok(&["send-keys", "-t", &pane, "Enter", "--wait-done"])?;
    assert_eq!(done, "done 7\n");
    let output = server.ok(&["capture-pane", "-t", &pane, "--last-output"])?;
    assert_eq!(output, "out1\nout2\n");
    server.ok(&["send-keys", "-t", &pane, "Enter", "--wait-bell"])?;
    let json = server.ok(&["capture-pane", "-t", &pane, "--json"])?;
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
/// name; `--log` keeps all it wrote; `-S all` reads all history, `--main`
/// what a full-screen program hides; a mistyped key name is refused.
#[test]
fn run_programs_are_restarted_logged_and_read_whole() -> Outcome {
    let server = Server::start("")?;
    let log = server.dir.join("job.log");
    let log_arg = log.to_string_lossy().into_owned();
    let script = "echo before; seq 500; printf '\\033[?1049h'; echo \"full $V\"; read go";
    let args = [
        "run", "-n", "job", "--env", "V=a", "--log", &log_arg, "--size", "40x10",
    ];
    let pane = id(&server.ok(&[&args[..], &["--", "sh", "-c", script]].concat())?);
    server.ok(&["wait", "-t", "job", "--text", "full a"])?;
    let other = [
        "run",
        "-n",
        "other",
        "--",
        "sh",
        "-c",
        "printf 'R> '; exec cat",
    ];
    server.ok(&other)?;
    let prompt = [
        "wait",
        "-t",
        "job",
        "-t",
        "other",
        "--text",
        "R> ",
        "--timeout",
        "5",
    ];
    assert!(
        server.ok(&prompt)?.ends_with("R>\n"),
        "a prompt's blank is the line's end"
    );
    let main = server.ok(&["capture-pane", "-t", &pane, "--main", "-S", "all"])?;
    assert!(main.starts_with("before\n1\n"), "{main}");
    assert!(main.contains("\n500\n"), "{main}");
    let refused = server.fux(&["send-keys", "-t", &pane, "Entr"])?;
    assert_eq!(refused.status, 2);
    assert!(refused.stderr.contains("Entr"), "{}", refused.stderr);
    let busy = server.fux(&["respawn", "-t", &pane])?;
    assert_eq!(busy.status, 1, "it runs: {}", busy.stderr);
    let again = [
        "respawn",
        "-t",
        "job",
        "-k",
        "--env",
        "V=b",
        "--wait-text",
        "full b",
    ];
    assert_eq!(server.ok(&again)?, format!("{pane}\nfull b\n"));
    let empty = server.fux(&["wait", "-t", &pane, "--text", ""])?;
    assert_eq!(empty.status, 2, "an empty text is on every screen");
    let kept = std::fs::read_to_string(&log).map_err(e)?;
    assert!(kept.contains("full a") && kept.contains("full b"), "{kept}");
    server.ok(&["notify", "-t", &pane, "done"])?;
    // A full-screen program's last screen, after it left it and exited.
    let menu = r"printf '\033[?1049hpicked beta'; printf '\033[?1049l'";
    let started = server.ok(&["run", "--wait-exit", "--", "sh", "-c", menu])?;
    let (menu, ended) = started.split_once('\n').ok_or("the id, then the end")?;
    assert_eq!(ended, "exited 0\n");
    assert!(!server.ok(&["capture-pane", "-t", menu])?.contains("picked"));
    assert_eq!(
        server.ok(&["capture-pane", "-t", menu, "--alt"])?,
        "picked beta\n"
    );
    Ok(())
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
    let home_env = format!("HOME={}", home.display());
    let start = [
        "run",
        "-n",
        "sh",
        "--env",
        &home_env,
        "--wait-text",
        "mine>",
    ];
    server.ok(&[&start[..], &["--", "bash"]].concat())?;
    let typed = [
        "send-keys",
        "-t",
        "sh",
        "--text",
        "sleep 0.2; false",
        "Enter",
    ];
    assert_eq!(
        server.ok(&[&typed[..], &["--wait-done"]].concat())?,
        "done 1\n"
    );
    let echo = "sleep 0.2; echo RESULT=$((6*7))";
    let trap = [
        "send-keys",
        "-t",
        "sh",
        "--text",
        echo,
        "Enter",
        "--wait-text",
        "RESULT=",
    ];
    assert_eq!(server.ok(&trap)?, "RESULT=42\n", "not the typed line");
    server.ok(&["wait", "-t", "sh", "--text", "[saw 0]"])?;
    assert_eq!(
        server.ok(&["capture-pane", "-t", "sh", "--last-output"])?,
        "RESULT=42\n"
    );
    let screen = server.ok(&["capture-pane", "-t", "sh"])?;
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
        let zenv = format!("ZDOTDIR={}", zdot.display());
        let start = [
            "run",
            "-n",
            "z",
            "--env",
            &zenv,
            "--wait-text",
            "zed>",
            "--",
            "zsh",
        ];
        server.ok(&start)?;
        let typed = [
            "send-keys",
            "-t",
            "z",
            "--text",
            "(exit 3)",
            "Enter",
            "--wait-done",
        ];
        assert_eq!(server.ok(&typed)?, "done 3\n");
    }
    // bash without its startup files, as agents start it, is marked too.
    server.ok(&[
        "run",
        "-n",
        "bare",
        "--wait-idle",
        "200",
        "--",
        "bash",
        "--norc",
        "-i",
    ])?;
    // `true` first: bash before 4.4 (macOS's) marks from a trap, which a
    // subshell alone does not set off.
    let typed = [
        "send-keys",
        "-t",
        "bare",
        "--text",
        "true; (exit 4)",
        "Enter",
        "--wait-done",
    ];
    assert_eq!(server.ok(&typed)?, "done 4\n");
    // fish, its prompt from its own config, marked by an event handler.
    if has("fish")? {
        let xdg = server.dir.join("xdg");
        std::fs::create_dir_all(xdg.join("fish")).map_err(e)?;
        let prompt = "function fish_prompt; echo -n 'fsh> '; end\n";
        std::fs::write(xdg.join("fish").join("config.fish"), prompt).map_err(e)?;
        let xenv = format!("XDG_CONFIG_HOME={}", xdg.display());
        let start = [
            "run",
            "-n",
            "f",
            "--env",
            &xenv,
            "--wait-text",
            "fsh>",
            "--",
            "fish",
        ];
        server.ok(&start)?;
        let typed = [
            "send-keys",
            "-t",
            "f",
            "--text",
            "sh -c 'exit 6'",
            "Enter",
            "--wait-done",
        ];
        assert_eq!(server.ok(&typed)?, "done 6\n");
        let echo = [
            "send-keys",
            "-t",
            "f",
            "--text",
            "echo fish-$status",
            "Enter",
            "--wait-done",
        ];
        server.ok(&echo)?;
        assert_eq!(
            server.ok(&["capture-pane", "-t", "f", "--last-output"])?,
            "fish-6\n"
        );
    }
    let off = Server::start("set shell-marks off")?;
    let start = [
        "run",
        "-n",
        "sh",
        "--env",
        &home_env,
        "--wait-text",
        "mine>",
        "--",
        "bash",
    ];
    off.ok(&start)?;
    let typed = [
        "send-keys",
        "-t",
        "sh",
        "--text",
        "true",
        "Enter",
        "--wait-done",
    ];
    let unmarked = off.fux(&[&typed[..], &["--timeout", "0.5"]].concat())?;
    assert_eq!(unmarked.status, 124, "{}", unmarked.stderr);
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
/// program that ignores hangups; a close says what it closed; and a wait
/// that ends unmet says what the pane is doing.
#[test]
fn actions_are_confirmed_and_screens_read_plainly() -> Outcome {
    let server = Server::start("")?;
    let path = server.dir.join("clicks.py");
    std::fs::write(&path, CLICKS).map_err(e)?;
    let script = path.to_string_lossy().into_owned();
    let start = [
        "run",
        "-n",
        "dialog",
        "--wait-text",
        "ready",
        "--",
        "python3",
        "-I",
        &script,
    ];
    server.ok(&start)?;
    let click = ["send-mouse", "-t", "dialog", "click", "0", "0"];
    let answered = server.ok(&[&click[..], &["--wait-text", "clicked"]].concat())?;
    assert_eq!(answered, "clicked once\n");
    let ignored = server.fux(&[&click[..], &["--wait-idle", "200"]].concat())?;
    assert_eq!(ignored.status, 5, "nothing changed: {}", ignored.stderr);
    // A selection in reverse video, marked in plain text.
    let menu = r"printf 'alpha\n\033[7mbeta\033[0m\ngamma\n'; exec cat";
    let started = server.ok(&[
        "run",
        "-n",
        "menu",
        "--wait-text",
        "gamma",
        "--",
        "sh",
        "-c",
        menu,
    ])?;
    let menu_pane = started.lines().next().ok_or("its id")?.to_owned();
    let marked = server.ok(&["capture-pane", "-t", &menu_pane, "--marks"])?;
    assert_eq!(marked, "alpha\n⟦beta⟧\ngamma\n");
    // A program that ignores TERM and hangups: a timeout says what it is
    // doing, KILL ends it, and the wait says so.
    let stubborn = "trap '' TERM HUP INT; echo still here; exec sleep 100";
    server.ok(&[
        "run",
        "-n",
        "stub",
        "--wait-text",
        "here",
        "--",
        "sh",
        "-c",
        stubborn,
    ])?;
    let term = server.fux(&[
        "signal",
        "-t",
        "stub",
        "TERM",
        "--wait-exit",
        "--timeout",
        "0.5",
    ])?;
    assert_eq!(term.status, 124);
    assert!(
        term.stderr.contains("unchanged for") && term.stderr.contains("last line: `still here`"),
        "{}",
        term.stderr
    );
    let kill = server.fux(&["signal", "-t", "stub", "KILL", "--wait-exit"])?;
    assert_eq!(
        (kill.status, kill.stdout.as_str()),
        (137, "killed by SIGKILL\n")
    );
    // A program that exits first: its last words come with the error.
    let crash = server.fux(&[
        "run",
        "--wait-text",
        "READY",
        "--",
        "sh",
        "-c",
        "echo 'fatal: no config'; exit 2",
    ])?;
    assert_eq!(crash.status, 3);
    assert!(
        crash.stderr.contains("fatal: no config"),
        "{}",
        crash.stderr
    );
    // A tab closed by its name, saying what went.
    let closed = server.ok(&["kill-tab", "-t", "dialog"])?;
    assert!(
        closed.starts_with("closed @") && closed.contains(" %"),
        "{closed}"
    );
    // A split where it is asked, a line typed as it is.
    let sub = server.dir.join("sub");
    std::fs::create_dir_all(&sub).map_err(e)?;
    let sub_arg = sub.to_string_lossy().into_owned();
    let split = [
        "split",
        "-h",
        "-t",
        &menu_pane,
        "--cwd",
        &sub_arg,
        "--",
        "pwd && echo split-$((1+1))",
    ];
    let new = id(&server.ok(&split)?);
    let screen = server.ok(&["wait", "-t", &new, "--text", "split-2", "--print"])?;
    assert!(screen.contains(&sub_arg), "{screen}");
    // A program just started has drawn nothing: its text goes only once it
    // has come.
    let loading = r"sleep 0.3; printf loading; sleep 0.3; printf '\r\033[Kready\n'; exec cat";
    let gone = [
        "run",
        "--wait-gone",
        "loading",
        "--print",
        "--",
        "sh",
        "-c",
        loading,
    ];
    let done = server.ok(&gone)?;
    assert!(
        done.ends_with("ready\n") || done.contains("\nready"),
        "{done}"
    );
    let late = server.fux(&["send-keys", "-t", &menu_pane, "BSpace", "-N", "10"])?;
    assert!(late.stderr.contains("before the keys"), "{}", late.stderr);
    let valued = server.fux(&["send-keys", "-t", &menu_pane, "q", "--wait-exit", "5"])?;
    assert!(valued.stderr.contains("--timeout"), "{}", valued.stderr);
    let hint = server.fux(&["wait", "-t", &menu_pane, "--wait-gone", "x"])?;
    assert!(hint.stderr.contains("--gone"), "{}", hint.stderr);
    // After --, words are the program's, however they look.
    let theirs = server.ok(&["run", "--wait-exit", "--", "echo", "--wait-for-it"])?;
    assert!(theirs.ends_with("exited 0\n"), "{theirs}");
    // A name `-t` finds is one tab's only: a second is refused, saying why.
    let again = server.fux(&["run", "-n", "menu", "--", "cat"])?;
    assert_eq!(again.status, 1);
    assert!(
        again.stderr.contains("already") && again.stderr.contains(&menu_pane),
        "{}",
        again.stderr
    );
    // A line printed just before a program takes the full screen is seen,
    // and a full-screen program's last screen is what --print prints.
    let behind = r"echo 'error: disk full'; printf '\033[?1049hFULL SCREEN'; exec cat";
    let seen = server.ok(&["run", "--wait-text", "disk full", "--", "sh", "-c", behind])?;
    assert!(seen.ends_with("\nerror: disk full\n"), "{seen}");
    let quits = r"printf '\033[?1049hpicked beta'; printf '\033[?1049l'";
    let printed = server.ok(&["run", "--wait-exit", "--print", "--", "sh", "-c", quits])?;
    assert!(printed.contains("exited 0\npicked beta"), "{printed}");
    Ok(())
}

/// What the stress tests found waits get wrong: a pane that exited does
/// not end a wait another pane meets; text that comes again after a clear,
/// or after a Space key, is new; a respawn ends a wait on the program
/// before; and Enter typed after text, before more text, is read alone.
#[test]
fn waits_hold_up_under_stress() -> Outcome {
    let server = Server::start("")?;
    let failed = server.fux(&[
        "run",
        "-n",
        "a",
        "--wait-exit",
        "--",
        "sh",
        "-c",
        "echo FAIL-A; exit 1",
    ])?;
    assert_eq!(failed.status, 1, "the program's status");
    let b = id(&server.ok(&[
        "run",
        "-n",
        "b",
        "--",
        "sh",
        "-c",
        "echo PASS; exec sleep 30",
    ])?);
    server.ok(&["wait", "-t", "b", "--text", "PASS"])?;
    let either = server.ok(&["wait", "-t", "a", "-t", "b", "--text", "PASS"])?;
    assert_eq!(
        either,
        format!("{b} PASS\n"),
        "an exited pane does not end it"
    );
    server.ok(&[
        "run",
        "-n",
        "sh",
        "--wait-idle",
        "300",
        "--",
        "bash",
        "--norc",
        "-i",
    ])?;
    let again = [
        "send-keys",
        "-t",
        "sh",
        "--text",
        "clear; echo READY",
        "Enter",
        "--wait-text",
        "READY",
    ];
    server.ok(&again)?;
    // Cleared and printed again, other text: new, on the same row.
    let other = [
        "send-keys",
        "-t",
        "sh",
        "--text",
        "clear; echo READY 2",
        "Enter",
        "--wait-text",
        "READY",
    ];
    assert_eq!(server.ok(&other)?, "READY 2\n");
    let spaced = [
        "send-keys",
        "-t",
        "sh",
        "echo",
        "Space",
        "done42",
        "Enter",
        "--wait-text",
        "done42",
    ];
    assert_eq!(server.ok(&spaced)?, "done42\n");
    server.ok(&["run", "-n", "r", "--", "sleep", "60"])?;
    let waiting = std::process::Command::new(FUX)
        .args(["wait", "-t", "r", "--exit", "--timeout", "10"])
        .env("FUX_SOCKET", &server.socket)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(e)?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    server.ok(&["respawn", "-t", "r", "-k"])?;
    let ended = waiting.wait_with_output().map_err(e)?;
    assert_eq!(ended.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&ended.stderr).contains("respawn"));
    Ok(())
}

/// What the stress tests found the command line gets wrong: `--help`
/// stops nothing; a name is put in place of fux's own -t only; a name that
/// reads as an id, or one taken, is refused; workspace-or-tab targets take
/// a tab's name; a mistyped flag is not typed into the program; a click
/// outside the pane is refused; a shell is not signalled.
#[test]
fn the_command_line_holds_up_under_stress() -> Outcome {
    let server = Server::start("")?;
    let help = server.ok(&["kill-server", "--help"])?;
    assert!(help.contains("Stops the server"), "{help}");
    server.ok(&["ls"])?;
    server.ok(&["run", "-n", "ed", "--wait-idle", "200", "--", "cat"])?;
    let typed = [
        "send-keys",
        "-t",
        "ed",
        "-l",
        "grep",
        "-t",
        "ed",
        "--wait-idle",
        "200",
        "--print",
    ];
    assert!(
        server.ok(&typed)?.contains("grep-ted"),
        "the words are typed as they are"
    );
    let printed = server.ok(&["run", "--wait-exit", "--print", "--", "echo", "-t", "ed"])?;
    assert!(printed.contains("-t ed"), "{printed}");
    assert_eq!(server.fux(&["run", "-n", "@1", "--", "cat"])?.status, 1);
    assert_eq!(
        server.fux(&["new-tab", "-n", "ed"])?.status,
        1,
        "a name taken"
    );
    server.ok(&["resize", "-t", "ed", "40x12"])?;
    assert!(
        server.ok(&["ls"])?.contains(" 40x12"),
        "resized by its tab's name"
    );
    let typo = server.fux(&["send-keys", "-t", "ed", "x", "--wait-idel", "300"])?;
    assert_eq!(typo.status, 2);
    assert!(typo.stderr.contains("--wait-idel"), "{}", typo.stderr);
    let clicks = "printf '\\033[?1000h\\033[?1006hready'; exec cat";
    server.ok(&[
        "run",
        "-n",
        "m",
        "--size",
        "20x5",
        "--wait-text",
        "ready",
        "--",
        "sh",
        "-c",
        clicks,
    ])?;
    let outside = server.fux(&["send-mouse", "-t", "m", "click", "5", "0"])?;
    assert!(
        outside.stderr.contains("rows count 0-4"),
        "{}",
        outside.stderr
    );
    server.ok(&[
        "run",
        "-n",
        "shell",
        "--wait-idle",
        "300",
        "--",
        "bash",
        "--norc",
        "-i",
    ])?;
    let signalled = server.fux(&["signal", "-t", "shell", "HUP"])?;
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
    server.ok(&[
        "run",
        "-n",
        "menu",
        "--wait-text",
        "beta",
        "--",
        "sh",
        "-c",
        menu,
    ])?;
    assert_eq!(
        server.ok(&["capture-pane", "-t", "menu", "--marks"])?,
        "alpha\n⟦beta⟧\n⟦ 日本 ⟧\n"
    );
    let flood = r"seq 50; printf '\033[H\033[2J\033[3Jnew'; exec cat";
    server.ok(&[
        "run",
        "-n",
        "clear",
        "--size",
        "20x5",
        "--wait-text",
        "new",
        "--",
        "sh",
        "-c",
        flood,
    ])?;
    assert_eq!(
        server.ok(&["capture-pane", "-t", "clear", "-S", "all"])?,
        "new\n"
    );
    let home = server.dir.join("home");
    std::fs::create_dir_all(&home).map_err(e)?;
    let rc = "set -u\nexport PROMPT_COMMAND='echo own'\n";
    std::fs::write(home.join(".bashrc"), rc).map_err(e)?;
    let env = format!("HOME={}", home.display());
    server.ok(&[
        "run",
        "-n",
        "b",
        "--env",
        &env,
        "--wait-idle",
        "300",
        "--",
        "bash",
    ])?;
    let broken = [
        "send-keys",
        "-t",
        "b",
        "--text",
        "if then fi",
        "Enter",
        "--wait-done",
    ];
    assert_eq!(server.ok(&broken)?, "done 2\n", "bash could not parse it");
    let nested = [
        "send-keys",
        "-t",
        "b",
        "--text",
        "bash --norc -c 'echo $PROMPT_COMMAND'",
        "Enter",
        "--wait-done",
    ];
    server.ok(&nested)?;
    let output = server.ok(&["capture-pane", "-t", "b", "--last-output"])?;
    assert!(
        !output.contains("__fux"),
        "fux's PROMPT_COMMAND is not inherited: {output}"
    );
    Ok(())
}
