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
    server.ok(&["str", "%1", "pane", "str", "horizontal", "axis", "split"])?;
    // Without FUX_PANE there is no `here`.
    let out = server.fux(&["here", "str", "horizontal", "axis", "split"])?;
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
    let out = server.fux_env(&["here", "str", "vertical", "axis", "split"], &env)?;
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
    let ls = server.ok(&["ls", "listing"])?;
    assert!(
        ls.contains("+1 work2") && ls.contains("@1 code") && ls.contains("%1 editor"),
        "{ls}"
    );
    server.words("opts 80 24 size str other name new-workspace")?;
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
    let ls = server.ok(&["ls", "listing"])?;
    let at = |needle: &str| ls.find(needle).unwrap_or(usize::MAX);
    assert!(at("%2") < at("+2") && at("%1") > at("+2"), "{ls}");
    // Swap toward a neighbour uses the same atomic backend as a counterpart.
    server.ok(&["str", "%2", "pane", "str", "horizontal", "axis", "split"])?;
    server.ok(&["%2", "str", "right", "swap-with"])?;
    assert_eq!(server.ok(&["%2", "tab", "panes"])?, "%3\n%2\n");
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
    let ls = server.ok(&["ls", "listing"])?;
    assert!(ls.contains("@3 %3"), "{ls}");
    // Reorder tabs and workspaces.
    server.ok(&["str", "@3", "tab", "str", "previous", "reorder"])?;
    let ls = server.ok(&["ls", "listing"])?;
    assert!(
        ls.find("@3").unwrap_or(0) < ls.find("@2").unwrap_or(0),
        "{ls}"
    );
    server.ok(&["str", "+2", "workspace", "str", "previous", "reorder"])?;
    assert!(server.ok(&["ls", "listing"])?.starts_with("+2 other"));
    let end = server.fux(&["str", "+2", "workspace", "str", "previous", "reorder"])?;
    assert!(end.stderr.contains("already at that end"), "{}", end.stderr);
    // Kill a pane (its tab, now empty of panes, goes with it), a workspace,
    // and a tab (the workspace's last, so the workspace goes too).
    server.ok(&["str", "%3", "pane", "kill"])?;
    assert!(!server.ok(&["ls", "listing"])?.contains("@3 "));
    server.ok(&["str", "work2", "workspace", "kill"])?;
    let ls = server.ok(&["ls", "listing"])?;
    assert!(!ls.contains("work2") && ls.contains("other"), "{ls}");
    server.words("opts 80 24 size str third name new-workspace")?;
    server.ok(&["str", "@2", "tab", "kill"])?;
    let ls = server.ok(&["ls", "listing"])?;
    assert!(!ls.contains("other") && ls.contains("third"), "{ls}");
    Ok(())
}

#[test]
fn send_keys_and_signal() -> Outcome {
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
    assert!(
        server.ok(&["ls", "listing"])?.contains("%1"),
        "the shell survives"
    );
    Ok(())
}

