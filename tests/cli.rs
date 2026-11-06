//! The command line: a fuxth program, its words' errors and statuses, what
//! it leaves printed, `json`, and programs from stdin and files.
// They script `/bin/sh` over a Unix socket (WINDOWS.md, "What PR 8 found").
#![cfg(unix)]
mod support;
use support::*;

/// Waits until `%N`'s screen contains `needle`.
fn shows(server: &Server, pane: &str, needle: &str) -> Outcome {
    eventually(&format!("{needle:?} in {pane}"), || {
        Ok(server
            .ok(&[&format!("(capture (pane {}))", fux::script::quoted(pane))])?
            .contains(needle))
    })
}

#[test]
fn targets_are_named_or_here_and_never_guessed() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["(split (axis (pane \"%1\") \"horizontal\"))"])?;
    // Without FUX_PANE there is no `here`.
    let out = server.fux(&["(split (axis (here) \"horizontal\"))"])?;
    assert_eq!(out.status, 1);
    assert!(out.stderr.starts_with("fux: no-target: "), "{}", out.stderr);
    // FUX_PANE, of the server FUX_SERVER says, is the caller's pane, and
    // its tab and workspace; with no FUX_SERVER, no one's.
    let alone = server.fux_env(
        &["(sequence! (kill (here)) #inert)"],
        &[("FUX_PANE", "%2:0")],
    )?;
    assert!(
        alone.stderr.starts_with("fux: stray-pane: "),
        "{}",
        alone.stderr
    );
    let version = server.version("%1")?;
    let run = version.rsplit(':').next().unwrap_or_default();
    let env = [("FUX_PANE", "%2:0"), ("FUX_SERVER", run)];
    let out = server.fux_env(&["(split (axis (here) \"vertical\"))"], &env)?;
    assert_eq!(
        (out.status, out.stdout.split(':').next()),
        (0, Some("%3")),
        "{}",
        out.stderr
    );
    let out = server.fux_env(
        &["(sequence! (define! value-505 (here)) (list (tab value-505) (workspace value-505)))"],
        &env,
    )?;
    assert_eq!(out.stdout, "@1\n+1\n");
    // Missing targets, each named.
    let gone = server.fux(&["(sequence! (kill (tab \"@9\")) #inert)"])?;
    assert_eq!(gone.status, 1);
    assert!(
        gone.stderr.starts_with("fux: no-tab: ")
            && gone.stderr.contains("@9")
            && gone.stderr.contains("command line:1"),
        "{}",
        gone.stderr
    );
    let named = server.fux(&["(sequence! (kill (workspace \"nope\")) #inert)"])?;
    assert!(
        named.stderr.contains("no workspace named") && named.stderr.contains("nope"),
        "{}",
        named.stderr
    );
    Ok(())
}

