//! The command line: a fuxth program, its words' errors and statuses, what
//! it leaves printed, `json`, programs from stdin and files, and a command
//! line typed into a new shell.
mod support;
use support::*;

/// Waits until `%N`'s screen contains `needle`.
fn shows(server: &Server, pane: &str, needle: &str) -> Outcome {
    eventually(&format!("{needle:?} in {pane}"), || {
        Ok(server
            .ok(&["str", pane, "pane", "screen"])?
            .contains(needle))
    })
}

#[test]
fn a_typed_command_runs_in_the_new_shell_and_leaves_its_prompt() -> Outcome {
    let server = Server::start("")?;
    // The line arrives as written: spaces and quotes intact.
    let line = "printf '[%s]\\n' 'a b' \"it's\" '$HOME'";
    let pane = server.ok(&[
        "str",
        "%1",
        "pane",
        "opts",
        "horizontal",
        "str",
        line,
        "typed",
        "split",
    ])?;
    assert!(pane.starts_with(&format!("%2 {READING}\n")), "{pane}");
    shows(&server, "%2", "[a b]")?;
    shows(&server, "%2", "[it's]")?;
    shows(&server, "%2", "[$HOME]")?;
    // The command ended; the shell is still there and takes more input.
    server.type_line("%2", "echo still-here")?;
    shows(&server, "%2", "\nstill-here")?;
    assert!(
        server.ok(&["ls"])?.contains("%2 printf"),
        "the pane is named after its command"
    );
    // new-tab and new-workspace type theirs too.
    server.ok(&[
        "opts",
        "str",
        "+1",
        "workspace",
        "in",
        "str",
        "echo tab-cmd",
        "typed",
        "new-tab",
    ])?;
    shows(&server, "%3", "\ntab-cmd")?;
    server.ok(&["opts", "str", "echo ws-cmd", "typed", "new-workspace"])?;
    shows(&server, "%4", "\nws-cmd")?;
    // A control character would act as a key in the shell: refused, and no
    // pane is made.
    let refused = server.fux(&[
        "str",
        "%1",
        "pane",
        "opts",
        "horizontal",
        "str",
        "echo a\nb",
        "typed",
        "split",
    ])?;
    // A usage error, as its code says: status 2.
    assert_eq!(refused.status, 2);
    assert!(
        refused.stderr.contains("control character"),
        "{}",
        refused.stderr
    );
    assert!(!server.ok(&["ls"])?.contains("%5"));
    Ok(())
}

#[test]
fn targets_are_named_or_here_and_never_guessed() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["str", "%1", "pane", "opts", "horizontal", "split"])?;
    // Without FUX_PANE there is no `here`.
    let out = server.fux(&["here", "opts", "horizontal", "split"])?;
    assert_eq!(out.status, 1);
    assert!(out.stderr.starts_with("fux: no_target: "), "{}", out.stderr);
    // FUX_PANE is the caller's pane, and its tab and workspace.
    let env = [("FUX_PANE", "%2")];
    let out = server.fux_env(&["here", "opts", "vertical", "split"], &env)?;
    assert_eq!(
        (out.status, out.stdout.lines().next()),
        (0, Some(format!("%3 {READING}").as_str())),
        "{}",
        out.stderr
    );
    let out = server.fux_env(&["here", "dup", "tab", "swap", "workspace"], &env)?;
    assert_eq!(out.stdout, "@1\n+1\n");
    // Missing targets, each named.
    let gone = server.fux(&["str", "@9", "tab", "kill"])?;
    assert_eq!(
        (gone.status, gone.stderr.as_str()),
        (1, "fux: no_tab: argument 3: tab: no tab @9\n")
    );
    let named = server.fux(&["str", "nope", "workspace", "kill"])?;
    assert!(
        named.stderr.contains("no workspace named \"nope\""),
        "{}",
        named.stderr
    );
    Ok(())
}