/// A word's error names its code and exits 2 for a mistake in the program;
/// a wait's failure keeps its status, its stdout empty; what is left on the
/// stack is printed, a value a line, bottom first; `json` turns an answer
/// into JSON.
#[test]
fn errors_statuses_and_what_is_left() -> Outcome {
    let server = Server::start("")?;
    let typed = server.fux(&["3", "pane"])?;
    assert_eq!(typed.status, 2);
    assert!(typed.stderr.starts_with("fux: type: "), "{}", typed.stderr);
    let unknown = server.fux(&["frobnicate"])?;
    assert_eq!(unknown.status, 2);
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
        "new-tab",
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
        "new-tab",
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
    // A failing wait prints nothing: on stderr, why, then what the words
    // before it left and its pane's answer.
    let failed = server.fux(&["str", "before", "%1", "1", "timeout", "wait"])?;
    assert_eq!(failed.status, 124, "{}", failed.stderr);
    assert_eq!(failed.stdout, "");
    let told: Vec<_> = failed.stderr.lines().collect();
    assert!(
        matches!(&told[..], [why, "before", answer]
            if why.starts_with("fux: timeout: ") && answer.starts_with("%1:") && answer.ends_with(" timeout")),
        "{}",
        failed.stderr
    );
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
    assert!(
        record.contains(r#""panes":[{"id":"%1","tab":"@1","name":null"#),
        "{record}"
    );
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
    // A size no screen can have is refused, `size` as the server refuses
    // it: past what a grid holds, or past a cell's range.
    for (cols, rows) in [("65535", "65535"), ("65536", "10")] {
        let wide = server.fux(&["str", "%1", "pane", cols, rows, "resize"])?;
        assert!(wide.stderr.starts_with("fux: size: "), "{}", wide.stderr);
        assert!(
            wide.stderr.contains("no screen is that size"),
            "{}",
            wide.stderr
        );
        let opened = server.fux(&["opts", cols, rows, "size", "new-tab"])?;
        assert!(
            opened.stderr.contains("no screen is that size"),
            "{}",
            opened.stderr
        );
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
/// bundled word by the step that called it; a word given what it doesn't
/// take, as it runs, after the words before it acted; a name that would
/// always be a handle, refused.
#[test]
fn mistakes_name_what_the_program_wrote() -> Outcome {
    let server = Server::start("")?;
    // Each fails with its code's status: 2 for a mistake in the program.
    let said = |args: &[&str]| -> Result<String, String> {
        let ran = server.fux(args)?;
        let code = (ran.stderr.strip_prefix("fux: "))
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(code, _)| code.parse::<fux_wire::code::Code>().ok())
            .ok_or_else(|| format!("{args:?}: {}", ran.stderr))?;
        assert_eq!(
            ran.status,
            i32::from(code.status().get()),
            "{args:?}: {}",
            ran.stdout
        );
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
    let caught = server.words("str type try list 1 end 0 at str x type catch drop end")?;
    assert_eq!(
        caught,
        "takes the pane it goes on, under its own values (or a tab, workspace, client, a request on one, or opts), not an integer\n"
    );
    // `add` fails as it runs, after `new-tab` made its tab (D-25).
    let before = said(&[
        "agent-opts",
        "80",
        "24",
        "size",
        "str",
        "sh",
        "program",
        "new-tab",
        "str",
        "a",
        "1",
        "add",
    ])?;
    assert!(
        before.starts_with("fux: type: argument 12: add: "),
        "{before}"
    );
    assert!(
        server.ok(&["ls", "listing"])?.contains("agent"),
        "new-tab ran before add failed"
    );
    // The language's own words say how they are written.
    for (words, usage) in [
        (
            &["str", "a", "if", "1", "then"][..],
            "(use: flag if … else … then)",
        ),
        (&["3", "each", "1", "end"][..], "(use: seq each … end)"),
        (&["let", "X"][..], "(use: x let NAME)"),
        (
            &["begin", "str", "x", "until"][..],
            "(use: begin … flag until)",
        ),
    ] {
        let wrong = said(words)?;
        assert!(wrong.ends_with(&format!("{usage}\n")), "{wrong}");
    }
    // `help WORD` of no word says so, as help, and where, as any error.
    assert_eq!(
        said(&["help", "no-such-word"])?,
        "fux: unknown_word: argument 3: explain: no word or topic \"no-such-word\"; the topics: \
         agents recipes keys read shells programs servers guide cookbook (`fux help` lists every word)\n"
    );
    let named = said(&["1", "let", "+7"])?;
    assert!(
        named.contains("cannot be defined: it is read as `str +7 workspace`"),
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
    server.ok(&["%1", "str", "horizontal", "axis", "split"])?;
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
    assert_eq!(named.status, 2);
    assert!(named.stderr.contains("unknown_word"), "{}", named.stderr);
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
        "\"tabs\":[\"@1\"]",
        "\"workspace\":\"+1\"",
        "\"tab\":\"@1\"",
        "\"panes\":[{\"id\":\"%1\"",
        "\"rows\":9,\"cols\":50",
        "\"cursor\":",
        r#""host":"c1""#,
        r#""terminal":{"sixel_reported":null,"cell_reported":null,"underline_styles_reported":null,"kitty_flags_reported":null}"#,
        r#""output":{"graphics":"text","underline_styles":false,"clipboard":"osc52","kitty_flags_requested":null}"#,
        "\"clients\":[{\"id\":\"c1\"",
        "\"pane_rows\":9,\"pane_cols\":50",
    ] {
        assert!(json.contains(field), "{field} in {json}");
    }
    let revision = server.ok(&["@1", "str", "layout_revision", "get"])?;
    let parsed: fux_wire::command::LayoutRevision =
        revision.trim().parse().map_err(|()| "layout revision")?;
    assert_eq!(parsed.tab.to_string(), "@1");
    assert!(
        json.contains(&format!("\"layout_revision\":\"{parsed}\"")),
        "{json}"
    );
    assert_eq!(
        server.ok(&["all print list all end json"])?,
        "all\n[\"all\"]\n"
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
            "new-tab",
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
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.starts_with("fux: usage: "), "{stderr}");
    Ok(())
}

/// `write` and `emit` deliver their text as they run, with no line end:
/// a reader has it while the program is still running.
#[test]
fn write_and_emit_deliver_as_they_run() -> Outcome {
    use std::io::Read;
    for (words, said) in [(&["str", "x", "write"][..], b"x"), (&["120", "emit"], b"x")] {
        let mut child = std::process::Command::new(fux())
            .args(words)
            .args(["begin", "false", "until"])
            .env_remove("FUX_SOCKET")
            .env_remove("FUX_PANE")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(e)?;
        let mut stdout = child.stdout.take().ok_or("no stdout")?;
        let reader = std::thread::spawn(move || {
            let mut got = [0u8; 1];
            stdout.read_exact(&mut got).map(|()| got)
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !reader.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let finished = reader.is_finished();
        child.kill().map_err(e)?;
        child.wait().map_err(e)?;
        assert!(finished, "{words:?}: nothing delivered while it ran");
        let got = reader.join().map_err(|_| "the reader failed")?.map_err(e)?;
        assert_eq!(&got, said, "{words:?}");
    }
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
            "new-tab",
        ])
        .output()
        .map_err(e)?;
    let said = String::from_utf8_lossy(&ran.stderr);
    assert_eq!(ran.status.code(), Some(1), "{said}");
    assert!(said.starts_with("fux: output: "), "{said}");
    assert!(
        !server.ok(&["ls", "listing"])?.contains("%2"),
        "new-tab ran after help failed"
    );
    let cursor = server.ok(&["all", "str", "", "0", "next-events", "str", "next", "get"])?;
    server.ok(&["%1", "str", "told", "rename"])?;
    let mut events = (full()?.args(["all", "str", cursor.trim(), "events"]))
        .spawn()
        .map_err(e)?;
    eventually("events to end", || {
        Ok(events.try_wait().map_err(e)?.is_some())
    })?;
    Ok(())
}

/// A stdout whose reader goes (`| head -1`) ends fux quietly, with 141 as
/// a shell gives `cat` that SIGPIPE ended: a stream of events waiting for
/// the next, as a program printing.
#[test]
fn a_closed_pipe_ends_fux_quietly() -> Outcome {
    use std::io::{BufRead, BufReader};
    let server = Server::start("")?;
    let cursor = server.ok(&["all", "str", "", "0", "next-events", "str", "next", "get"])?;
    server.ok(&["%1", "str", "told", "rename"])?;
    let printing = "0 begin str line print 1 add dup 1000000 eq until";
    for args in [&["all", "str", cursor.trim(), "events"][..], &[printing]] {
        let mut fux = std::process::Command::new(fux())
            .args(args)
            .env("FUX_SOCKET", &server.socket)
            .env_remove("FUX_PANE")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(e)?;
        let mut first = String::new();
        let stdout = fux.stdout.take().ok_or("no stdout")?;
        BufReader::new(stdout).read_line(&mut first).map_err(e)?;
        eventually("fux to end", || Ok(fux.try_wait().map_err(e)?.is_some()))?;
        let ended = fux.wait_with_output().map_err(e)?;
        assert!(!first.is_empty(), "{args:?}");
        assert_eq!(ended.status.code(), Some(141), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&ended.stderr), "", "{args:?}");
    }
    Ok(())
}

/// An action acts though stdout's reader is already gone: the closed pipe
/// is met as its answer is written (141), never before it asks. A wait or
/// stream meets it as it waits, caught or not.
#[test]
fn an_action_acts_though_stdout_is_closed() -> Outcome {
    let server = Server::start("")?;
    let closed = |args: &[&str]| -> Result<std::process::Output, String> {
        let (reader, writer) = std::io::pipe().map_err(e)?;
        drop(reader);
        std::process::Command::new(fux())
            .args(args)
            .env("FUX_SOCKET", &server.socket)
            .env_remove("FUX_PANE")
            .stdout(writer)
            .output()
            .map_err(e)
    };
    let renamed = closed(&["%1", "str", "renamed", "rename"])?;
    assert_eq!(renamed.status.code(), Some(0));
    assert_eq!(server.ok(&["str", "renamed", "pane"])?, "%1\n");
    let launch = [
        "agent-opts",
        "80",
        "24",
        "size",
        "str",
        "sh",
        "program",
        "new-tab",
    ];
    let made = closed(&launch)?;
    assert_eq!(made.status.code(), Some(141));
    assert_eq!(
        server.ok(&["str", "agent", "workspace", "panes", "len"])?,
        "1\n"
    );
    let caught = "str output try all str '' events catch 2drop end";
    let stream = closed(&[caught])?;
    assert_eq!(stream.status.code(), Some(141));
    assert_eq!(String::from_utf8_lossy(&stream.stderr), "");
    Ok(())
}

/// A batch `next-events` leaves prints as its events, a JSON object a line:
/// nothing when none came; `json` gives all of it.
#[test]
fn a_batch_left_prints_its_events() -> Outcome {
    let server = Server::start("")?;
    let idle = server.fux(&["all", "str", "", "0", "next-events"])?;
    assert_eq!((idle.status, idle.stdout.as_str()), (0, ""));
    let cursor = server.ok(&["all", "str", "", "0", "next-events", "str", "next", "get"])?;
    server.ok(&["%1", "str", "told", "rename"])?;
    let read = ["all", "str", cursor.trim(), "0", "next-events"];
    let events = server.ok(&read)?;
    assert!(
        matches!(&events.lines().collect::<Vec<_>>()[..], [one]
            if one.starts_with(r#"{"event":"renamed""#) && one.contains(r#""next":""#)),
        "{events}"
    );
    let whole = server.ok(&[&read[..], &["json"]].concat())?;
    assert!(whole.starts_with(r#"{"events":[{"#) && whole.contains(r#""missed":0"#));
    Ok(())
}

/// A failure's report tells every value left, a batch that missed entries
/// whole among them, and stdout nothing: the action after the batch is told.
#[test]
fn a_failure_tells_what_follows_a_lost_batch() -> Outcome {
    let server = Server::start_journal(2)?;
    let cursor = server.ok(&["all", "str", "", "0", "next-events", "str", "next", "get"])?;
    for name in ["a", "b", "c", "d"] {
        server.ok(&["%1", "str", name, "rename"])?;
    }
    let lost = ["all", "str", cursor.trim(), "0", "next-events"];
    let keys = ["%1", "str", "x", "text", "send-keys", "str", "boom", "fail"];
    let failed = server.fux(&[&lost[..], &keys].concat())?;
    assert_eq!(failed.status, 1, "{}", failed.stderr);
    assert_eq!(failed.stdout, "");
    let told: Vec<_> = failed.stderr.lines().collect();
    assert!(
        matches!(&told[..], [why, batch, answer]
            if why.starts_with("fux: fail: ") && batch.contains(r#""missed":2"#)
                && answer.starts_with("%1:")),
        "{}",
        failed.stderr
    );
    Ok(())
}

/// A stop between requests ends the run, not the connection: the next
/// run's request is asked on it, as one after a stop under way is asked on
/// a new one.
#[test]
fn a_stop_between_requests_leaves_the_connection_asking() -> Outcome {
    use fux::script::Asks;
    let server = Server::start("")?;
    let path = fux_wire::protocol::Choice::Path(server.socket.clone());
    let stop = fux::script::Stop::default();
    let mut asks = fux::script::Server::stoppable(path, stop.clone());
    let ls = fux_wire::protocol::Asked {
        after: None,
        request: fux_wire::command::Command::Inspect {
            scope: fux_wire::inspect::Scope::All,
            projection: fux_wire::inspect::Projection::Facts,
        }
        .into(),
        context: fux_wire::protocol::Context::default(),
    };
    asks.ask(&ls).map_err(|f| f.to_string())?;
    stop.stop();
    stop.reset();
    asks.ask(&ls).map_err(|f| f.to_string())?;
    Ok(())
}
