//! Config evaluation changes only its own client state.
mod support;
use support::*;

/// Ambient config is for a typing terminal, including when a terminal is
/// available to a watcher or a headless invocation.
#[test]
fn only_a_typing_terminal_loads_the_user_config() -> Outcome {
    let server = Server::start("")?;
    std::fs::write(
        server.dir.join("config/fux/config.fux"),
        "(sequence! (set \"shell\" (list \"/no/such/leaked-shell\")) (move-keys \"C-b\" \"C-a\") #inert)",
    )
    .map_err(e)?;
    // With the bundled shell, this starts a shell and waits for its
    // prompt. A leaked shell instead fails to start.
    let waiting = [
        "opts", "str", "main", "in", "80", "24", "size", "new-tab", "wait",
    ];
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
            .contains("str 'C-b d' code")
    );
    for args in [
        &["opts", "watch", "attach"][..],
        &["opts", "80", "10", "size", "attach"],
        &["opts", "80", "10", "size", "watch", "attach"],
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
        &["str", "buffers", "0", "set"][..],
        &["str", "terminal-clipboard", "str", "maybe", "set"],
        &["str", "nonsense", "1", "set"],
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
    std::fs::write(directory.join("config.fux"), "require shared.fux").map_err(e)?;
    std::fs::write(
        directory.join("shared.fux"),
        "(sequence! (set \"titles\" #true) (set \"history-bytes\" 0) #inert)",
    )
    .map_err(e)?;
    let configured = fux::script::run_file(&directory.join("config.fux")).map_err(e)?;
    assert_eq!(configured.preferences.history_bytes.get(), 0);
    assert!(configured.preferences.titles);
    assert_eq!(server.ok(&["(listing (list-session))"])?, before);
    a.pump()?;
    b.pump()?;
    assert!(!String::from_utf8_lossy(&a.painted).contains("\x1b]2;"));
    assert!(!String::from_utf8_lossy(&b.painted).contains("\x1b]2;"));
    std::fs::write(
        directory.join("shared.fux"),
        "(sequence! (move-keys \"C-b\" \"C-a\") (set \"titles\" #true) (attach-defaults (in (options) \"missing\")) (define! only-in-failed-config (lambda! () 41)) (split (axis (pane 1) \"horizontal\")) #inert)",
    )
    .map_err(e)?;
    let before = server.ok(&["(listing (list-session))"])?;
    let failed = fux::script::run_file(&directory.join("config.fux"))
        .err()
        .ok_or("config acted on the server")?;
    assert_eq!(failed.code, fux_wire::code::Code::NotConfig);
    assert!(failed.notice().contains("shared.fux"));
    assert_eq!(server.ok(&["(listing (list-session))"])?, before);
    let mut terminal = Terminal::attach(&server, 10, 100, &[])?;
    terminal.wait_for("a config changes only its own client state")?;
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

/// A config that fails is reported to the client attaching with it, which
/// keeps the default keys; running it locally, as attaching does, finds its
/// mistakes.
#[test]
fn a_bad_config_is_reported() -> Outcome {
    let server = Server::start("")?;
    let config = server.dir.join("config").join("fux").join("config.fux");
    std::fs::write(&config, "(sequence! (move-keys \"Nope\" \"C-a\") #inert)").map_err(e)?;
    let mut terminal = Terminal::attach(&server, 10, 100, &[])?;
    terminal.wait_for("bad_binding: move-keys:")?;
    // The defaults: C-b opens the column.
    terminal.type_bytes(b"\x02")?;
    terminal.wait_for("split side by side")?;
    terminal.type_bytes(b"d")?;
    terminal.wait_exit()?;
    // An unknown word is found before anything runs, naming its line.
    std::fs::write(&config, "str history-bytes 5 set\nfrobnicate\n").map_err(e)?;
    let path = config.to_string_lossy().into_owned();
    let failed = fux::script::run_file(&config)
        .err()
        .ok_or("frobnicate ran")?;
    assert!(
        failed.to_string().contains("config.fux:2: frobnicate"),
        "{failed}"
    );
    std::fs::write(&config, "(sequence! (move-keys \"C-b\" \"C-a\") #inert)").map_err(e)?;
    fux::script::run_file(&config).map_err(e)?;
    // A binding sees the config's definitions made before it, as code
    // compiled there does; one not made yet is said at the binding's line,
    // before the binding is made.
    let ok = "(sequence! (define! sr (lambda! () (sequence! (split (axis (focused (me)) \"horizontal\")) #inert))) (bind-key \"C-b v\" (code! (lambda! () (sequence! (sr) #inert)))) #inert)";
    std::fs::write(&config, ok).map_err(e)?;
    fux::script::run_file(&config).map_err(e)?;
    let text =
        "str C-b str C-a move-keys\nstr 'C-a v' code sr end bind\ndefine sr me focused zoom end\n";
    std::fs::write(&config, text).map_err(e)?;
    let failed = fux::script::run_file(&config)
        .err()
        .ok_or(text)?
        .to_string();
    assert!(failed.contains(&path.replace("config.fux", "")), "{failed}");
    assert!(
        failed.contains("config.fux:2: sr: no such word"),
        "{failed}"
    );
    Ok(())
}

/// Only a missing optional root selects defaults; other read failures are visible.
#[test]
fn optional_config_reports_read_failures() -> Outcome {
    let server = Server::start("")?;
    let path = server.dir.join("config/fux/config.fux");
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
            "loop" => std::os::unix::fs::symlink("config.fux", &path).map_err(e)?,
            "directory" => std::fs::create_dir(&path).map_err(e)?,
            _ => std::fs::write(&path, [0xff]).map_err(e)?,
        }
        assert!(
            server
                .ok(&["(sequence! (list-keys) #inert)"])?
                .contains("'C-b d'")
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
    let path = server.dir.join("config/fux/config.fux");
    std::fs::write(&path, "(sequence! (set \"history-bytes\" -1) #inert)").map_err(e)?;
    let failure = fux::script::run_file(&path)
        .err()
        .ok_or("expected a bad setting")?;
    assert!(failure.place.is_some());
    let notice = failure.notice();
    assert!(
        notice.starts_with("config: set: history-bytes: -1 is out of range"),
        "{notice}"
    );
    assert_eq!(notice.matches("config:").count(), 1);
    assert!(notice.ends_with("config.fux:1)"), "{notice}");
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
    let config = server.dir.join("config.fux");
    for word in ["bind", "move-keys", "repeat"] {
        let explained = server.ok(&[&format!(
            "(sequence! (explain {}) #inert)",
            fux::script::quoted(word)
        )])?;
        let example = (explained.lines())
            .find_map(|line| line.split_once("; e.g. "))
            .map(|(_, example)| example)
            .ok_or(format!("no example: {explained}"))?;
        std::fs::write(&config, example).map_err(e)?;
        fux::script::run_file(&config).map_err(|f| format!("{example}: {f}"))?;
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
        &["str", "C-b", "str", "C-a", "move-keys"][..],
        &["str", "q", "code", "me", "focused", "zoom", "end", "bind"],
        &["str", "q", "unbind"],
        &["unbind-all"],
        &["str", "copy", "str", "g", "str", "top", "bind-in"],
        &["str", "copy", "str", "v", "unbind-in"],
        &["str", "name", "label"],
        &["str", "name", "group"],
        &["str", "C-b r", "repeat"],
    ] {
        let result = server.fux(words)?;
        assert_eq!(result.status, 1, "silent CLI key change: {words:?}");
        assert!(
            result.stderr.contains("not_config") && result.stderr.contains("config"),
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
    for words in [&["--version"][..], &["ls"], &["status"], &["kill-server"]] {
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
        .arg("list-keys")
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
    assert!(String::from_utf8_lossy(&result.stdout).contains("str 'C-b d' code"));
    let diagnostic = String::from_utf8_lossy(&result.stderr);
    assert!(diagnostic.contains("SHELL"), "{diagnostic}");
    assert_eq!(diagnostic.lines().count(), 1, "{diagnostic}");
    Ok(())
}

#[test]
fn explicit_settings_survive_a_failed_attach_config() -> Outcome {
    let server = Server::start("")?;
    std::fs::write(
        server.dir.join("config/fux/config.fux"),
        "(sequence! (unbind \"Nope\") #inert)",
    )
    .map_err(e)?;
    let mut terminal = Terminal::attach(
        &server,
        12,
        100,
        &["str", "titles", "true", "set", "opts", "attach"],
    )?;
    terminal.wait_for("bad_binding")?;
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
            "str shell list str /bin/sh str -c str 'printf {marker}; exec /bin/cat' end set \
             opts str main in 80 24 size str {marker} name new-tab"
        );
        let answer = server.ok(&[&code])?;
        let pane = answer.trim().split(':').next().ok_or("shell pane")?;
        eventually("shell output", || {
            Ok(server
                .ok(&[&format!("(capture {})", support::reference(pane))])?
                .contains(marker))
        })?;
        let saved = server.ok(&[&format!("(info {})", support::reference(pane))])?;
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
                .contains("only_shell")
        );
        server.ok(&[&format!(
            "(sequence! (send-keys (key {} \"C-d\")) #inert)",
            support::reference(pane)
        )])?;
        eventually("shell closes", || {
            Ok(!server.panes()?.iter().any(|(p, _)| p == pane))
        })?;
    }
    let split = server.ok(&["(info (split (axis (pane 1) \"horizontal\")))"])?;
    assert!(
        split.contains(r#""generation":0,"argv":["/bin/sh"]"#),
        "{split}"
    );
    assert!(split.contains(r#""launch":{"choice":"shell""#), "{split}");
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
    let mut sending = server.spawn(&[&format!("str {id} client str F5 key send-keys drop")])?;
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
        "false"
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
    assert!(
        recipe.contains("0 history-bytes false reflow 0 alternate-scroll-lines 0 picture-bytes false program-clipboard"),
        "{recipe}"
    );
    for (field, value) in [
        ("reflow", "false"),
        ("alternate_scroll_lines", "0"),
        ("picture_bytes", "0"),
    ] {
        assert_eq!(
            server
                .ok(&[&format!(
                    "(get {} {})",
                    fux::script::quoted(field),
                    support::reference(pane)
                )])?
                .trim(),
            value
        );
    }
    let replay = server.dir.join("pane-policy.fux");
    std::fs::write(
        &replay,
        format!("str history-bytes 8192 set str program-clipboard true set\n{recipe}"),
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
        "false"
    );
    for (field, value) in [
        ("reflow", "false"),
        ("alternate_scroll_lines", "0"),
        ("picture_bytes", "0"),
    ] {
        assert_eq!(
            server
                .ok(&[&format!("(get (pane 3) {})", fux::script::quoted(field))])?
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
        "str 'C-b g u' code me focused zoom end bind  str 'C-b g' repeat\n\
         define say-ran str ran-in-the-client print end\n\
         str 'C-b g s' code say-ran end bind\n\
         define take end  str 'C-b g r' code take end bind\n\
         str 'C-b g y' code stop-compiling digits-begin start-compiling end bind\n\
         str 'C-b g n' code me focused swap rename end bind\n\
         str 'C-b g t' code str ran-by-the-reader print end bind\n\
         str history-bytes 4096 set\n",
    )?;
    let mut terminal = Terminal::attach(&server, 10, 80, &[])?;
    terminal.wait_for("$")?;
    let listing = server.ok(&["(listing (list-session))"])?;
    let id = (listing.lines())
        .find_map(|line| line.strip_prefix("client "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or(format!("no client: {listing}"))?
        .to_owned();
    let read = |what: &str| {
        server.ok(&[&format!(
            "(list (client {}) {})",
            fux::script::quoted(&id),
            support::reference(what)
        )])
    };
    let keys = read("keys")?;
    for said in [
        r#"{"at":"","keymap":[{"kind":"map","key":"C-b","repeat":false,"keymap":["#,
        r#"{"kind":"map","key":"g","repeat":true,"keymap":[{"kind":"binding","key":"u","code":"me focused zoom","label":"me focused zoom","group":"Other"},"#,
        r#"{"kind":"binding","key":"x","code":"me-focused confirm-close","label":"close pane","group":"Panes"}"#,
        r#"{"key":"Enter","action":"submit","code":null,"label":"accept"}"#,
    ] {
        assert!(keys.contains(said), "{said} in {keys}");
    }
    // A binding of the map `C-b`, the root's first key, its code then
    // `then` run on it.
    let program = |keys: &[&str], then: &[&str]| {
        let read = ["str", &id, "client", "keys", "str", "keymap", "get"];
        let mut program = [&read[..], &["first", "str", "keymap", "get"], keys].concat();
        program.extend([&["str", "code", "get"][..], then].concat());
        program.into_iter().map(str::to_owned).collect::<Vec<_>>()
    };
    let code = |keys: &[&str], then: &[&str]| {
        let program = program(keys, then);
        server.ok(&program.iter().map(String::as_str).collect::<Vec<_>>())
    };
    let last = ["last", "str", "keymap", "get", "last"];
    assert_eq!(code(&last, &["execute"])?.trim(), "ran-by-the-reader");
    // A word compiled before the code is read runs it as `execute` does.
    let early = [
        &["define", "run-code", "execute", "end"].map(str::to_owned)[..],
        &program(&last, &["run-code"]),
    ]
    .concat();
    let early: Vec<&str> = early.iter().map(String::as_str).collect();
    assert_eq!(server.ok(&early)?.trim(), "ran-by-the-reader");
    // `C-b x` runs `me-focused confirm-close`, `me-focused` a word of the
    // bundled keys', which a client's dictionary has and the command
    // line's lacks: a code value here too, which fails as it is executed.
    assert_eq!(code(&["2", "at"], &["kind"])?.trim(), "code");
    let close = program(&["2", "at"], &["execute"]);
    let failed = server.fux(&close.iter().map(String::as_str).collect::<Vec<_>>())?;
    assert!(
        failed.stderr.contains("unknown_word") && failed.stderr.contains("me-focused"),
        "{}",
        failed.stderr
    );
    // Read in the middle of number text and with a value on the return
    // stack, beside `take`, immediate here, which `C-b g r` names, and
    // `C-b g y`, whose words begin number text as they compile.
    let midway = [
        "define",
        "take",
        "from-return",
        "drop",
        "end",
        "immediate",
        "digits-begin",
        "str",
        "kept",
        "holds",
        "7",
        "to-return",
        "str",
        &id,
        "client",
        "keys",
        "drop",
        "from-return",
        "print",
        "0",
        "digits-end",
        "print",
    ];
    assert_eq!(server.ok(&midway)?, "7\nkept\n");
    terminal.type_bytes(b"\x02g")?;
    eventually("the client in the map C-b g", || {
        Ok(read("keys")?.starts_with(r#"{"at":"C-b g","#))
    })?;
    let preferences = read("preferences")?;
    let history =
        r#"{"name":"history-bytes","value":4096,"type":"a number, 0 to 9223372036854775807"}"#;
    assert!(preferences.contains(history), "{preferences}");
    terminal.type_bytes(b"\x1b")?;
    eventually("Escape leaving the map", || {
        Ok(read("keys")?.starts_with(r#"{"at":"","#))
    })?;
    // A read code value is a callback too: `C-b g n`'s, the prompt's.
    let opens = [
        "str",
        &id,
        "client",
        "str",
        "Reuse",
        "str",
        "named-by-prompt",
    ];
    let reuse = program(&["last", "str", "keymap", "get", "4", "at"], &["prompt"]);
    let reuse = [
        &opens[..],
        &reuse.iter().map(String::as_str).collect::<Vec<_>>(),
    ]
    .concat();
    server.ok(&reuse)?;
    terminal.wait_for("Reuse")?;
    terminal.type_bytes(b"\r")?;
    let name = ["str", &id, "client", "focused", "str", "name", "get"];
    eventually("the prompt's callback ran", || {
        Ok(server.ok(&name)?.trim() == "named-by-prompt")
    })?;
    // In the client, `C-b g s`'s word is its config's, and runs.
    terminal.type_bytes(b"\x02e")?;
    terminal.wait_for("Enter accept")?;
    terminal.type_bytes(
        b"me keys str keymap get first str keymap get last str keymap get 1 at \
          str code get execute\r",
    )?;
    eventually("the code run in the client", || {
        terminal.pump();
        Ok(String::from_utf8_lossy(&terminal.output).contains("ran-in-the-client"))
    })?;
    // And bound again, as it came: `C-b g t`'s code on `C-b Q`.
    terminal.type_bytes(b"\x02e")?;
    terminal.wait_for("Enter accept")?;
    terminal.type_bytes(
        b"str 'C-b Q' me keys str keymap get first str keymap get last str keymap get last \
          str code get bind\r",
    )?;
    let rebound = r#"{"kind":"binding","key":"Q","code":"str ran-by-the-reader print""#;
    eventually("C-b Q bound", || Ok(read("keys")?.contains(rebound)))?;
    // A key bound, then read, in one program at its prompt: the read finds
    // it, as pending keys are committed before the program asks anything.
    // What it prints is joined as it runs, so the line typed never shows
    // either answer.
    terminal.type_bytes(b"\x02e")?;
    terminal.wait_for("Enter accept")?;
    terminal.type_bytes(
        b"str 'C-b y' code me focused zoom end bind  \
          me keys json str '\"key\":\"y\"' contains if str fresh else str stale then \
          str read-back- swap cat print\r",
    )?;
    let mut said = String::new();
    eventually("the program's answer", || {
        terminal.pump();
        let shown = format!(
            "{}{}",
            terminal.text(),
            String::from_utf8_lossy(&terminal.output)
        );
        said = ["read-back-fresh", "read-back-stale"]
            .into_iter()
            .find(|answer| shown.contains(answer))
            .unwrap_or_default()
            .to_owned();
        Ok(!said.is_empty())
    })?;
    assert_eq!(
        said, "read-back-fresh",
        "the read found the keys before the bind"
    );
    terminal.type_bytes(b"\x02d")?;
    terminal.wait_exit()?;
    let gone = server.fux(&[&format!("(keys (client {}))", fux::script::quoted(&id))])?;
    assert!(gone.stderr.contains("no_client"), "{}", gone.stderr);
    Ok(())
}