#[test]
fn rename_move_swap_reorder_and_kill() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["str", "%1", "pane", "str", "editor", "rename"])?;
    server.ok(&["str", "@1", "tab", "str", "code", "rename"])?;
    server.ok(&["str", "+1", "workspace", "str", "work", "rename"])?;
    server.ok(&["str", "work", "workspace", "str", "work2", "rename"])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.contains("+1 work2") && ls.contains("@1 code") && ls.contains("%1 editor"),
        "{ls}"
    );
    server.ok(&["opts", "str", "other", "name", "new-workspace"])?;
    let clash = server.fux(&["str", "other", "workspace", "str", "work2", "rename"])?;
    assert!(
        clash.stderr.contains("a workspace is named"),
        "{}",
        clash.stderr
    );
    assert_eq!(
        server
            .fux(&["str", "%1", "pane", "str", "", "rename"])?
            .status,
        1
    );
    // Swap across workspaces.
    server.ok(&["str", "%1", "pane", "str", "%2", "pane", "swap-panes"])?;
    let ls = server.ok(&["ls"])?;
    let at = |needle: &str| ls.find(needle).unwrap_or(usize::MAX);
    assert!(at("%2") < at("+2") && at("%1") > at("+2"), "{ls}");
    // Move to a workspace, then to a new tab.
    server.ok(&["str", "%2", "pane", "opts", "horizontal", "split"])?;
    server.ok(&["str", "%3", "pane", "str", "+2", "workspace", "move-to"])?;
    server.ok(&["str", "%3", "pane", "str", "new-tab", "move-to"])?;
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("@3 tab-2"), "{ls}");
    // Reorder tabs and workspaces.
    server.ok(&["str", "@3", "tab", "str", "previous", "reorder"])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.find("@3").unwrap_or(0) < ls.find("@2").unwrap_or(0),
        "{ls}"
    );
    server.ok(&["str", "+2", "workspace", "str", "previous", "reorder"])?;
    assert!(server.ok(&["ls"])?.starts_with("+2 other"));
    let end = server.fux(&["str", "+2", "workspace", "str", "previous", "reorder"])?;
    assert!(end.stderr.contains("already at that end"), "{}", end.stderr);
    // Kill a pane (its tab, now empty of panes, goes with it), a workspace,
    // and a tab (the workspace's last, so the workspace goes too).
    server.ok(&["str", "%3", "pane", "kill"])?;
    assert!(!server.ok(&["ls"])?.contains("@3 "));
    server.ok(&["str", "work2", "workspace", "kill"])?;
    let ls = server.ok(&["ls"])?;
    assert!(!ls.contains("work2") && ls.contains("other"), "{ls}");
    server.ok(&["opts", "str", "third", "name", "new-workspace"])?;
    server.ok(&["str", "@2", "tab", "kill"])?;
    let ls = server.ok(&["ls"])?;
    assert!(!ls.contains("other") && ls.contains("third"), "{ls}");
    Ok(())
}

#[test]
fn send_keys_signal_and_buffers() -> Outcome {
    let server = Server::start("")?;
    // send-keys types at once; after the prompt, so the output is a line
    // of its own however slowly the shell starts.
    shows(&server, "%1", "$")?;
    server.ok(&[
        "str",
        "%1",
        "pane",
        "keys",
        "str",
        "echo lit",
        "text",
        "str",
        "eral",
        "text",
        "send-keys",
    ])?;
    server.ok(&["str", "%1", "pane", "str", "Enter", "press"])?;
    shows(&server, "%1", "\nliteral")?;
    // signal stops what runs in the foreground, never the shell.
    let idle = server.fux(&["str", "%1", "pane", "str", "TERM", "signal"])?;
    assert!(
        idle.stderr.contains("nothing is running"),
        "{}",
        idle.stderr
    );
    // The sleep never settles: `0 timeout` answers at once.
    server.ok(&[
        "str",
        "%1",
        "pane",
        "keys",
        "str",
        "sleep 600; echo after-sleep",
        "text",
        "str",
        "Enter",
        "key",
        "0",
        "timeout",
        "send-keys",
    ])?;
    eventually("sleep in the foreground", || {
        Ok(server
            .fux(&["str", "%1", "pane", "str", "TERM", "signal"])?
            .status
            == 0)
    })?;
    shows(&server, "%1", "after-sleep")?;
    assert!(server.ok(&["ls"])?.contains("%1"), "the shell survives");
    // No buffers yet.
    assert_eq!(server.ok(&["buffers"])?.trim(), "");
    assert_eq!(server.fux(&["0", "buffer"])?.status, 1);
    Ok(())
}