#[test]
fn rename_move_swap_reorder_and_kill() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["(sequence! (rename (pane \"%1\") \"editor\") #inert)"])?;
    server.ok(&["(sequence! (rename (tab \"@1\") \"code\") #inert)"])?;
    server.ok(&["(sequence! (rename (workspace \"+1\") \"work\") #inert)"])?;
    server.ok(&["(sequence! (rename (workspace \"work\") \"work2\") #inert)"])?;
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(
        ls.contains("+1 work2") && ls.contains("@1 code") && ls.contains("%1 editor"),
        "{ls}"
    );
    server.words("(new-tab (in (size (options) 80 24) \"other\"))")?;
    let clash = server.fux(&["(sequence! (rename (workspace \"other\") \"work2\") #inert)"])?;
    assert!(
        clash.stderr.contains("+1, a workspace, is named"),
        "{}",
        clash.stderr
    );
    assert_eq!(
        server
            .fux(&["(sequence! (rename (pane \"%1\") \"\") #inert)"])?
            .status,
        1
    );
    // Swap across workspaces.
    server.ok(&["(sequence! (swap-with (pane \"%1\") (pane \"%2\")) #inert)"])?;
    let ls = server.ok(&["(listing (list-session))"])?;
    let at = |needle: &str| ls.find(needle).unwrap_or(usize::MAX);
    assert!(at("%2") < at("+2") && at("%1") > at("+2"), "{ls}");
    // Swap toward a neighbour uses the same atomic backend as a counterpart.
    server.ok(&["(split (axis (pane \"%2\") \"horizontal\"))"])?;
    server.ok(&["(sequence! (swap-with (pane 2) \"right\") #inert)"])?;
    assert_eq!(server.ok(&["(panes (tab (pane 2)))"])?, "%3\n%2\n");
    server.ok(&["(sequence! (swap-with (pane 2) (pane 3)) #inert)"])?;
    let refused = server.fux(&["(sequence! (move (pane 2) \"new-tabb\") #inert)"])?;
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
    server.ok(&["(sequence! (move (pane \"%3\") (workspace \"+2\")) #inert)"])?;
    server.ok(&["(sequence! (move (pane \"%3\") \"new-tab\") #inert)"])?;
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(ls.contains("@3 %3"), "{ls}");
    // Reorder tabs and workspaces.
    server.ok(&["(sequence! (reorder (tab \"@3\") \"previous\") #inert)"])?;
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(
        ls.find("@3").unwrap_or(0) < ls.find("@2").unwrap_or(0),
        "{ls}"
    );
    server.ok(&["(sequence! (reorder (workspace \"+2\") \"previous\") #inert)"])?;
    assert!(
        server
            .ok(&["(listing (list-session))"])?
            .starts_with("+2 other")
    );
    let end = server.fux(&["(sequence! (reorder (workspace \"+2\") \"previous\") #inert)"])?;
    assert!(end.stderr.contains("already at that end"), "{}", end.stderr);
    // Kill a pane (its tab, now empty of panes, goes with it), a workspace,
    // and a tab (the workspace's last, so the workspace goes too).
    server.ok(&["(sequence! (kill (pane \"%3\")) #inert)"])?;
    assert!(!server.ok(&["(listing (list-session))"])?.contains("@3 "));
    server.ok(&["(sequence! (kill (workspace \"work2\")) #inert)"])?;
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(!ls.contains("work2") && ls.contains("other"), "{ls}");
    server.words("(new-tab (in (size (options) 80 24) \"third\"))")?;
    server.ok(&["(sequence! (kill (tab \"@2\")) #inert)"])?;
    let ls = server.ok(&["(listing (list-session))"])?;
    assert!(!ls.contains("other") && ls.contains("third"), "{ls}");
    Ok(())
}

