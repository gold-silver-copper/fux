//! The command line: a fuxth program, its words' errors and statuses, what
//! it leaves printed, `json`, and programs from stdin and files.
mod support;
use support::*;

/// Waits until `%N`'s screen contains `needle`.
fn shows(server: &Server, pane: &str, needle: &str) -> Outcome {
    eventually(&format!("{needle:?} in {pane}"), || {
        Ok(server
            .ok(&["str", pane, "pane", "capture"])?
            .contains(needle))
    })
}

#[test]
fn targets_are_named_or_here_and_never_guessed() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
    // Without FUX_PANE there is no `here`.
    let out = server.fux(&["here", "horizontal", "split"])?;
    assert_eq!(out.status, 1);
    assert!(out.stderr.starts_with("fux: no_target: "), "{}", out.stderr);
    // FUX_PANE, of the server FUX_SERVER says, is the caller's pane, and
    // its tab and workspace; with no FUX_SERVER, no one's.
    let alone = server.fux_env(&["here", "kill"], &[("FUX_PANE", "%2:0")])?;
    assert!(
        alone.stderr.starts_with("fux: stray_pane: "),
        "{}",
        alone.stderr
    );
    let version = server.version("%1")?;
    let run = version.rsplit(':').next().unwrap_or_default();
    let env = [("FUX_PANE", "%2:0"), ("FUX_SERVER", run)];
    let out = server.fux_env(&["here", "vertical", "split"], &env)?;
    assert_eq!(
        (out.status, out.stdout.split(':').next()),
        (0, Some("%3")),
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
    server.ok(&[
        "opts",
        "80",
        "24",
        "size",
        "str",
        "other",
        "name",
        "new-workspace",
    ])?;
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
    server.ok(&["str", "%1", "pane", "str", "%2", "pane", "swap-with"])?;
    let ls = server.ok(&["ls"])?;
    let at = |needle: &str| ls.find(needle).unwrap_or(usize::MAX);
    assert!(at("%2") < at("+2") && at("%1") > at("+2"), "{ls}");
    // Swap toward a neighbour uses the same atomic backend as a counterpart.
    server.ok(&["str", "%2", "pane", "horizontal", "split"])?;
    server.ok(&["%2", "str", "right", "swap-with"])?;
    assert_eq!(server.ok(&["%2", "tab", "panes"])?.trim(), "%3 %2");
    server.ok(&["%2", "%3", "swap-with"])?;
    let refused = server.fux(&["%2", "str", "new-tabb", "move"])?;
    assert_eq!(refused.status, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("\"new-tabb\""),
        "{}",
        refused.stderr
    );
    assert!(
        refused.stderr.contains("new-workspace"),
        "{}",
        refused.stderr
    );
    // Move to a workspace, then to a new tab.
    server.ok(&["str", "%3", "pane", "str", "+2", "workspace", "move"])?;
    server.ok(&["str", "%3", "pane", "str", "new-tab", "move"])?;
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("@3 %3"), "{ls}");
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
    server.ok(&[
        "opts",
        "80",
        "24",
        "size",
        "str",
        "third",
        "name",
        "new-workspace",
    ])?;
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
    server.ok(&[
        "str",
        "%1",
        "pane",
        "str",
        "sleep 600; echo after-sleep",
        "text",
        "str",
        "Enter",
        "key",
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
    assert_eq!(server.ok(&["buffers"])?.trim(), "[]");
    assert_eq!(server.fux(&["newest", "buffer"])?.status, 1);
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
    let path = server.fux(&[
        "agent-opts",
        "80",
        "24",
        "size",
        "str",
        "no\nsuch",
        "program",
        "run",
    ])?;
    assert!(
        path.stderr.starts_with("fux: start: ") && path.stderr.contains(r"starting no\nsuch in "),
        "{}",
        path.stderr
    );
    let separator = server.fux(&["str", "a\u{2028}b", "fail"])?;
    assert!(
        separator.stderr.contains(r"a\u{2028}b"),
        "{}",
        separator.stderr
    );
    // A pane that never changes is waited on until the time is up.
    let timed_out = server.fux(&[
        "agent-opts",
        "80",
        "24",
        "size",
        "str",
        "sleep",
        "program",
        "str",
        "30",
        "arg",
        "run",
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
    // What the words before a failing wait left is printed first, then its
    // pane's answer, and why on one line of stderr.
    let failed = server.fux(&["str", "before", "%1", "1", "timeout", "wait"])?;
    assert_eq!(failed.status, 124, "{}", failed.stderr);
    assert!(
        failed.stdout.starts_with("before\n%1:") && failed.stdout.ends_with(" timeout\n"),
        "{}",
        failed.stdout
    );
    assert_eq!(failed.stderr.lines().count(), 1, "{}", failed.stderr);
    // A try naming the timeout goes on after it: the wait's answer goes
    // with the body's values, its code is left.
    let caught = server.fux(&[
        "str", "timeout", "try", "%1", "1", "timeout", "wait", "catch", "nip", "end", "str",
        "after",
    ])?;
    assert_eq!(caught.status, 0, "{}", caught.stderr);
    assert_eq!(caught.stdout, "timeout\nafter\n");
    let left = server.ok(&[
        "1", "2", "add", "str", "a b", "str", "%1", "pane", "dup", "tab",
    ])?;
    assert_eq!(left, "3\na b\n%1\n@1\n");
    let record = server.ok(&["str", "%1", "pane", "info", "json"])?;
    assert!(record.starts_with(r#"{"id":"%1","name":null"#), "{record}");
    assert_eq!(
        server.ok(&["str", "%1", "pane", "str", "exited", "get"])?,
        "null\n"
    );
    // An option a word would leave unused is refused, not ignored: a new
    // tab waits on nothing.
    {
        let unused = &["1", "timeout"][..];
        let refused =
            server.fux(&[&["opts"][..], unused, &["str", "w", "in", "new-tab"]].concat())?;
        assert!(
            refused.stderr.starts_with("fux: unused: "),
            "{}",
            refused.stderr
        );
    }
    // The language's codes fux's words give as they run are caught by
    // name: an option left unused, a word `explain` has not, a name a
    // target would read as an id.
    let program = "str unused try opts 1 timeout str w in new-tab drop str made catch nip end
        str unknown_word try str nosuch explain str shown catch nip end
        str bad_name try str %1 pane tab str %7 rename str renamed catch nip end";
    let caught = server.ok(&program.split_whitespace().collect::<Vec<_>>())?;
    assert_eq!(caught, "unused\nunknown_word\nbad_name\n");
    // A size past a client's screen's most is refused, saying the range
    // it may take, past a cell's too.
    for size in ["4097", "65536"] {
        let wide = server.fux(&["str", "%1", "pane", size, "10", "resize"])?;
        assert!(wide.stderr.starts_with("fux: range: "), "{}", wide.stderr);
        assert!(wide.stderr.contains("1 to 4096"), "{}", wide.stderr);
        let opened = server.fux(&["opts", size, "10", "size", "new-tab"])?;
        assert!(opened.stderr.contains("1 to 4096"), "{}", opened.stderr);
    }
    // A new tab from outside a pane is told the option that places it.
    let nowhere = server.fux(&["opts", "80", "24", "size", "str", "logs", "name", "new-tab"])?;
    assert!(
        nowhere.stderr.contains("`str NAME in`"),
        "{}",
        nowhere.stderr
    );
    let placed = [
        "opts", "80", "24", "size", "str", "logs", "name", "str", "agent", "in", "new-tab",
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
    // What its kinds can't show before it runs (a list's element) fails
    // as it runs, inside the word.
    let inside = said(&["list", "1", "end", "0", "at", "str", "x", "type"])?;
    assert!(
        inside.starts_with("fux: type: argument 8: type: inside type, text: "),
        "{inside}"
    );
    assert!(
        inside.ends_with("(use: PANE TEXT type, which leaves answer)\n"),
        "{inside}"
    );
    // Caught, it is only why: no place, no bundled step, no usage.
    let caught = server.ok(&[
        "str", "type", "try", "list", "1", "end", "0", "at", "str", "x", "type", "catch", "drop",
        "end",
    ])?;
    assert_eq!(
        caught,
        "takes a pane, tab, workspace or client, or options, not an integer\n"
    );
    // Refused before `run` starts anything.
    let before = said(&[
        "agent-opts",
        "80",
        "24",
        "size",
        "str",
        "sh",
        "program",
        "run",
        "str",
        "a",
        "1",
        "add",
    ])?;
    assert!(
        before.starts_with("fux: type: argument 12: add: "),
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
        "fux: unknown_word: argument 3: explain: no word or topic \"no-such-word\"; the topics: \
         agents recipes keys read shells programs servers guide limits (`fux help` lists every word)\n"
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
    let text = said(&["%1", "opts", "80", "24", "size", "text"])?;
    assert!(text.contains("a string, a number or a boolean"), "{text}");
    // A workspace never stands for its member, even when there is only one.
    let single = said(&["%1", "workspace", "pane"])?;
    assert!(single.contains("+1 is a workspace, not a pane"), "{single}");
    server.ok(&["%1", "horizontal", "split"])?;
    let many = said(&["%1", "workspace", "pane"])?;
    assert!(many.contains("+1 is a workspace, not a pane"), "{many}");
    Ok(())
}

/// `fux -` runs stdin, and `fux --file FILE` a file, whatever its name.
#[test]
fn programs_come_from_stdin_and_files() -> Outcome {
    let server = Server::start("")?;
    let program = "# the panes, doubled\ndefine twice dup add end\n21 twice str %1 pane\n";
    let out = server.piped(fux().as_ref(), &["-"], program)?;
    assert_eq!(out.stdout, "42\n%1\n");
    // What it reads is its program: `input` has nothing left to read.
    let input = server.piped(fux().as_ref(), &["-"], "input")?;
    assert!(input.stderr.starts_with("fux: read: "), "{}", input.stderr);
    for name in ["prog.fux", "prog"] {
        let file = server.dir.join(name);
        std::fs::write(&file, program).map_err(e)?;
        let path = file.to_string_lossy().into_owned();
        assert_eq!(server.ok(&["--file", &path])?, "42\n%1\n", "{path}");
    }
    let missing = server.fux(&["--file", "missing.fux"])?;
    assert_eq!(missing.status, 1);
    assert!(
        missing.stderr.starts_with("fux: read: "),
        "{}",
        missing.stderr
    );
    Ok(())
}

/// A script runs as a shell runs it: from its `#!` line, with its
/// arguments; it reads stdin, files and the environment (a variable not
/// set fails `unset_var`), warns on stderr, and ends with the status it
/// gives. A file given as a word is not run: the error says how.
#[test]
fn a_script_runs_as_the_shell_runs_it() -> Outcome {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::start("")?;
    let script = |name: &str, text: &str| {
        let file = server.dir.join(name);
        std::fs::write(&file, text).map_err(e)?;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
        Ok::<_, String>(file)
    };
    let greet = script(
        "greet.fux",
        "#!/usr/bin/env -S fux --file\nargs each str 'hello ' swap cat print end\n",
    )?;
    let greeted = server.piped(&greet, &["ann", "bob"], "")?;
    assert_eq!(
        greeted.stdout, "hello ann\nhello bob\n",
        "{}",
        greeted.stderr
    );
    let note = script("note.txt", "noted\n")?;
    let shell = script(
        "shell.fux",
        "#!/usr/bin/env -S fux --file
        input trim print
        args 0 at read-file trim print
        str FUX_SOCKET getenv print
        str unset_var try str FUX_NO_SUCH_VAR getenv catch nip end print
        str 'to stderr' warn
        str left 3 exit
        str never print\n",
    )?;
    let ran = server.piped(&shell, &[&note.to_string_lossy()], "typed\n")?;
    let socket = server.socket.to_string_lossy();
    let said = format!("typed\nnoted\n{socket}\nunset_var\nleft\n");
    assert_eq!(
        (ran.status, ran.stdout, ran.stderr),
        (3, said, "to stderr\n".into())
    );
    let path = greet.to_string_lossy();
    let named = server.fux(&[&path, "ann"])?;
    assert_eq!(named.status, 1);
    let how = format!("{path} is a file, which `fux --file {path}` runs");
    assert!(named.stderr.contains(&how), "{}", named.stderr);
    assert!(named.stdout.is_empty(), "{}", named.stdout);
    Ok(())
}

#[test]
fn ls_json_list_keys_and_help() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 50)?;
    client.wait_for("$")?;
    let json = server.ok(&["ls", "json"])?;
    for field in [
        "{\"workspaces\":[{\"id\":\"+1\",\"name\":\"main\",\"label\":\"main\",\"tabs\":[{\"id\":\"@1\",\"name\":null,\"label\":\"%1\",\"panes\":[{\"id\":\"%1\",\"name\":null,\"label\":\"%1\",\"rows\":9,\"cols\":50,\"pid\":",
        "\"clients\":[{\"id\":\"c1\",\"rows\":10,\"cols\":50,\"workspace\":\"+1\",\"tab\":\"@1\",\"pane\":\"%1\",\"zoom\":false}],\"events\":\"",
    ] {
        assert!(json.contains(field), "{field}\nin\n{json}");
    }
    // A replay request stays distinguishable from the bare session scope.
    assert_eq!(
        server.ok(&["all print all str 0:1-2 from print"])?,
        "all\n(a whole-session events request)\n"
    );
    assert_eq!(
        server.ok(&["list all all str 0:1-2 from end json"])?,
        "[\"all\",\"(a whole-session events request)\"]\n"
    );
    // The command line's own keymap: the defaults.
    let keys = server.ok(&["list-keys"])?;
    assert!(keys.contains("\nstr C-b prefix\n"), "{keys}");
    // help lists every word.
    let help = server.ok(&["help"])?;
    for word in fux::script::Word::ALL.iter().map(|w| w.row().name) {
        assert!(
            help.lines()
                .any(|l| l.split_whitespace().next() == Some(word)),
            "{word} is not in help"
        );
    }
    Ok(())
}

/// A caller in a directory whose name is not UTF-8 starts what it runs
/// there: the path's bytes reach the server as they are, not made text.
/// Linux only: macOS's file systems refuse a name that is not UTF-8.
#[cfg(target_os = "linux")]
#[test]
fn a_directory_named_in_any_bytes_is_where_a_run_starts() -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let server = Server::start("")?;
    let dir = server.dir.join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    std::fs::create_dir(&dir).map_err(e)?;
    let out = std::process::Command::new(fux())
        .args([
            "agent-opts",
            "80",
            "24",
            "size",
            "str",
            "touch",
            "program",
            "str",
            "here",
            "arg",
            "run",
        ])
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

/// An argument that is not UTF-8 text is refused as a usage error, said,
/// not a panic.
#[test]
fn an_argument_that_is_not_text_is_refused() -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let out = std::process::Command::new(fux())
        .arg("str")
        .arg(std::ffi::OsStr::from_bytes(b"caf\xe9"))
        .output()
        .map_err(e)?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("fux: usage: "), "{stderr}");
    Ok(())
}

/// Output that cannot be written fails where it is written, with
/// `output`: what comes after it never runs, and a stream ends.
#[cfg(target_os = "linux")]
#[test]
fn a_write_that_fails_stops_the_program() -> Outcome {
    let server = Server::start("")?;
    let full = || -> Result<std::process::Command, String> {
        let full = std::fs::OpenOptions::new().write(true).open("/dev/full");
        let mut command = std::process::Command::new(fux());
        command
            .env("FUX_SOCKET", &server.socket)
            .env_remove("FUX_PANE")
            .stdout(full.map_err(e)?)
            .stderr(std::process::Stdio::piped());
        Ok(command)
    };
    let ran = full()?
        .args([
            "help",
            "agent-opts",
            "80",
            "24",
            "size",
            "str",
            "sleep",
            "program",
            "str",
            "9",
            "arg",
            "run",
        ])
        .output()
        .map_err(e)?;
    let said = String::from_utf8_lossy(&ran.stderr);
    assert_eq!(ran.status.code(), Some(1), "{said}");
    assert!(said.starts_with("fux: output: "), "{said}");
    assert!(
        !server.ok(&["ls"])?.contains("%2"),
        "run ran after help failed"
    );
    let mut events = full()?.args(["all", "events"]).spawn().map_err(e)?;
    eventually("events to end", || {
        Ok(events.try_wait().map_err(e)?.is_some())
    })?;
    Ok(())
}

/// A stop between requests ends the run, not the connection: the next
/// run's request is asked on it, as one after a stop under way is asked on
/// a new one.
#[test]
fn a_stop_between_requests_leaves_the_connection_asking() -> Outcome {
    use fux::script::Asks;
    let server = Server::start("")?;
    let path = fux::socket::Choice::Path(server.socket.clone());
    let stop = fux::script::Stop::default();
    let mut asks = fux::script::Server::stoppable(path, stop.clone());
    let ls = fux::protocol::Asked {
        after: None,
        request: fux::command::Command::Ls {
            target: None,
            get: None,
        }
        .into(),
        context: fux::protocol::Context::default(),
    };
    let mut sink = std::io::sink();
    asks.ask(&ls, &mut sink).map_err(|f| f.to_string())?;
    stop.stop();
    stop.reset();
    asks.ask(&ls, &mut sink).map_err(|f| f.to_string())?;
    Ok(())
}