/// A word's error names its code and exits 1; a wait's failure keeps its
/// status; what is left on the stack is printed, a value a line, bottom
/// first; `json` turns an answer into JSON.
#[test]
fn errors_statuses_and_what_is_left() -> Outcome {
    let server = Server::start("")?;
    let typed = server.fux(&["3", "pane"])?;
    assert_eq!(typed.status, 1);
    assert!(typed.stderr.starts_with("fux: type: "), "{}", typed.stderr);
    let unknown = server.fux(&["frobnicate"])?;
    assert_eq!(unknown.status, 1);
    assert!(
        unknown.stderr.starts_with("fux: unknown_word: ") && unknown.stderr.contains("fux help"),
        "{}",
        unknown.stderr
    );
    // A program's path holding a line end fails to start, the line end
    // shown as `\n`, as is a line separator in a message.
    let path = server.fux(&["opts", "str", "no\nsuch", "program", "run"])?;
    assert!(
        path.stderr.starts_with("fux: start: ") && path.stderr.contains(r"no\nsuch:"),
        "{}",
        path.stderr
    );
    let separator = server.fux(&["str", "a\u{2028}b", "fail"])?;
    assert!(
        separator.stderr.contains(r"a\u{2028}b"),
        "{}",
        separator.stderr
    );
    // A text wait on a program reading its terminal ends at once: the text
    // cannot come before more input. Where fux is not told so (not Linux),
    // it is waited for to the limit.
    let reading = server.fux(&[
        "str",
        "%1",
        "pane",
        "opts",
        "str",
        "never-shown",
        "wait-text",
        "2",
        "timeout",
        "wait",
    ])?;
    assert_eq!(reading.status, CANNOT_COME, "{}", reading.stderr);
    assert!(
        reading.stdout.starts_with("%1 reading\n") || !PROBED,
        "{}",
        reading.stdout
    );
    // On one that sleeps, it waits until its time is up.
    let timed_out = server.fux(&[
        "opts",
        "str",
        "sleep 30",
        "command",
        "0",
        "timeout",
        "run",
        "opts",
        "str",
        "never-shown",
        "wait-text",
        "1",
        "timeout",
        "wait",
    ])?;
    assert_eq!(timed_out.status, 124, "{}", timed_out.stderr);
    assert!(
        timed_out.stderr.starts_with("fux: timeout: "),
        "{}",
        timed_out.stderr
    );
    // A run whose program exits before the text: its pane on stdout, and
    // why on one line of stderr, its last lines with it; what the words
    // before it left is printed first.
    let exited = server.fux(&[
        "str",
        "before",
        "opts",
        "str",
        "sh -c 'echo ONE; echo TWO; exit 5'",
        "command",
        "str",
        "NEVER",
        "wait-text",
        "run",
    ])?;
    assert_eq!(exited.status, 3, "{}", exited.stderr);
    assert!(
        exited.stdout.starts_with("before\n%") && exited.stdout.lines().count() == 2,
        "{}",
        exited.stdout
    );
    assert_eq!(exited.stderr.lines().count(), 1, "{}", exited.stderr);
    assert!(exited.stderr.contains(r"ONE\nTWO"), "{}", exited.stderr);
    let left = server.ok(&[
        "1", "2", "add", "str", "a b", "str", "%1", "pane", "dup", "tab",
    ])?;
    assert_eq!(left, "3\na b\n%1\n@1\n");
    let outcome = server.ok(&["str", "%1", "pane", "opts", "wait", "json"])?;
    assert!(
        outcome.starts_with(&format!(r#"{{"pane":"%1","state":"{READING}""#)),
        "{outcome}"
    );
    let record = server.ok(&["str", "%1", "pane", "info", "json"])?;
    assert!(record.starts_with(r#"{"id":"%1","name":"sh""#), "{record}");
    assert_eq!(
        server.ok(&["str", "%1", "pane", "str", "exited", "get"])?,
        "null\n"
    );
    // An option a word would leave unused is refused, not ignored: a new
    // tab starts a shell, and waits on nothing.
    for unused in [
        &["str", "top", "program"][..],
        &["str", "x", "wait-text"][..],
    ] {
        let refused =
            server.fux(&[&["opts"][..], unused, &["str", "w", "in", "new-tab"]].concat())?;
        assert!(
            refused.stderr.starts_with("fux: unused: "),
            "{}",
            refused.stderr
        );
    }
    // A size past 500 is refused, saying the range it may take, past a
    // cell's too.
    for size in ["501", "65536"] {
        let wide = server.fux(&["str", "%1", "pane", size, "10", "resize"])?;
        assert!(wide.stderr.starts_with("fux: range: "), "{}", wide.stderr);
        assert!(wide.stderr.contains("1 to 500"), "{}", wide.stderr);
        let opened = server.fux(&["opts", size, "10", "size"])?;
        assert!(opened.stderr.contains("1 to 500"), "{}", opened.stderr);
    }
    // A new tab from outside a pane is told the option that places it.
    let nowhere = server.fux(&["opts", "str", "logs", "name", "new-tab"])?;
    assert!(
        nowhere.stderr.contains("`str NAME in`"),
        "{}",
        nowhere.stderr
    );
    let placed = [
        "opts", "str", "logs", "name", "str", "agent", "in", "new-tab",
    ];
    assert_eq!(server.fux(&placed)?.status, 0);
    Ok(())
}

/// A mistake is named by what the program wrote: a failure inside a
/// bundled word by the step that called it; a mistake every run makes,
/// before anything runs; a name that would always be a handle, refused.
#[test]
fn mistakes_name_what_the_program_wrote() -> Outcome {
    let server = Server::start("")?;
    let said = |args: &[&str]| -> Result<String, String> {
        let ran = server.fux(args)?;
        assert_eq!(ran.status, 1, "{args:?}: {}", ran.stdout);
        Ok(ran.stderr)
    };
    let inside = said(&["1", "2", "type"])?;
    assert!(
        inside.starts_with("fux: type: argument 3: type: inside type, send-keys: "),
        "{inside}"
    );
    assert!(
        inside.ends_with("(use: PANE TEXT type, which leaves [answer])\n"),
        "{inside}"
    );
    // Refused before `run` starts anything.
    let before = said(&[
        "opts", "str", "sh", "program", "run", "str", "a", "1", "add",
    ])?;
    assert!(
        before.starts_with("fux: type: argument 9: add: "),
        "{before}"
    );
    assert!(!server.ok(&["ls"])?.contains("agent"), "nothing ran");
    // The language's own words say how they are written.
    for (words, usage) in [
        (
            &["str", "a", "if", "1", "then"][..],
            "(use: BOOL if … else … then)",
        ),
        (&["3", "each", "1", "end"][..], "(use: SEQ each … end)"),
        (&["let", "X"][..], "(use: X let NAME)"),
        (
            &["begin", "str", "x", "until"][..],
            "(use: begin … BOOL until)",
        ),
    ] {
        let wrong = said(words)?;
        assert!(wrong.ends_with(&format!("{usage}\n")), "{wrong}");
    }
    // `help WORD` of no word says so, as help, and where, as any error.
    assert_eq!(
        said(&["help", "no-such-word"])?,
        "fux: unknown_word: argument 2: help: no word \"no-such-word\" (`fux help` lists \
         every word)\n"
    );
    let named = said(&["1", "let", "+7"])?;
    assert!(
        named.contains("+7: cannot be defined: it names a workspace"),
        "{named}"
    );
    let client = said(&["str", "c9", "client", "focused"])?;
    assert!(client.contains("no client c9"), "{client}");
    // What a word takes, all of it: focus's every way, text's booleans.
    let way = said(&["str", "c9", "client", "str", "sideways", "focus"])?;
    assert!(way.contains("up, down, next, previous or last"), "{way}");
    let text = said(&["keys", "opts", "text"])?;
    assert!(text.contains("a string, a number or a boolean"), "{text}");
    // A workspace of one pane is that pane; of more, said so.
    assert_eq!(server.ok(&["%1", "workspace", "pane"])?, "%1\n");
    server.ok(&["%1", "opts", "horizontal", "split"])?;
    let many = said(&["%1", "workspace", "pane"])?;
    assert!(many.contains("+1 is a workspace of 2 panes"), "{many}");
    Ok(())
}

/// `fux -` runs stdin, and `fux FILE` a file: a word with a `/` or ending
/// `.fux`.
#[test]
fn programs_come_from_stdin_and_files() -> Outcome {
    use std::io::Write;
    let server = Server::start("")?;
    let program = "# the panes, doubled\ndefine twice dup add end\n21 twice str %1 pane\n";
    let mut child = std::process::Command::new(FUX)
        .arg("-")
        .env("FUX_SOCKET", &server.socket)
        .env_remove("FUX_PANE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(e)?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(program.as_bytes())
        .map_err(e)?;
    let out = child.wait_with_output().map_err(e)?;
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n%1\n");
    for name in ["prog.fux", "prog"] {
        let file = server.dir.join(name);
        std::fs::write(&file, program).map_err(e)?;
        let path = file.to_string_lossy().into_owned();
        assert_eq!(server.ok(&[&path])?, "42\n%1\n", "{path}");
    }
    let missing = server.fux(&["missing.fux"])?;
    assert_eq!(missing.status, 1);
    assert!(
        missing.stderr.starts_with("fux: read: "),
        "{}",
        missing.stderr
    );
    Ok(())
}

#[test]
fn ls_json_list_keys_and_help() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 50)?;
    client.wait_for("$")?;
    let json = server.ok(&["ls", "json"])?;
    for field in [
        "{\"workspaces\":[{\"id\":\"+1\",\"name\":\"main\",\"tabs\":[{\"id\":\"@1\",\"name\":\"main\",\"panes\":[{\"id\":\"%1\",\"name\":\"sh\",\"title\":\"\",\"rows\":9,\"cols\":50,\"pid\":",
        "\"clients\":[{\"id\":\"c1\",\"rows\":10,\"cols\":50,\"workspace\":\"+1\",\"tab\":\"@1\",\"pane\":\"%1\",\"zoom\":false}],\"grants\":[]}",
    ] {
        assert!(json.contains(field), "{field}\nin\n{json}");
    }
    // The command line's own keymap: the defaults.
    let keys = server.ok(&["list-keys"])?;
    assert!(keys.contains("After the prefix, C-b"), "{keys}");
    // help lists every word.
    let help = server.ok(&["help"])?;
    for word in fux::script::WORDS {
        assert!(
            help.lines()
                .any(|l| l.split_whitespace().next() == Some(word.name)),
            "{} is not in help",
            word.name
        );
    }
    Ok(())
}

/// A shell setup that writes during its startup and then discards pending
/// input (as some line editors and plugins do) must not lose a typed
/// command: fux types it only once the output has gone quiet. The stand-in
/// startup is one Python process -- so no process start opens a gap in its
/// output -- that prints a line every 5 ms for about 200 ms and flushes the
/// terminal's pending input after the tenth line, some 50 ms in; then sh
/// starts.
///
/// That tests something only if the stand-in's first line comes before
/// fux's deadline and its lines up to the flush come within fux's quiet
/// time of each other. A slow machine cannot always manage it (under
/// emulated amd64 its gaps were 50 to 85 ms), and then fux types in a gap,
/// as designed; so the stand-in reports its timing, and an attempt that
/// missed the premise is run again, up to ten times. If it never holds, the
/// machine is too slow for the test to say anything about fux, and it says
/// so and passes: a failure here always means a lost command.
#[test]
fn a_typed_command_survives_a_startup_that_writes_then_discards_input() -> Outcome {
    let mut missed = Vec::new();
    for _ in 0..10 {
        match typed_command_after_a_noisy_startup()? {
            Ok(()) => return Ok(()),
            Err(why) => missed.push(why),
        }
    }
    eprintln!(
        "not tested: in ten attempts the stand-in startup never wrote fast enough: {missed:?}"
    );
    Ok(())
}

/// One attempt: `Err` inside if the premise did not hold, and nothing was
/// tested.
fn typed_command_after_a_noisy_startup() -> Result<Result<(), String>, String> {
    let dir = fuxix::file::scratch().map_err(e)?;
    let script = dir.join(format!("fux-noisy-startup-{}.py", std::process::id()));
    let timing = dir.join(format!("fux-noisy-timing-{}", std::process::id()));
    let _ = std::fs::remove_file(&timing);
    std::fs::write(
        &script,
        format!(
            "import termios, time\n\
             first = time.time()\n\
             last = time.monotonic()\n\
             gap = 0.0\n\
             for i in range(40):\n\
             \x20   print('starting-%d' % i, flush=True)\n\
             \x20   now = time.monotonic()\n\
             \x20   if i <= 10:\n\
             \x20       gap = max(gap, now - last)\n\
             \x20   last = now\n\
             \x20   time.sleep(0.005)\n\
             \x20   if i == 10:\n\
             \x20       termios.tcflush(0, termios.TCIFLUSH)\n\
             open('{}', 'w').write('%f %f' % (first, gap))\n",
            timing.display()
        ),
    )
    .map_err(e)?;
    let shell = format!(
        "str shell str '/bin/sh -c \"python3 {}; exec /bin/sh\"' set",
        script.display()
    );
    let server = Server::start(&shell)?;
    let split_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(e)?
        .as_secs_f64();
    server.ok(&[
        "str",
        "%1",
        "pane",
        "opts",
        "horizontal",
        "str",
        "echo typed-after-quiet",
        "typed",
        "split",
    ])?;
    // The command ran: its output is on a line of its own, or after a
    // prompt drawn before it; which, the premise does not decide.
    let ran = |screen: &str| {
        screen
            .lines()
            .any(|l| l.ends_with("typed-after-quiet") && !l.contains("echo"))
    };
    let result = eventually("the typed command's output", || {
        Ok(ran(&server.ok(&["str", "%2", "pane", "screen"])?))
    });
    let screen = server.ok(&["str", "%2", "pane", "screen"])?;
    let _ = std::fs::remove_file(&script);
    let measured = std::fs::read_to_string(&timing).unwrap_or_default();
    let _ = std::fs::remove_file(&timing);
    let mut words = measured.split_whitespace().map(str::parse::<f64>);
    let (Some(Ok(first)), Some(Ok(gap))) = (words.next(), words.next()) else {
        return Err(format!(
            "the stand-in reported no timing; %2 shows:\n{screen}"
        ));
    };
    // With a margin: the deadline counts from a little before the split.
    let late = first - split_at;
    let deadline = fux::session::TYPE_WAIT.as_secs_f64() * 0.8;
    let quiet = fux::pane::QUIET.as_secs_f64() * 0.8;
    if late > deadline || gap > quiet {
        return Ok(Err(format!(
            "first line {:.0} ms after the split, largest gap {:.0} ms",
            late * 1000.0,
            gap * 1000.0
        )));
    }
    if result.is_err() {
        return Err(format!("the typed command was lost; %2 shows:\n{screen}"));
    }
    assert!(
        screen.contains("starting-39"),
        "the startup ran whole: {screen}"
    );
    Ok(Ok(()))
}

/// A caller in a directory whose name is not UTF-8 starts what it runs
/// there: the path's bytes reach the server as they are, not made text.
#[test]
fn a_directory_named_in_any_bytes_is_where_a_run_starts() -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let server = Server::start("")?;
    let dir = server.dir.join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    std::fs::create_dir(&dir).map_err(e)?;
    let out = std::process::Command::new(fux())
        .args(["opts", "str", "touch here", "command", "run"])
        .current_dir(&dir)
        .env("FUX_SOCKET", &server.socket)
        .env("XDG_CONFIG_HOME", server.dir.join("config"))
        .env_remove("FUX_PANE")
        .env_remove("FUX_SERVER")
        .output()
        .map_err(e)?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    eventually("the file it made there", || Ok(dir.join("here").exists()))
}
