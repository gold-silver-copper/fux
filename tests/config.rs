//! Config evaluation changes only its own client state.
// They script `/bin/sh` over a Unix socket (WINDOWS.md, "What PR 8 found").
#![cfg(unix)]
mod support;
use support::*;

/// Ambient config is for a typing terminal, including when a terminal is
/// available to a watcher or a headless invocation.
#[test]
fn only_a_typing_terminal_loads_the_user_config() -> Outcome {
    let server = Server::start("")?;
    std::fs::write(
        server.dir.join("config/fux/config.grift"),
        "(sequence! (set \"shell\" (list \"/no/such/leaked-shell\")) (move-keys \"C-b\" \"C-a\") #inert)",
    )
    .map_err(e)?;
    // With the bundled shell, this starts a shell and waits for its
    // prompt. A leaked shell instead fails to start.
    let waiting = ["(wait (new-tab (size (in (options) \"main\") 80 24)))"];
    // Exercise start on a fresh socket: an already running server would
    // bypass the old startup config path. Clean up even on a refusal.
    let socket = server.dir.join("started/fux.sock");
    let socket = socket.to_str().ok_or("a text socket path")?;
    let env = [("FUX_SOCKET", socket)];
    let started = (|| {
        let out = server.fux_env(&["(sequence! (start) #inert)"], &env)?;
        if out.status != 0 || !out.stderr.is_empty() {
            return Err(format!("start: {}", out.stderr));
        }
        let out = server.fux_env(&waiting, &env)?;
        if out.status != 0 {
            return Err(format!("start loaded the config: {}", out.stderr));
        }
        Ok(())
    })();
    let stopped = stop_server(std::path::Path::new(socket));
    started?;
    stopped?;
    assert!(
        server
            .ok(&["(sequence! (list-keys) #inert)"])?
            .contains("(bind-key \"C-b d\"")
    );
    for args in [
        &[
            "(sequence! (define! variables-1 (context)) (define! value-2 (options)) (define! value-3 (watch value-2)) (define! value-4 (attach value-3)) #inert)",
        ],
        &[
            "(sequence! (define! variables-5 (context)) (define! value-6 (options)) (define! value-7 (size value-6 80 10)) (define! value-8 (attach value-7)) #inert)",
        ],
        &[
            "(sequence! (define! variables-9 (context)) (define! value-10 (options)) (define! value-11 (size value-10 80 10)) (define! value-12 (watch value-11)) (define! value-13 (attach value-12)) #inert)",
        ],
    ] {
        let mut terminal = Terminal::attach(&server, 10, 80, args)?;
        let mut id = String::new();
        eventually("the attached client", || {
            id = server
                .ok(&["(listing (list-session))"])?
                .lines()
                .find_map(|line| line.strip_prefix("client "))
                .and_then(|line| line.split_whitespace().next())
                .unwrap_or_default()
                .to_owned();
            Ok(!id.is_empty())
        })?;
        let out = server.fux(&waiting)?;
        assert_eq!(out.status, 0, "{args:?}: {}", out.stderr);
        // Bundled C-b d works even though the user's prefix is C-a.
        server.ok(&[&format!(
            "(send-keys (key (key (client {}) \"C-b\") \"d\"))",
            fux::script::quoted(&id)
        )])?;
        terminal.wait_exit()?;
    }
    let mut terminal = Terminal::attach(&server, 10, 80, &[])?;
    terminal.wait_for("$")?;
    assert_eq!(server.fux(&waiting)?.status, 0);
    terminal.type_bytes(b"\x01d")?;
    terminal.wait_exit()?;
    Ok(())
}

#[test]
fn the_config_sets_the_prefix_and_bindings() -> Outcome {
    let server = Server::start(
        "(sequence! (move-keys \"C-b\" \"C-a\") (define! split-below (lambda! () (sequence! (split (axis (focused (me)) \"vertical\")) #inert))) (bind-key \"C-a y\" (code! (lambda! () (sequence! (split-below) #inert)))) (unbind \"C-a h\") (bind-key \"C-a g\" (code! (lambda! () (sequence! (split (argument (argument (program (axis (focused (me)) \"horizontal\") \"sh\") \"-c\") \"echo from-g; exec sh\")) #inert)))) (group \"Tools\") #inert)",
    )?;
    let mut client = server.attach(24, 90)?;
    client.wait_for("$")?;
    client.ready()?;
    // C-a y splits, by a word of the config's; C-b is an ordinary key now.
    client.keys("\x01y")?;
    eventually("split by C-a y", || {
        client.pump()?;
        Ok(focused(&server)? == "%2")
    })?;
    // The prefix twice sends it to the pane. Keys typed while the shell is
    // still reading its command line reach the terminal in the shell's raw
    // mode, where Enter stays a carriage return and cat never sees a line:
    // type them once the shell has run the command line.
    client.keys("echo cat-ready; cat -v\r")?;
    client.wait("the shell running the line", |t| {
        t.lines().any(|l| l == "cat-ready")
    })?;
    client.keys("\x01\x01\x02\r")?;
    client.wait("^A^B from cat -v", |t| t.lines().any(|l| l == "^A^B"))?;
    client.keys("\x03")?;
    // An unbound key says so and leaves the column open.
    client.keys("\x01h")?;
    client.wait("the notice", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("C-a h is not bound"))
    })?;
    client.wait("the column still open", |t| {
        t.contains("split side by side")
    })?;
    // A custom group is a heading in the column, after the built-in ones.
    client.keys("\x1b[F")?;
    client.wait("the Tools group", |t| t.contains("Tools"))?;
    client.keys("g")?;
    // The new pane is on the right; its own row is past the border.
    client.wait("from-g run in a new pane", |t| {
        t.lines().any(|l| l.rsplit('│').next() == Some("from-g"))
    })?;
    Ok(())
}