#[test]
fn send_keys_and_signal() -> Outcome {
    let server = Server::start("")?;
    // send-keys types at once; after the prompt, so the output is a line
    // of its own however slowly the shell starts.
    shows(&server, "%1", "$")?;
    server.ok(&["(send-keys (text (text (pane \"%1\") \"echo lit\") \"eral\"))"])?;
    server.ok(&["(press (pane \"%1\") \"Enter\")"])?;
    shows(&server, "%1", "\nliteral")?;
    // signal stops what runs in the foreground, never the shell.
    let idle = server.fux(&["(signal (pane \"%1\") \"TERM\")"])?;
    assert!(
        idle.stderr.contains("nothing is running"),
        "{}",
        idle.stderr
    );
    server.ok(&[
        "(send-keys (key (text (pane \"%1\") \"sleep 600; echo after-sleep\") \"Enter\"))",
    ])?;
    eventually("sleep in the foreground", || {
        Ok(server.fux(&["(signal (pane \"%1\") \"TERM\")"])?.status == 0)
    })?;
    shows(&server, "%1", "after-sleep")?;
    assert!(
        server.ok(&["(listing (list-session))"])?.contains("%1"),
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
    let typed = server.fux(&["(pane (list 3))"])?;
    assert_eq!(typed.status, 2);
    assert!(
        typed.stderr.starts_with("fux: type-error: "),
        "{}",
        typed.stderr
    );
    let unknown = server.fux(&["frobnicate"])?;
    assert_eq!(unknown.status, 2);
    assert!(
        unknown.stderr.starts_with("fux: unbound-symbol: ")
            && unknown.stderr.contains("frobnicate"),
        "{}",
        unknown.stderr
    );
    // A program's path holding a line end fails to start, the line end
    // shown as `\n`, as is a line separator in a message.
    let path =
        server.fux(&["(new-tab (program (size (in (options) \"agent\") 80 24) \"no\\nsuch\"))"])?;
    assert!(
        path.stderr.starts_with("fux: start: ") && path.stderr.contains(r"starting no\nsuch in "),
        "{}",
        path.stderr
    );
    let separator = server.fux(&["(sequence! (raise (error (quote! fail) \"a b\")) #inert)"])?;
    assert!(
        separator.stderr.contains(r"a\u{2028}b"),
        "{}",
        separator.stderr
    );
    // A pane that never changes is waited on until the time is up.
    let timed_out = server.fux(&["(wait (timeout (new-tab (argument (program (size (in (options) \"agent\") 80 24) \"sleep\") \"30\")) 1))"])?;
    assert_eq!(timed_out.status, 124, "{}", timed_out.stderr);
    assert!(
        timed_out.stderr.starts_with("fux: timeout: "),
        "{}",
        timed_out.stderr
    );
    // A failing wait prints nothing: on stderr, why, then what the words
    // before it left and its pane's answer.
    let failed = server.fux(&["(list \"before\" (wait (timeout (pane 1) 1)))"])?;
    assert_eq!(failed.status, 124, "{}", failed.stderr);
    assert_eq!(failed.stdout, "");
    assert!(
        failed.stderr.contains("fux: timeout:") && failed.stderr.contains("%1:"),
        "{}",
        failed.stderr
    );
    let caught = server.fux(&[r#"(catch-kind! (wait (timeout (pane 1) 1)) timeout failure (list (symbol-to-string (get failure "kind")) "after"))"#])?;
    assert_eq!(caught.status, 0, "{}", caught.stderr);
    assert_eq!(caught.stdout, "timeout\nafter\n");
    let left = server.ok(&["(sequence! (define! value-704 (add 1 2)) (define! value-705 \"a b\") (define! value-707 (pane \"%1\")) (list value-704 value-705 value-707 (tab value-707)))"])?;
    assert_eq!(left, "3\na b\n%1\n@1\n");
    let record = server.ok(&["(information (pane \"%1\"))"])?;
    assert!(
        read_data(
            &record,
            "(field (first (field data (quote! panes))) (quote! id))"
        )? == "%1",
        "{record}"
    );
    assert_eq!(
        server.ok(&["(sequence! (define! value-715 (pane \"%1\")) \"program\" (define! value-717 (get value-715 \"program\")) \"exited\" (get value-717 \"exited\"))"])?,
        "none\n"
    );
    // An option a word would leave unused is refused, not ignored: a new
    // tab waits on nothing.
    let refused = server.fux(&[r#"(new-tab (in (timeout (options) 1) "w"))"#])?;
    assert!(
        refused.stderr.starts_with("fux: unused: "),
        "{}",
        refused.stderr
    );
    let program = r#"(list
      (catch-kind! (new-tab (in (timeout (options) 1) "w")) unused failure (symbol-to-string (get failure "kind")))
      (catch-kind! (explain "nosuch") unbound-symbol failure (symbol-to-string (get failure "kind")))
      (catch-kind! (rename (tab (pane 1)) "%7") bad-name failure (symbol-to-string (get failure "kind"))))"#;
    assert_eq!(server.ok(&[program])?, "unused\nunbound-symbol\nbad-name\n");
    // A size no screen can have is refused, `size` as the server refuses
    // it: past what a grid holds, or past a cell's range.
    for (cols, rows) in [("65535", "65535"), ("65536", "10")] {
        let wide = server.fux(&[&format!(
            "(sequence! (resize (pane \"%1\") {} {}) #inert)",
            support::reference(cols),
            support::reference(rows)
        )])?;
        assert!(wide.stderr.starts_with("fux: size: "), "{}", wide.stderr);
        assert!(
            wide.stderr.contains("no screen is that size"),
            "{}",
            wide.stderr
        );
        let opened = server.fux(&[&format!(
            "(new-tab (size (options) {} {}))",
            support::reference(cols),
            support::reference(rows)
        )])?;
        assert!(
            opened.stderr.contains("no screen is that size"),
            "{}",
            opened.stderr
        );
    }
    // A new tab from outside a pane is told the option that places it.
    let nowhere = server.fux(&["(new-tab (name (size (options) 80 24) \"logs\"))"])?;
    assert!(nowhere.stderr.contains("workspace"), "{}", nowhere.stderr);
    let placed = ["(new-tab (in (name (size (options) 80 24) \"logs\") \"agent\"))"];
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
    let said = |program: &str, kind: &str| -> Result<String, String> {
        let out = server.fux(&[program])?;
        assert_ne!(out.status, 0, "{program}");
        assert!(
            out.stderr.starts_with(&format!("fux: {kind}: ")),
            "{}",
            out.stderr
        );
        Ok(out.stderr)
    };
    let inside = said(r#"(type (first (list 1)) "x")"#, "type-error")?;
    assert!(
        inside.contains("integer") && inside.contains("(type") && inside.contains("command line:1"),
        "{inside}"
    );
    let rethrown = said(
        r#"(catch! (add "a" 1) failure (raise failure))"#,
        "type-error",
    )?;
    assert!(rethrown.contains(r#"(add "a" 1)"#), "{rethrown}");
    let caught = server.words(
        r#"(catch-kind! (type 1 "x") type-error failure (symbol-to-string (get failure "kind")))"#,
    )?;
    assert_eq!(caught, "type-error\n");
    let before = said(
        r#"(new-tab (program (size (in (options) "agent") 80 24) "sh")) (add "a" 1)"#,
        "type-error",
    )?;
    assert!(server.ok(&["(listing (list-session))"])?.contains("agent"));
    assert!(before.contains("already took effect: new-tab"), "{before}");
    let caught = server.words(r#"(catch! (sequence! (line (pane 1) "x") (divide 1 0)) failure (list (symbol-to-string (get failure "kind")) (length (actions))))"#)?;
    assert_eq!(caught, "division-by-zero\n1\n");
    let twice = server.words(r#"(catch! (sequence! (rename (pane 1) "first") (rename (pane 1) "second") (raise (error (quote! fail) "boom"))) failure (length (actions)))"#)?;
    assert_eq!(twice, "2\n");
    for (program, kind) in [
        ("(if! #true 1)", "arity-error"),
        ("(first 3)", "type-error"),
        ("(define!)", "arity-error"),
        ("(", "parse-error"),
    ] {
        said(program, kind)?;
    }
    let unknown = said(r#"(explain "no-such-word")"#, "unbound-symbol")?;
    assert!(unknown.contains("no-such-word"));
    let client = said(r#"(focused (client "c9"))"#, "no-client")?;
    assert!(client.contains("c9"));
    let way = said(r#"(step (client "c9") (pane 1) "sideways")"#, "bad-value")?;
    assert!(way.contains("up, down, next, previous or last"), "{way}");
    let single = said("(pane (workspace (pane 1)))", "no-target")?;
    assert!(single.contains("workspace") && single.contains("pane"));
    server.ok(&[r#"(split (axis (pane 1) "horizontal"))"#])?;
    let many = said("(pane (workspace (pane 1)))", "no-target")?;
    assert!(many.contains("workspace") && many.contains("pane"));
    Ok(())
}

/// `fux -` runs stdin, and `fux --file FILE` a file, whatever its name.
#[test]
fn programs_come_from_stdin_and_files() -> Outcome {
    let server = Server::start("")?;
    let program = "(sequence! (define! twice (lambda! (argument-185) (add argument-185 argument-185))) (list (twice 21) (pane \"%1\")))";
    let out = server.piped(fux().as_ref(), &["-"], program)?;
    assert_eq!(out.stdout, "42\n%1\n");
    // What it reads is its program: `input` has nothing left to read.
    let input = server.piped(fux().as_ref(), &["-"], "(input)")?;
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
        "greet.grift",
        "#!/usr/bin/env -S fux --file\n(sequence! (fold (lambda! (accumulator-201 item-196) (let! ((() accumulator-201)) (sequence! (print (string-append \"hello \" item-196)) (list)))) (list) (arguments)) #inert)",
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
        "#!/usr/bin/env -S fux --file\n(print (string-trim (input)))\n(print (string-trim (read-file (first (arguments)))))\n(print (environment-variable \"FUX_SOCKET\"))\n(print (catch-kind! (environment-variable \"FUX_NO_SUCH_VAR\") unset-variable failure (symbol-to-string (get failure \"kind\"))))\n(warn \"to stderr\")\n(print \"left\") (exit 3)\n(print \"never\")\n",
    )?;
    let ran = server.piped(&shell, &[&note.to_string_lossy()], "typed\n")?;
    let socket = server.socket.to_string_lossy();
    let said = format!("typed\nnoted\n{socket}\nunset-variable\nleft\n");
    assert_eq!(
        (ran.status, ran.stdout, ran.stderr),
        (3, said, "to stderr\n".into())
    );
    let path = greet.to_string_lossy();
    let named = server.fux(&[&path, "ann"])?;
    assert_eq!(named.status, 2);
    assert!(named.stderr.contains("unbound-symbol"), "{}", named.stderr);
    assert!(named.stdout.is_empty(), "{}", named.stdout);
    Ok(())
}

#[test]
fn ls_json_list_keys_and_help() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 50)?;
    client.wait_for("$")?;
    let data = server.ok(&["(list-session)"])?;
    for (query, expected) in [
        (
            "(first (field (first (field data (quote! workspaces))) (quote! children)))",
            "@1",
        ),
        (
            "(field (first (field data (quote! tabs))) (quote! workspace))",
            "+1",
        ),
        (
            "(field (first (field data (quote! panes))) (quote! id))",
            "%1",
        ),
        (
            "(field (first (field data (quote! panes))) (quote! tab))",
            "@1",
        ),
        (
            "(field (first (field data (quote! panes))) (quote! rows))",
            "10",
        ),
        (
            "(field (first (field data (quote! panes))) (quote! columns))",
            "50",
        ),
        (
            "(field (first (field data (quote! tabs))) (quote! host))",
            "c1",
        ),
        (
            "(field (first (field data (quote! clients))) (quote! id))",
            "c1",
        ),
        (
            "(field (first (field data (quote! clients))) (quote! pane-rows))",
            "10",
        ),
        (
            "(field (first (field data (quote! clients))) (quote! pane-columns))",
            "50",
        ),
    ] {
        assert_eq!(read_data(&data, query)?, expected, "{query}");
    }
    assert!(!read_data(&data, "(field data (quote! cursor))")?.is_empty());
    for field in [
        "sixel-reported",
        "cell-reported",
        "underline-styles-reported",
        "kitty-flags-reported",
    ] {
        assert_eq!(
            read_data(
                &data,
                &format!(
                    "(field (field (first (field data (quote! clients))) (quote! terminal)) (quote! {field}))"
                )
            )?,
            "none"
        );
    }
    for (field, expected) in [
        ("graphics", "text"),
        ("underline-styles", "#false"),
        ("clipboard", "osc52"),
        ("kitty-flags-requested", "none"),
    ] {
        assert_eq!(
            read_data(
                &data,
                &format!(
                    "(field (field (first (field data (quote! clients))) (quote! output)) (quote! {field}))"
                )
            )?,
            expected
        );
    }
    let revision = server.ok(&[r#"(get (tab 1) "layout-revision")"#])?;
    let parsed: fux_wire::command::LayoutRevision =
        revision.trim().parse().map_err(|()| "layout revision")?;
    assert_eq!(parsed.tab.to_string(), "@1");
    assert_eq!(
        read_data(
            &data,
            "(field (first (field data (quote! tabs))) (quote! layout-revision))"
        )?,
        parsed.to_string()
    );
    assert_eq!(
        server.ok(&[r#"(print (all)) (display (data-text (list (all))) "\n")"#])?,
        "all\n(%all)\n"
    );
    let keys = server.ok(&["(list-keys)"])?;
    assert!(
        keys.contains(r#"(bind-key "C-b d""#) && keys.contains("(detach (me))"),
        "{keys}"
    );
    let help = server.ok(&["(help)"])?;
    for word in fux::script::Word::ALL.iter().map(|w| w.row().name) {
        assert!(
            help.lines()
                .any(|line| line.split_whitespace().next() == Some(word)),
            "{word}"
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
        .args(["(new-tab (argument (program (size (in (options) \"agent\") 80 24) \"touch\") \"here\"))"])
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
    for text in [r#"(display "x")"#, "(display (code-point-to-string 120))"] {
        let program = format!("{text} (define! forever (lambda! () (forever))) (forever)");
        let mut child = std::process::Command::new(fux())
            .arg(&program)
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
        let deadline = after(std::time::Duration::from_secs(10));
        while !reader.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let finished = reader.is_finished();
        child.kill().map_err(e)?;
        child.wait().map_err(e)?;
        assert!(finished, "{program}: nothing delivered while it ran");
        assert_eq!(
            reader.join().map_err(|_| "reader failed")?.map_err(e)?,
            *b"x"
        );
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
        .args(["(sequence! (help) (new-tab (argument (program (size (in (options) \"agent\") 80 24) \"sleep\") \"9\")))"])
        .output()
        .map_err(e)?;
    let said = String::from_utf8_lossy(&ran.stderr);
    assert_eq!(ran.status.code(), Some(1), "{said}");
    assert!(said.starts_with("fux: output: "), "{said}");
    assert!(
        !server.ok(&["(listing (list-session))"])?.contains("%2"),
        "new-tab ran after help failed"
    );
    // What is left refused as it is printed, after the run acted: the
    // report still lists the action.
    let refused = full()?.args([r#"(rename (pane 1) "x") 42"#]).output();
    let told = String::from_utf8_lossy(&refused.map_err(e)?.stderr).into_owned();
    assert!(
        told.starts_with("fux: output: ") && told.contains("\nfux: already took effect: rename\n"),
        "{told}"
    );
    let cursor = server.ok(&["(sequence! (define! value-765 (next-events (all) \"\" 0)) \"next\" (get value-765 \"next\"))"])?;
    server.ok(&["(sequence! (rename (pane 1) \"told\") #inert)"])?;
    let mut events = (full()?.arg(format!(
        "(events (all) {})",
        fux::script::quoted(cursor.trim())
    )))
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
    let cursor = server.ok(&["(sequence! (define! value-775 (next-events (all) \"\" 0)) \"next\" (get value-775 \"next\"))"])?;
    server.ok(&["(sequence! (rename (pane 1) \"told\") #inert)"])?;
    let printing = "(define! lines (lambda! (n) (when! (less-than? n 1000000) (print \"line\") (lines (add n 1))))) (lines 0)";
    let stream = format!("(events (all) {})", fux::script::quoted(cursor.trim()));
    for args in [[stream.as_str()], [printing]] {
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
    let renamed = closed(&[r#"(rename (pane 1) "renamed")"#])?;
    assert_eq!(renamed.status.code(), Some(0));
    assert_eq!(server.ok(&["(pane \"renamed\")"])?, "%1\n");
    let launch = [r#"(new-tab (program (size (in (options) "agent") 80 24) "sh"))"#];
    let made = closed(&launch)?;
    assert_eq!(made.status.code(), Some(141));
    assert_eq!(
        server.ok(&["(length (panes (workspace \"agent\")))"])?,
        "1\n"
    );
    let caught = r#"(catch-kind! (events (all) "") output failure #inert)"#;
    let stream = closed(&[caught])?;
    assert_eq!(stream.status.code(), Some(141));
    assert_eq!(String::from_utf8_lossy(&stream.stderr), "");
    Ok(())
}

/// `start` is an action, which a later failure lists.
#[test]
fn a_started_server_is_an_action() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["(sequence! (kill-server) #inert)"])?;
    let failed = {
        let _guard = spawning()?;
        server.fux(&["(sequence! (start) (raise (error (quote! fail) \"boom\")) #inert)"])?
    };
    server.ok(&["(sequence! (kill-server) #inert)"])?;
    let caught = {
        let _guard = spawning()?;
        server.ok(&[r#"(catch! (sequence! (start) (raise (error (quote! fail) "boom"))) failure (length (actions)))"#])
    };
    server.ok(&["(kill-server)"])?;
    assert!(
        failed
            .stderr
            .contains("\nfux: already took effect: start\n"),
        "{}",
        failed.stderr
    );
    assert_eq!(caught?, "1\n");
    Ok(())
}

/// A batch `next-events` leaves prints as its events, a JSON object a line:
/// nothing when none came; `json` gives all of it.
#[test]
fn a_batch_left_prints_its_events() -> Outcome {
    let server = Server::start("")?;
    let idle = server.fux(&["(get (next-events (all) \"\" 0) \"events\")"])?;
    assert_eq!((idle.status, idle.stdout.as_str()), (0, ""));
    let cursor = server.ok(&["(get (next-events (all) \"\" 0) \"next\")"])?;
    server.ok(&["(rename (pane 1) \"told\")"])?;
    let read = format!(
        "(next-events (all) {} 0)",
        fux::script::quoted(cursor.trim())
    );
    let events = server.ok(&[&format!("(get {read} \"events\")")])?;
    assert_eq!(events.lines().count(), 1);
    assert_eq!(
        read_data(&events, "(field data (quote! event))")?,
        "renamed"
    );
    assert!(!read_data(&events, "(field data (quote! next))")?.is_empty());
    let whole = server.ok(&[&read])?;
    assert_eq!(
        read_data(&whole, "(length (field data (quote! events)))")?,
        "1"
    );
    assert_eq!(read_data(&whole, "(field data (quote! missed))")?, "0");
    Ok(())
}

/// A failure's report tells the step that took effect, then every value
/// left, a batch that missed entries whole among them, and stdout nothing:
/// the action after the batch is told.
#[test]
fn a_failure_tells_what_follows_a_lost_batch() -> Outcome {
    let server = Server::start_journal(2)?;
    let cursor = server.ok(&["(sequence! (define! value-817 (next-events (all) \"\" 0)) \"next\" (get value-817 \"next\"))"])?;
    for name in ["a", "b", "c", "d"] {
        server.ok(&[&format!(
            "(sequence! (rename (pane 1) {}) #inert)",
            fux::script::quoted(name)
        )])?;
    }
    let prefix = format!(
        "(define! batch (next-events (all) {} 0)) (define! result (send-keys (text (pane 1) \"x\")))",
        fux::script::quoted(cursor.trim())
    );
    // A grift error carries its context explicitly; there is no operand stack to dump.
    let failed = server.fux(&[&format!(
        "{prefix} (raise (error (quote! fail) \"boom\" (data-text batch) (string result)))"
    )])?;
    assert_eq!(failed.status, 1, "{}", failed.stderr);
    assert!(failed.stdout.is_empty());
    assert!(
        failed.stderr.starts_with("fux: fail: "),
        "{}",
        failed.stderr
    );
    assert!(
        failed.stderr.contains("(missed 2)") && failed.stderr.contains("%1:"),
        "{}",
        failed.stderr
    );
    assert!(
        failed.stderr.contains("already took effect: send-keys"),
        "{}",
        failed.stderr
    );
    let refused = server.fux(&[&format!("{prefix} (when! (greater-than? (get batch \"missed\") 0) (raise (error (quote! journal-loss) \"entries missed\" (data-text batch))))")])?;
    assert_eq!(refused.status, 1, "{}", refused.stderr);
    assert!(refused.stdout.is_empty());
    assert!(
        refused.stderr.contains("entries missed")
            && refused.stderr.contains("already took effect: send-keys"),
        "{}",
        refused.stderr
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
    let stop = fux::script::Stop::default();
    let mut asks = fux::script::Server::stoppable(server.socket.clone(), stop.clone());
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