/// A client supplies creation policy; another caller uses its own defaults.
#[test]
fn launch_preferences_apply_only_to_new_requests() -> Outcome {
    // Three rows of two cells or more: 24 bytes a row and 8 a cell.
    let server = Server::start("(sequence! (set \"history-bytes\" 120) #inert)")?;
    // History bytes apply to new panes.
    let pane = server.words("(split (history-bytes (axis (pane 1) \"vertical\") 120))")?;
    let pane = pane.split(':').next().unwrap_or_default();
    server.type_line(
        pane,
        "for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do echo h$i; done",
    )?;
    eventually("the loop", || {
        Ok(server
            .ok(&[&format!("(capture (pane {}))", fux::script::quoted(pane))])?
            .contains("h15"))
    })?;
    let history = server.ok(&[&format!(
        "(capture (history (pane {}) 100))",
        fux::script::quoted(pane)
    )])?;
    let rows = history.lines().count();
    let screen = server
        .ok(&[&format!("(capture (pane {}))", fux::script::quoted(pane))])?
        .lines()
        .count();
    assert!(rows <= screen + 3, "at most 3 lines of history: {history}");
    // A failing local script leaves launch policy alone.
    let failed = server.fux(&[
        "(sequence! (set \"shell\" (list \"/no/such/failed-shell\")) (unbind \"Nope\") #inert)",
    ])?;
    assert_eq!(failed.status, 1);
    // A new shell program applies to new panes.
    let custom = server.words("(sequence! (set \"shell\" (list \"/usr/bin/env\" \"PS1=custom$\" \"/bin/sh\")) (split (axis (pane 1) \"horizontal\")))")?;

    eventually("the custom prompt", || {
        Ok(server
            .ok(&[&format!(
                "(capture (pane {}))",
                fux::script::quoted(custom.split_whitespace().next().unwrap_or_default())
            )])?
            .contains("custom$"))
    })?;
    // Bad values are refused; keys are a client's, never the server's.
    for args in [
        &[
            "(sequence! (define! variables-14 (context)) (define! value-15 \"buffers\") (define! value-16 (set value-15 0)) #inert)",
        ],
        &[
            "(sequence! (define! variables-17 (context)) (define! value-18 \"terminal-clipboard\") (define! value-19 \"maybe\") (define! value-20 (set value-18 value-19)) #inert)",
        ],
        &[
            "(sequence! (define! variables-21 (context)) (define! value-22 \"nonsense\") (define! value-23 (set value-22 1)) #inert)",
        ],
    ] {
        let out = server.fux(args)?;
        assert_ne!(out.status, 0, "{args:?}: {}", out.stderr);
    }
    Ok(())
}

/// An imported config cannot act on the server or commit half its local state.
#[test]
fn an_imported_failure_keeps_the_whole_bundled_state() -> Outcome {
    let server = Server::start("")?;
    let directory = server.dir.join("config/fux");
    let mut a = server.attach(8, 80)?;
    let mut b = server.attach(8, 80)?;
    a.wait_for("$")?;
    b.wait_for("$")?;
    let before = server.ok(&["(listing (list-session))"])?;
    std::fs::write(
        directory.join("config.grift"),
        "(require! \"shared.grift\")",
    )
    .map_err(e)?;
    std::fs::write(
        directory.join("shared.grift"),
        "(sequence! (set \"titles\" #true) (set \"history-bytes\" 0) #inert)",
    )
    .map_err(e)?;
    let configured = fux::script::run_file(&directory.join("config.grift")).map_err(e)?;
    assert_eq!(configured.preferences.history_bytes.get(), 0);
    assert!(configured.preferences.titles);
    assert_eq!(server.ok(&["(listing (list-session))"])?, before);
    a.pump()?;
    b.pump()?;
    assert!(!String::from_utf8_lossy(&a.painted).contains("\x1b]2;"));
    assert!(!String::from_utf8_lossy(&b.painted).contains("\x1b]2;"));
    std::fs::write(
        directory.join("shared.grift"),
        "(sequence! (move-keys \"C-b\" \"C-a\") (set \"titles\" #true) (attach-defaults (in (options) \"missing\")) (define! only-in-failed-config (lambda! () 41)) (split (axis (pane 1) \"horizontal\")) #inert)",
    )
    .map_err(e)?;
    let before = server.ok(&["(listing (list-session))"])?;
    let failed = fux::script::run_file(&directory.join("config.grift"))
        .err()
        .ok_or("config acted on the server")?;
    assert_eq!(failed.code, fux_wire::code::Code::NotConfig);
    assert!(failed.notice().contains("shared.grift"));
    assert_eq!(server.ok(&["(listing (list-session))"])?, before);
    let mut terminal = Terminal::attach(&server, 10, 100, &[])?;
    terminal.wait_for("a config changes only")?;
    terminal.type_bytes(b"\x02")?;
    terminal.wait_for("split side by side")?;
    terminal.type_bytes(b"d")?;
    terminal.wait_exit()?;
    assert_eq!(server.panes()?.len(), 1);
    let probe = server.fux(&["(wait (new-tab (argument (argument (program (size (in (options) \"agent\") 80 24) \"/bin/sh\") \"-c\") \"sleep 1\")))"])?;
    assert_eq!(
        probe.status, 0,
        "a failed config changed another invocation's wait: {}",
        probe.stderr
    );
    Ok(())
}

/// A config's stray `label` is its own mistake, told as it attaches, and
/// the client keeps working default keys.
#[test]
fn a_stray_label_is_reported_and_keeps_the_default_keys() -> Outcome {
    let server = Server::start("")?;
    let config = server.dir.join("config/fux/config.grift");
    std::fs::write(&config, r#"(label "mine")"#).map_err(e)?;
    let mut terminal = Terminal::attach(&server, 10, 100, &[])?;
    terminal.wait_for("bad-binding")?;
    terminal.type_bytes(b"\x02")?;
    terminal.wait_for("split side by side")?;
    terminal.type_bytes(b"d")?;
    terminal.wait_exit()?;
    Ok(())
}

/// A config that fails is reported to the client attaching with it, which
/// keeps the default keys; running it locally, as attaching does, finds its
/// mistakes.
#[test]
fn a_bad_config_is_reported() -> Outcome {
    let server = Server::start("")?;
    let config = server.dir.join("config/fux/config.grift");
    std::fs::write(&config, r#"(move-keys "Nope" "C-a")"#).map_err(e)?;
    let mut terminal = Terminal::attach(&server, 10, 100, &[])?;
    terminal.wait_for("bad-binding:")?;
    terminal.type_bytes(b"\x02")?;
    terminal.wait_for("split side by side")?;
    terminal.type_bytes(b"d")?;
    terminal.wait_exit()?;
    std::fs::write(&config, "(set \"history-bytes\" 5)\n(frobnicate)\n").map_err(e)?;
    let failed = fux::script::run_file(&config)
        .err()
        .ok_or("frobnicate ran")?;
    assert!(
        failed.to_string().contains("config.grift:2") && failed.to_string().contains("frobnicate"),
        "{failed}"
    );
    // A failed config's settings are thrown away, so none took effect.
    std::fs::write(&config, "(set \"titles\" #true)\n(panes (all))\n").map_err(e)?;
    let failed = fux::script::run_file(&config)
        .err()
        .ok_or("a config asked the server")?;
    assert_eq!(failed.kind.as_deref(), Some("not-config"), "{failed}");
    assert_eq!(failed.actions, Vec::<String>::new(), "{failed}");
    let later = r#"(move-keys "C-b" "C-a")
        (bind-key "C-a v" (code! (lambda! () (sr) #inert)))
        (define! sr (lambda! () (zoom (focused (me)))))"#;
    std::fs::write(&config, later).map_err(e)?;
    fux::script::run_file(&config).map_err(e)?;
    let mut terminal = Terminal::attach(&server, 10, 100, &[])?;
    terminal.wait_for("$")?;
    terminal.type_bytes(b"\x01v")?;
    eventually("the callback resolves its later definition", || {
        Ok(server
            .ok(&["(get (first (get (snapshot (tab 1) \"topology\") \"tabs\")) \"zoomed\")"])?
            .trim()
            != "none")
    })?;
    terminal.type_bytes(b"\x01d")?;
    terminal.wait_exit()?;
    Ok(())
}

/// Only a missing optional root selects defaults; other read failures are visible.
#[test]
fn optional_config_reports_read_failures() -> Outcome {
    let server = Server::start("")?;
    let path = server.dir.join("config/fux/config.grift");
    std::fs::remove_file(&path).map_err(e)?;
    let mut terminal = Terminal::attach(&server, 10, 80, &[])?;
    terminal.wait_for("$")?;
    terminal.type_bytes(b"\x02")?;
    terminal.wait_for("split side by side")?;
    assert!(!String::from_utf8_lossy(&terminal.output).contains("read:"));
    terminal.type_bytes(b"d")?;
    terminal.wait_exit()?;
    // Every read failure remains visible to a typing terminal; ordinary
    // CLI commands never inspect the optional config.
    for kind in ["broken", "loop", "directory", "non-text"] {
        match kind {
            "broken" => std::os::unix::fs::symlink("gone", &path).map_err(e)?,
            "loop" => std::os::unix::fs::symlink("config.grift", &path).map_err(e)?,
            "directory" => std::fs::create_dir(&path).map_err(e)?,
            _ => std::fs::write(&path, [0xff]).map_err(e)?,
        }
        assert!(
            server
                .ok(&["(sequence! (list-keys) #inert)"])?
                .contains("\"C-b d\"")
        );
        let mut terminal = Terminal::attach(&server, 10, 80, &[])?;
        terminal.wait_for("read:")?;
        if kind == "non-text" {
            terminal.wait_for("not UTF-8")?;
        }
        terminal.type_bytes(b"\x02")?;
        terminal.wait_for("split side by side")?;
        terminal.type_bytes(b"d")?;
        terminal.wait_exit()?;
        if kind == "directory" {
            std::fs::remove_dir(&path).map_err(e)?;
        } else {
            std::fs::remove_file(&path).map_err(e)?;
        }
    }
    Ok(())
}

#[test]
fn config_notices_show_the_cause_once_before_the_path() -> Outcome {
    let server = Server::start("")?;
    let path = server.dir.join("config/fux/config.grift");
    std::fs::write(&path, r#"(set "history-bytes" -1)"#).map_err(e)?;
    let failure = fux::script::run_file(&path)
        .err()
        .ok_or("bad setting accepted")?;
    let notice = failure.notice();
    let cause = notice
        .find("history-bytes: -1 is out of range")
        .ok_or(format!("no cause: {notice}"))?;
    let location = notice
        .find("config.grift:1")
        .ok_or(format!("no location: {notice}"))?;
    assert!(
        notice.starts_with("config:") && cause < location,
        "{notice}"
    );
    assert_eq!(notice.matches("is out of range").count(), 1, "{notice}");
    let mut terminal = Terminal::attach(&server, 10, 80, &[])?;
    terminal.wait_for("history-bytes: -1 is out of range")?;
    terminal.type_bytes(b"\x02d")?;
    terminal.wait_exit()?;
    Ok(())
}

/// The examples of the words a config uses run as a config: none binds
/// after a key bound by default.
#[test]
fn the_config_words_examples_run_as_a_config() -> Outcome {
    let server = Server::start("")?;
    let config = server.dir.join("config.grift");
    for word in ["bind-key", "move-keys", "repeat"] {
        let explained = server.ok(&[&format!(
            "(sequence! (explain {}) #inert)",
            fux::script::quoted(word)
        )])?;
        let example = (explained.lines())
            .find_map(|line| line.strip_prefix("Example: "))
            .ok_or(format!("no example: {explained}"))?;
        std::fs::write(&config, example).map_err(e)?;
        fux::script::run_file(&config).map_err(|f| {
            format!(
                "(list {} {})",
                support::reference(&format!("{example}:")),
                support::reference(&format!("{f}"))
            )
        })?;
    }
    Ok(())
}

#[test]
fn a_program_uses_preferences_before_its_next_operation() -> Outcome {
    let server = Server::start("")?;
    let result = server.fux(&["(sequence! (set \"shell\" (list \"/no/such/fux-review-shell\")) (split (axis (pane 1) \"horizontal\")) #inert)"])?;
    assert_eq!(
        result.status, 1,
        "split used the previous shell: {}",
        result.stdout
    );
    assert!(
        result.stderr.contains("/no/such/fux-review-shell"),
        "{}",
        result.stderr
    );
    assert_eq!(server.panes()?.len(), 1);
    Ok(())
}

/// A new small pane uses its invocation's history budget.
fn history_rows(server: &Server) -> Result<usize, String> {
    let answer = server.ok(&["(new-tab (argument (argument (program (size (in (options) \"agent\") 80 5) \"/bin/sh\") \"-c\") \"i=0; while [ \\\"$i\\\" -lt 40 ]; do echo row; i=$((i+1)); done\"))"])?;
    let pane = answer.trim().split(':').next().ok_or("pane")?;
    server.exits(pane)?;
    Ok(server
        .ok(&[&format!(
            "(capture (all-history {}))",
            support::reference(pane)
        )])?
        .lines()
        .count())
}

/// Local creation defaults never become server policy.
#[test]
fn launch_defaults_do_not_change_the_server() -> Outcome {
    let server = Server::start("")?;
    for code in [
        "(sequence! (set \"history-bytes\" 0) #inert)",
        "(sequence! (set \"history-bytes\" 0) (list-session))",
    ] {
        let out = server.fux(&[code])?;
        assert_eq!(out.status, 0, "{}", out.stderr);
        assert!(
            history_rows(&server)? > 5,
            "local budget changed the server"
        );
    }
    Ok(())
}

#[test]
fn key_words_refuse_the_command_line() -> Outcome {
    let server = Server::start("")?;
    for words in [
        &[
            "(sequence! (define! variables-24 (context)) (define! value-25 \"C-b\") (define! value-26 \"C-a\") (define! value-27 (move-keys value-25 value-26)) #inert)",
        ],
        &[
            "(sequence! (define! variables-28 (context)) (define! value-29 \"q\") (define! value-34 (code! (lambda! () (sequence! (define! variables-30 (context)) (define! value-31 (me)) (define! value-32 (focused value-31)) (define! value-33 (zoom value-32)) #inert)))) (define! value-35 (bind-key value-29 value-34)) #inert)",
        ],
        &[
            "(sequence! (define! variables-36 (context)) (define! value-37 \"q\") (define! value-38 (unbind value-37)) #inert)",
        ],
        &["(sequence! (define! variables-39 (context)) (define! value-40 (unbind-all)) #inert)"],
        &[
            "(sequence! (define! variables-41 (context)) (define! value-42 \"copy\") (define! value-43 \"g\") (define! value-44 \"top\") (define! value-45 (bind-in value-42 value-43 value-44)) #inert)",
        ],
        &[
            "(sequence! (define! variables-46 (context)) (define! value-47 \"copy\") (define! value-48 \"v\") (define! value-49 (unbind-in value-47 value-48)) #inert)",
        ],
        &[
            "(sequence! (define! variables-50 (context)) (define! value-51 \"name\") (define! value-52 (label value-51)) #inert)",
        ],
        &[
            "(sequence! (define! variables-53 (context)) (define! value-54 \"name\") (define! value-55 (group value-54)) #inert)",
        ],
        &[
            "(sequence! (define! variables-56 (context)) (define! value-57 \"C-b r\") (define! value-58 (repeat value-57)) #inert)",
        ],
    ] {
        let result = server.fux(words)?;
        assert_eq!(result.status, 1, "silent CLI key change: {words:?}");
        assert!(
            result.stderr.contains("not-config") && result.stderr.contains("config"),
            "{words:?}: {}",
            result.stderr
        );
    }
    Ok(())
}

#[test]
fn unrelated_commands_do_not_load_shell_defaults() -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let server = Server::start("")?;
    for words in [
        &["--version"][..],
        &[
            "(sequence! (define! variables-59 (context)) (define! value-60 (list-session)) value-60)",
        ],
        &["(sequence! (define! variables-61 (context)) (define! value-62 (status)) value-62)"],
        &["(sequence! (define! variables-63 (context)) (define! value-64 (kill-server)) #inert)"],
    ] {
        let result = std::process::Command::new(FUX)
            .args(words)
            .env("FUX_SOCKET", &server.socket)
            .env("SHELL", std::ffi::OsStr::from_bytes(b"/bin/\xffsh"))
            .env_remove("FUX_PANE")
            .env_remove("FUX_SERVER")
            .output()
            .map_err(e)?;
        assert!(
            result.status.success(),
            "{words:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty(), "{words:?} read SHELL");
    }
    Ok(())
}

#[test]
fn non_text_shell_defaults_report_the_library_fallback() -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let server = Server::start("")?;
    let result = std::process::Command::new(FUX)
        .arg("(list-keys)")
        .env("FUX_SOCKET", &server.socket)
        .env("SHELL", std::ffi::OsStr::from_bytes(b"/bin/\xffsh"))
        .env_remove("FUX_PANE")
        .env_remove("FUX_SERVER")
        .output()
        .map_err(e)?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("(bind-key \"C-b d\""));
    let diagnostic = String::from_utf8_lossy(&result.stderr);
    assert!(diagnostic.contains("SHELL"), "{diagnostic}");
    assert_eq!(diagnostic.lines().count(), 1, "{diagnostic}");
    Ok(())
}

#[test]
fn explicit_settings_survive_a_failed_attach_config() -> Outcome {
    let server = Server::start("")?;
    std::fs::write(
        server.dir.join("config/fux/config.grift"),
        "(sequence! (unbind \"Nope\") #inert)",
    )
    .map_err(e)?;
    let mut terminal = Terminal::attach(
        &server,
        12,
        100,
        &[r#"(set "titles" #true) (attach (options))"#],
    )?;
    terminal.wait_for("bad-binding")?;
    terminal.type_bytes(b"printf '\\033]2;explicit-title\\007'; echo cli''-ready\r")?;
    terminal.wait_for("cli-ready")?;
    terminal.pump();
    let titled = String::from_utf8_lossy(&terminal.output).contains("\x1b]2;explicit-title\x1b\\");
    terminal.type_bytes(b"\x02d")?;
    terminal.wait_exit()?;
    assert!(
        titled,
        "the explicit titles setting was dropped with the failed config"
    );
    Ok(())
}

/// Launch policy belongs to each initiating client, and its saved patch
/// survives both respawn and replay under a different client's selection.
#[test]
fn launches_save_the_callers_argv_role_and_environment() -> Outcome {
    let server = Server::start("")?;
    for marker in ["alpha", "beta"] {
        let code = format!(
            "(sequence! (set \"shell\" (list \"/bin/sh\" \"-c\" {})) (new-tab (name (size (in (options) \"main\") 80 24) {})))",
            fux::script::quoted(&format!("printf {marker}; exec /bin/cat")),
            fux::script::quoted(marker)
        );
        let answer = server.ok(&[&code])?;
        let pane = answer.trim().split(':').next().ok_or("shell pane")?;
        eventually("shell output", || {
            Ok(server
                .ok(&[&format!("(capture {})", support::reference(pane))])?
                .contains(marker))
        })?;
        let saved = server.ok(&[&format!("(information {})", support::reference(pane))])?;
        assert!(
            saved.contains(&format!("printf {marker}; exec /bin/cat")),
            "{saved}"
        );
        assert!(
            server
                .fux(&[&format!("(respawn {})", support::reference(pane))])?
                .stderr
                .contains("runs a shell")
        );
        assert!(
            server
                .fux(&[&format!("(signal {} \"TERM\")", support::reference(pane))])?
                .stderr
                .contains("only-shell")
        );
        server.ok(&[&format!(
            "(sequence! (send-keys (key {} \"C-d\")) #inert)",
            support::reference(pane)
        )])?;
        eventually("shell closes", || {
            Ok(!server.panes()?.iter().any(|(p, _)| p == pane))
        })?;
    }
    let split = server.ok(&["(information (split (axis (pane 1) \"horizontal\")))"])?;
    assert_eq!(
        read_data(
            &split,
            "(field (field (first (field data (quote! panes))) (quote! program)) (quote! generation))"
        )?,
        "0"
    );
    assert_eq!(
        read_data(
            &split,
            "(first (field (field (first (field data (quote! panes))) (quote! program)) (quote! arguments)))"
        )?,
        "/bin/sh"
    );
    assert_eq!(
        read_data(
            &split,
            "(length (field (field (first (field data (quote! panes))) (quote! program)) (quote! arguments)))"
        )?,
        "1"
    );
    assert_eq!(
        read_data(
            &split,
            "(field (field (first (field data (quote! panes))) (quote! launch)) (quote! choice))"
        )?,
        "shell"
    );
    let before = server.panes()?.len();
    for (conflict, code) in [
        (
            "(new-tab (program (shell-program (size (in (options) \"main\") 80 24) (list \"/bin/sh\")) \"/bin/cat\"))",
            "unused",
        ),
        (
            "(new-tab (shell-program (program (size (in (options) \"main\") 80 24) \"/bin/cat\") (list \"/bin/sh\")))",
            "unused",
        ),
    ] {
        let out = server.fux(&[conflict])?;
        assert_eq!(out.status, 1, "{conflict}");
        assert!(
            out.stderr.starts_with(&format!("fux: {code}: ")),
            "{}",
            out.stderr
        );
        assert_eq!(server.panes()?.len(), before);
    }
    Ok(())
}

/// A remote client action commits while its subsequent wait is still pending.
#[test]
fn presentation_updates_only_the_addressed_view_before_a_wait_finishes() -> Outcome {
    let server = Server::start(
        "(sequence! (bind-key \"F5\" (code! (lambda! () (sequence! (set \"titles\" #true) (wait (timeout (pane 1) 30)) #inert)))) #inert)",
    )?;
    let mut a = server.attach(8, 60)?;
    let mut b = server.attach(8, 60)?;
    a.wait_for("$")?;
    b.wait_for("$")?;
    // Its id as it was welcomed: two clients attaching at once are given
    // theirs in the order their connections' processes reach the server.
    let id = a.id.clone().ok_or("a's id")?;
    // Answered once its client took the keys, so it is pumped meanwhile.
    let mut sending = server.spawn(&[&format!(
        "(sequence! (send-keys (key (client {}) \"F5\")) #inert)",
        fux::script::quoted(&id.to_string())
    )])?;
    eventually("committed title before the wait finishes", || {
        a.pump()?;
        b.pump()?;
        Ok(String::from_utf8_lossy(&a.painted).contains("\x1b]2;"))
    })?;
    eventually("the keys taken", || {
        a.pump()?;
        Ok(sending.try_wait().map_err(e)?.is_some_and(|s| s.success()))
    })?;
    assert!(!String::from_utf8_lossy(&b.painted).contains("\x1b]2;"));
    Ok(())
}

#[test]
fn pane_creation_overrides_preserve_zero_and_false_and_dump_replays_them() -> Outcome {
    let server = Server::start("")?;
    let created = server.ok(&["(sequence! (set \"history-bytes\" 4096) (set \"program-clipboard\" #true) (new-tab (program (program-clipboard (picture-bytes (alternate-scroll-lines (reflow (history-bytes (size (in (options) \"agent\") 40 6) 0) #false) 0) 0) #false) \"/bin/cat\")))"])?;
    let pane = created.trim().split(':').next().ok_or("pane")?;
    assert_eq!(
        server
            .ok(&[&format!(
                "(sequence! \"history_bytes\" (get {} \"history-bytes\"))",
                support::reference(pane)
            )])?
            .trim(),
        "0"
    );
    assert_eq!(
        server
            .ok(&[&format!(
                "(sequence! \"program_clipboard\" (get {} \"program-clipboard\"))",
                support::reference(pane)
            )])?
            .trim(),
        "#false"
    );
    server.words(
        "(sequence! (set \"history-bytes\" 1024) (set \"program-clipboard\" #true) #inert)",
    )?;
    assert_eq!(
        server
            .ok(&[&format!(
                "(sequence! \"history_bytes\" (get {} \"history-bytes\"))",
                support::reference(pane)
            )])?
            .trim(),
        "0"
    );
    let recipe = server.ok(&[&format!("(dump (tab {}))", support::reference(pane))])?;
    for (field, value) in [
        ("history-bytes", "0"),
        ("reflow", "#false"),
        ("alternate-scroll-lines", "0"),
        ("picture-bytes", "0"),
        ("program-clipboard", "#false"),
    ] {
        assert_eq!(
            read_data(
                &recipe,
                &format!(
                    "(field (first (field (second (second data)) (quote! panes))) (quote! {field}))"
                )
            )?,
            value
        );
    }
    for (field, value) in [
        ("reflow", "#false"),
        ("alternate_scroll_lines", "0"),
        ("picture_bytes", "0"),
    ] {
        assert_eq!(
            server
                .ok(&[&format!(
                    "(get {} {})",
                    support::reference(pane),
                    fux::script::quoted(&fux::script::field_name(field))
                )])?
                .trim(),
            value
        );
    }
    let replay = server.dir.join("pane-policy.grift");
    std::fs::write(
        &replay,
        format!("(set \"history-bytes\" 8192) (set \"program-clipboard\" #true)\n{recipe}"),
    )
    .map_err(e)?;
    server.ok(&["--file", &replay.to_string_lossy()])?;
    assert_eq!(
        server.ok(&["(sequence! (define! value-975 (pane 3)) \"history_bytes\" (get value-975 \"history-bytes\"))"])?.trim(),
        "0"
    );
    assert_eq!(
        server
            .ok(&["(sequence! (define! value-979 (pane 3)) \"program_clipboard\" (get value-979 \"program-clipboard\"))"])?
            .trim(),
        "#false"
    );
    for (field, value) in [
        ("reflow", "#false"),
        ("alternate_scroll_lines", "0"),
        ("picture_bytes", "0"),
    ] {
        assert_eq!(
            server
                .ok(&[&format!(
                    "(get (pane 3) {})",
                    fux::script::quoted(&fux::script::field_name(field))
                )])?
                .trim(),
            value
        );
    }
    Ok(())
}

/// Another program reads a client's keys and preferences from the client
/// itself: the keymap nested, a binding two maps deep, the map the client
/// is in now, and a preference's name, value and type. A binding's code is
/// a code value of its words, which reading never compiles: `execute`
/// compiles them in the dictionary it runs in, so a word that one lacks
/// fails there, and a read in the middle of a computation leaves it as it
/// was.
#[test]
fn another_program_reads_a_clients_keys_and_preferences() -> Outcome {
    let server = Server::start(
        r#"
        (bind-key "C-b g u" (code! (lambda! () (zoom (focused (me))) #inert)))
        (repeat "C-b g")
        (define! say-ran (lambda! () (print "ran-in-the-client")))
        (bind-key "C-b g s" (code! (lambda! () (say-ran))))
        (bind-key "C-b g r" (code! (lambda! () #inert)))
        (bind-key "C-b g y" (code! (lambda! () (raise (error (quote! deferred) "not read")))))
        (bind-key "C-b g n" (code! (lambda! (name) (rename (focused (me)) name))))
        (bind-key "C-b g t" (code! (lambda! () (print "ran-by-the-reader"))))
        (set "history-bytes" 4096)
    "#,
    )?;
    let mut terminal = Terminal::attach(&server, 10, 80, &[])?;
    terminal.wait_for("$")?;
    let listing = server.ok(&["(listing (list-session))"])?;
    let id = listing
        .lines()
        .find_map(|line| line.strip_prefix("client "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or("no client")?
        .to_owned();
    let prefix = format!(
        r#"(define! c (client {}))
        (define! find-key (lambda! (rows key) (first (filter (lambda! (row) (same? (get row "key") key)) rows))))
        (define! root-rows (lambda! () (get (keys c) "keymap")))
        (define! child-rows (lambda! (rows key) (get (find-key rows key) "keymap")))
        (define! prefix-rows (lambda! () (child-rows (root-rows) "C-b")))
        (define! tool-rows (lambda! () (child-rows (prefix-rows) "g")))
        (define! binding-code (lambda! (key) (get (find-key (tool-rows) key) "code")))
    "#,
        fux::script::quoted(&id)
    );
    let query = |body: &str| format!("{prefix} {body}").replace('\n', " ");
    let read = |what: &str| {
        server.ok(&[&format!(
            "(data-text ({what} (client {})))",
            fux::script::quoted(&id)
        )])
    };
    let keys = read("keys")?;
    assert_eq!(read_data(&keys, "(field data (quote! at))")?, "");
    assert_eq!(
        server
            .ok(&[&query(r#"(get (find-key (prefix-rows) "g") "repeat")"#)])?
            .trim(),
        "#true"
    );
    assert_eq!(
        server
            .ok(&[&query(
                r#"(kind (get (find-key (prefix-rows) "x") "code"))"#
            )])?
            .trim(),
        "code"
    );
    assert!(keys.contains("confirm-close") && keys.contains("close pane"));
    assert!(keys.contains("submit") && keys.contains("accept"));
    assert_eq!(
        server
            .ok(&[&query(r#"(execute! (binding-code "t"))"#)])?
            .trim(),
        "ran-by-the-reader"
    );
    assert_eq!(server.ok(&[&query(r#"(define! execute-read (lambda! (code) (execute! code))) (execute-read (binding-code "t"))"#)])?.trim(), "ran-by-the-reader");
    let unavailable = server.fux(&[&query(r#"(execute! (binding-code "s"))"#)])?;
    assert!(
        unavailable.stderr.contains("unbound-symbol") && unavailable.stderr.contains("say-ran"),
        "{}",
        unavailable.stderr
    );
    let scoped = server.fux(&[&query(r#"(execute! (binding-code "u"))"#)])?;
    assert!(scoped.stderr.contains("not-here"), "{}", scoped.stderr);
    assert_eq!(server.ok(&[&query(r#"(define! kept "kept") (define! n 7) (keys c) (list n kept (kind (binding-code "y")))"#)])?, "7\nkept\ncode\n");
    terminal.type_bytes(b"\x02g")?;
    eventually("the client in C-b g", || {
        Ok(read_data(&read("keys")?, "(field data (quote! at))")? == "C-b g")
    })?;
    let preferences = read("preferences")?;
    assert_eq!(
        read_data(
            &preferences,
            "(field (first (filter (lambda! (row) (same? (field row (quote! name)) \"history-bytes\")) data)) (quote! value))"
        )?,
        "4096"
    );
    let type_name = read_data(
        &preferences,
        "(field (first (filter (lambda! (row) (same? (field row (quote! name)) \"history-bytes\")) data)) (quote! type))",
    )?;
    assert!(type_name.contains("9223372036854775807"), "{type_name}");
    terminal.type_bytes(b"\x1b")?;
    eventually("Escape leaves the map", || {
        Ok(read_data(&read("keys")?, "(field data (quote! at))")?.is_empty())
    })?;
    server.ok(&[&query(
        r#"(prompt c "Reuse" "named-by-prompt" (binding-code "n"))"#,
    )])?;
    terminal.wait_for("Reuse")?;
    terminal.type_bytes(b"\r")?;
    eventually("the reused callback ran", || {
        Ok(server.ok(&[&query(r#"(get (focused c) "name")"#)])?.trim() == "named-by-prompt")
    })?;
    terminal.type_bytes(b"\x02e")?;
    terminal.wait_for("Enter accept")?;
    terminal.type_bytes(format!("{}\r", query(r#"(execute! (binding-code "s"))"#)).as_bytes())?;
    terminal.wait_for("ran-in-the-client")?;
    terminal.type_bytes(b"\x02e")?;
    terminal.wait_for("Enter accept")?;
    terminal.type_bytes(
        format!("{}\r", query(r#"(bind-key "C-b Q" (binding-code "t"))"#)).as_bytes(),
    )?;
    eventually("the read code was rebound", || {
        Ok(server.ok(&[&query(r#"(not? (empty-list? (filter (lambda! (row) (same? (get row "key") "Q")) (prefix-rows))))"#)])?.trim() == "#true")
    })?;
    assert_eq!(
        server
            .ok(&[&query(
                r#"(kind (get (find-key (prefix-rows) "Q") "code"))"#
            )])?
            .trim(),
        "code"
    );
    terminal.type_bytes(b"\x02Q")?;
    terminal.wait_for("ran-by-the-reader")?;
    terminal.type_bytes(b"\x02e")?;
    terminal.wait_for("Enter accept")?;
    let fresh = r#"(bind-key "C-b y" (code! (lambda! () (zoom (focused (me))) #inert)))
        (if! (same? (kind (get (find-key (prefix-rows) "y") "code")) "code") (print (string-append "read-back-" "fresh")) (print (string-append "read-back-" "stale")))"#;
    terminal.type_bytes(format!("{}\r", query(fresh)).as_bytes())?;
    terminal.wait_for("read-back-fresh")?;
    terminal.type_bytes(b"\x02d")?;
    terminal.wait_exit()?;
    let gone = server.fux(&[&format!("(keys (client {}))", fux::script::quoted(&id))])?;
    assert!(gone.stderr.contains("no-client"), "{}", gone.stderr);
    Ok(())
}
