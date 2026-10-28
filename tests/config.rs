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
        "str wait-seconds 0 set\nstr C-a prefix\n",
    )
    .map_err(e)?;
    // With the bundled wait, this waits for the child's output/exit. A
    // leaked wait=0 instead answers timeout before the child writes.
    let waiting = [
        "opts",
        "str",
        "main",
        "in",
        "80",
        "24",
        "size",
        "str",
        "/bin/sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "sleep 1; echo observed",
        "arg",
        "run",
        "wait",
    ];
    // Exercise start on a fresh socket: an already running server would
    // bypass the old startup config path. Clean up even on a refusal.
    let socket = server.dir.join("started/fux.sock");
    let socket = socket.to_str().ok_or("a text socket path")?;
    let env = [("FUX_SOCKET", socket)];
    let started = (|| {
        let out = server.fux_env(&["start"], &env)?;
        if out.status != 0 || !out.stderr.is_empty() {
            return Err(format!("start: {}", out.stderr));
        }
        let out = server.fux_env(&waiting, &env)?;
        if out.status != 0 {
            return Err(format!("start loaded the config: {}", out.stderr));
        }
        Ok(())
    })();
    let _ = server.fux_env(&["force-kill-server"], &env);
    started?;
    assert!(server.ok(&["list-keys"])?.contains("str C-b prefix"));
    for args in [
        &["opts", "watch", "attach"][..],
        &["opts", "80", "10", "size", "attach"],
        &["opts", "80", "10", "size", "watch", "attach"],
    ] {
        let mut terminal = Terminal::attach(&server, 10, 80, args)?;
        let mut id = String::new();
        eventually("the attached client", || {
            id = server
                .ok(&["ls"])?
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
        server.ok(&[
            "str",
            &id,
            "client",
            "str",
            "C-b",
            "key",
            "str",
            "d",
            "key",
            "send-keys",
        ])?;
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
        "str C-a prefix\n\
         str y str split-below bind\n\
         define split-below me focused str vertical axis split drop end\n\
         str h unbind\n\
         str g str 'me focused str horizontal axis str sh program str -c arg str \"echo from-g; exec sh\" arg split drop' bind\n\
         str Tools group\n",
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
    client.wait("the column still open", |t| t.contains("Commands"))?;
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
    let server = Server::start("str history-bytes 120 set")?;
    // History bytes apply to new panes.
    let pane = server.ok(&[
        "%1",
        "str",
        "vertical",
        "axis",
        "120",
        "history-bytes",
        "split",
    ])?;
    let pane = pane.split(':').next().unwrap_or_default();
    server.type_line(
        pane,
        "for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do echo h$i; done",
    )?;
    eventually("the loop", || {
        Ok(server
            .ok(&["str", pane, "pane", "capture"])?
            .contains("h15"))
    })?;
    let history = server.ok(&["str", pane, "pane", "100", "history", "capture"])?;
    let rows = history.lines().count();
    let screen = server
        .ok(&["str", pane, "pane", "capture"])?
        .lines()
        .count();
    assert!(rows <= screen + 3, "at most 3 lines of history: {history}");
    // Checking a config and a failing local script leave launch policy alone.
    let checked = server.dir.join("checked.fux");
    std::fs::write(&checked, "str shell list str /no/such/check-shell end set").map_err(e)?;
    server.ok(&["str", &checked.to_string_lossy(), "str", "config", "check"])?;
    let failed = server.fux(&[
        "str",
        "shell",
        "list",
        "str",
        "/no/such/failed-shell",
        "end",
        "set",
        "str",
        "Nope",
        "prefix",
    ])?;
    assert_eq!(failed.status, 1);
    // A new shell program applies to new panes.
    let custom = server.ok(&[
        "str",
        "shell",
        "list",
        "str",
        "/usr/bin/env",
        "str",
        "PS1=custom$",
        "str",
        "/bin/sh",
        "end",
        "set",
        "%1",
        "str",
        "horizontal",
        "axis",
        "split",
    ])?;

    eventually("the custom prompt", || {
        Ok(server
            .ok(&[
                "str",
                custom.split_whitespace().next().unwrap_or_default(),
                "pane",
                "capture",
            ])?
            .contains("custom$"))
    })?;
    // Bad values are refused; keys are a client's, never the server's.
    for args in [
        &["str", "buffers", "0", "set"][..],
        &["str", "terminal-clipboard", "str", "maybe", "set"],
        &["str", "nonsense", "1", "set"],
    ] {
        let out = server.fux(args)?;
        assert_eq!(out.status, 1, "{args:?}: {}", out.stderr);
    }
    Ok(())
}

/// An imported config cannot act on the server or commit half its local state.
#[test]
fn an_imported_failure_keeps_the_whole_bundled_state() -> Outcome {
    let server = Server::start("str wait-seconds 0 set")?;
    let directory = server.dir.join("config/fux");
    let mut a = server.attach(8, 80)?;
    let mut b = server.attach(8, 80)?;
    a.wait_for("$")?;
    b.wait_for("$")?;
    let before = server.ok(&["ls"])?;
    std::fs::write(directory.join("config.fux"), "require shared.fux").map_err(e)?;
    std::fs::write(
        directory.join("shared.fux"),
        "str wait-seconds 2 set str titles true set str history-bytes 0 set",
    )
    .map_err(e)?;
    let configured = fux::script::run_file(&directory.join("config.fux")).map_err(e)?;
    assert_eq!(configured.preferences.wait.seconds(), 2);
    assert!(configured.preferences.titles);
    server.ok(&[
        "str",
        &directory.join("config.fux").to_string_lossy(),
        "str",
        "config",
        "check",
    ])?;
    assert_eq!(server.ok(&["ls"])?, before);
    a.pump()?;
    b.pump()?;
    assert!(!String::from_utf8_lossy(&a.painted).contains("\x1b]2;"));
    assert!(!String::from_utf8_lossy(&b.painted).contains("\x1b]2;"));
    std::fs::write(
        directory.join("shared.fux"),
        "str C-a prefix str wait-seconds 0 set str titles true set \
         opts str missing in attach-defaults \
         define only-in-failed-config 41 end \
         %1 str horizontal axis split drop",
    )
    .map_err(e)?;
    let before = server.ok(&["ls"])?;
    let failed = fux::script::run_file(&directory.join("config.fux"))
        .err()
        .ok_or("config acted on the server")?;
    assert_eq!(failed.code, fux::code::Code::NotConfig);
    assert!(failed.notice().contains("shared.fux"));
    assert_eq!(server.ok(&["ls"])?, before);
    let mut terminal = Terminal::attach(&server, 10, 100, &[])?;
    terminal.wait_for("a config changes only its own client state")?;
    terminal.type_bytes(b"\x02")?;
    terminal.wait_for("Commands")?;
    terminal.type_bytes(b"d")?;
    terminal.wait_exit()?;
    assert_eq!(server.panes()?.len(), 1);
    let probe = server.fux(&[
        "agent-opts",
        "80",
        "24",
        "size",
        "str",
        "/bin/sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "sleep 1",
        "arg",
        "run",
        "wait",
    ])?;
    assert_eq!(
        probe.status, 0,
        "a failed config changed another invocation's wait: {}",
        probe.stderr
    );
    Ok(())
}

/// A config that fails is reported to the client attaching with it, which
/// keeps the default keys; `check` finds its mistakes without running it.
#[test]
fn a_bad_config_is_reported() -> Outcome {
    let server = Server::start("")?;
    let config = server.dir.join("config").join("fux").join("config.fux");
    std::fs::write(&config, "str Nope prefix\n").map_err(e)?;
    let mut terminal = Terminal::attach(&server, 10, 100, &[])?;
    terminal.wait_for("bad_binding: prefix:")?;
    // The defaults: C-b opens the column.
    terminal.type_bytes(b"\x02")?;
    terminal.wait_for("Commands")?;
    terminal.type_bytes(b"d")?;
    terminal.wait_exit()?;
    // An unknown word is found before anything runs, naming its line.
    std::fs::write(&config, "str history-bytes 5 set\nfrobnicate\n").map_err(e)?;
    let path = config.to_string_lossy().into_owned();
    let checked = server.fux(&["str", &path, "str", "config", "check"])?;
    assert_eq!(checked.status, 1);
    assert!(
        checked.stderr.contains("config.fux:2: frobnicate"),
        "{}",
        checked.stderr
    );
    std::fs::write(&config, "str C-a prefix\n").map_err(e)?;
    assert!(
        server
            .ok(&["str", &path, "str", "config", "check"])?
            .ends_with(": ok\n")
    );
    // A binding sees the config's definitions, written anywhere in it; a
    // mistake in one, or in a binding, is said at its own line and word.
    let ok = "str v str sr bind\ndefine sr me focused str horizontal axis split drop end\n";
    std::fs::write(&config, ok).map_err(e)?;
    assert!(
        server
            .ok(&["str", &path, "str", "config", "check"])?
            .ends_with(": ok\n")
    );
    for (text, said) in [
        (
            "str C-a prefix\nstr v str sr bind\n",
            "config.fux:2: sr: no such word; text is `str WORD`, in the binding for v",
        ),
        (
            "define go %2 capture str x text end\n",
            "config.fux:1: text: takes the pane",
        ),
        (
            "5 let n\ndefine go n end\nstr v str go bind\n",
            "config.fux:3: go: reads let names",
        ),
    ] {
        std::fs::write(&config, text).map_err(e)?;
        let checked = server.fux(&["str", &path, "str", "config", "check"])?;
        assert_eq!(checked.status, 1, "{text}");
        let stderr = checked
            .stderr
            .strip_prefix("fux: check: ")
            .unwrap_or_default();
        assert!(
            stderr.starts_with(&path.replace("config.fux", "")),
            "{}",
            checked.stderr
        );
        assert!(stderr.contains(said), "{}", checked.stderr);
    }
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
    terminal.wait_for("Commands")?;
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
        assert!(server.ok(&["list-keys"])?.contains("prefix"));
        let mut terminal = Terminal::attach(&server, 10, 80, &[])?;
        terminal.wait_for("read:")?;
        if kind == "non-text" {
            terminal.wait_for("not UTF-8")?;
        }
        terminal.type_bytes(b"\x02")?;
        terminal.wait_for("Commands")?;
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
    std::fs::write(&path, "str bell 3 set").map_err(e)?;
    let failure = fux::script::run_file(&path)
        .err()
        .ok_or("expected a bad setting")?;
    assert!(failure.place.is_some());
    let notice = failure.notice();
    assert!(
        notice.starts_with("config: set: bell: takes true or false"),
        "{notice}"
    );
    assert_eq!(notice.matches("config:").count(), 1);
    assert!(notice.ends_with("config.fux:1)"), "{notice}");
    let mut terminal = Terminal::attach(&server, 10, 80, &[])?;
    terminal.wait_for("bell: takes true or false")?;
    terminal.type_bytes(b"\x02d")?;
    terminal.wait_exit()?;
    Ok(())
}

#[test]
fn check_mode_is_explicit_and_independent_of_the_filename() -> Outcome {
    let server = Server::start("")?;
    for name in ["sample.fux", "config.fux", "arbitrary"] {
        let path = server.dir.join(name);
        std::fs::write(&path, "str no-such-setting true set").map_err(e)?;
        let path = path.to_string_lossy();
        assert!(
            server
                .ok(&["str", &path, "str", "program", "check"])?
                .ends_with(": ok\n")
        );
        let checked = server.fux(&["str", &path, "str", "config", "check"])?;
        assert_eq!(checked.status, 1);
        assert!(checked.stderr.contains("no setting"), "{}", checked.stderr);
        assert_eq!(
            server.fux(&["str", &path, "str", "auto", "check"])?.status,
            1
        );
        assert_eq!(server.fux(&["str", &path, "check"])?.status, 1);
    }
    let path = server.dir.join("dry.fux");
    std::fs::write(&path, "str shell list str /not-a-program end set").map_err(e)?;
    server.ok(&["str", &path.to_string_lossy(), "str", "config", "check"])?;
    server.ok(&["%1", "str", "horizontal", "axis", "split", "drop"])?;
    Ok(())
}

/// Selected absence is saved even when the patch has no present values.
#[test]
fn an_absent_selection_unsets_the_server_value() -> Outcome {
    let server = Server::start("")?;
    let code = "str copy-environment list str ENV end set agent-opts 80 24 size \
        str /bin/sh program str -c arg str 'echo ${ENV-unset}' arg run";
    let out = std::process::Command::new(FUX)
        .arg(code)
        .env("FUX_SOCKET", &server.socket)
        .env_remove("FUX_PANE")
        .env_remove("FUX_SERVER")
        .env_remove("ENV")
        .output()
        .map_err(e)?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let answer = String::from_utf8(out.stdout).map_err(e)?;
    let pane = answer.trim().split(':').next().ok_or("pane")?;
    server.exits(pane)?;
    assert_eq!(server.ok(&[pane, "capture"])?.trim(), "unset");
    assert!(server.ok(&["all", "dump"])?.contains("str ENV unset-env"));
    Ok(())
}

/// The examples of the words a config uses check as a config: none binds
/// after a key bound by default.
#[test]
fn the_config_words_examples_check() -> Outcome {
    let server = Server::start("")?;
    let config = server.dir.join("config.fux");
    let path = config.to_string_lossy().into_owned();
    for word in ["bind", "bind-root", "prefix"] {
        let explained = server.ok(&["str", word, "explain"])?;
        let example = (explained.lines())
            .find_map(|line| line.split_once("; e.g. "))
            .map(|(_, example)| example)
            .ok_or(format!("no example: {explained}"))?;
        std::fs::write(&config, example).map_err(e)?;
        let checked = server.fux(&["str", &path, "str", "config", "check"])?;
        assert_eq!(checked.status, 0, "{example}: {}", checked.stderr);
    }
    Ok(())
}

/// `copy-environment` names what a new pane takes from the client that
/// made it: one the client has is passed on, as the bytes it is, UTF-8 or
/// not; one it lacks is removed, even though the server has it (here
/// `ENV`, which the test server is given).
#[test]
fn copy_environment_takes_the_clients_variables_or_none() -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let server =
        Server::start("str copy-environment list str ENV str FUXT_GIVEN str FUXT_BYTES end set")?;
    let shown = "echo \"[${ENV-unset}] [${FUXT_GIVEN-unset}]\"; \
                 printf %s \"${FUXT_BYTES-unset}\" | od -An -tx1";
    let out = std::process::Command::new(FUX)
        .args([
            "agent-opts",
            "list",
            "str",
            "ENV",
            "str",
            "FUXT_GIVEN",
            "str",
            "FUXT_BYTES",
            "end",
            "copy-environment",
            "80",
            "24",
            "size",
            "str",
            "sh",
            "program",
            "str",
            "-c",
            "arg",
            "str",
            shown,
            "arg",
            "str",
            "FUXT_OTHER",
            "str",
            "x",
            "env",
            "run",
        ])
        .env("FUX_SOCKET", &server.socket)
        .env("XDG_CONFIG_HOME", server.dir.join("config"))
        .env_remove("FUX_PANE")
        .env_remove("ENV")
        .env("FUXT_GIVEN", "yes")
        .env("FUXT_BYTES", std::ffi::OsStr::from_bytes(b"caf\xe9"))
        .output()
        .map_err(e)?;
    let pane = String::from_utf8_lossy(&out.stdout);
    let pane = pane.split(':').next().ok_or("its pane")?;
    server.exits(pane)?;
    let stdout = server.ok(&["str", pane, "pane", "capture"])?;
    assert!(stdout.contains("[unset] [yes]"), "{stdout}");
    // od spaces its bytes as each system likes.
    let bytes = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(bytes.contains("63 61 66 e9"), "{stdout}");
    assert!(
        server
            .ok(&["all", "dump"])?
            .contains("environment FUXT_BYTES is not UTF-8 text")
    );
    Ok(())
}

#[test]
fn a_program_uses_preferences_before_its_next_operation() -> Outcome {
    let server = Server::start("")?;
    let result = server.fux(&[
        "str",
        "shell",
        "list",
        "str",
        "/no/such/fux-review-shell",
        "end",
        "set",
        "%1",
        "str",
        "horizontal",
        "axis",
        "split",
        "drop",
    ])?;
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
    let result = server.fux(&[
        "str",
        "wait-seconds",
        "0",
        "set",
        "agent-opts",
        "80",
        "24",
        "size",
        "str",
        "/bin/sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "sleep 1",
        "arg",
        "run",
        "wait",
    ])?;
    assert_eq!(
        result.status, 124,
        "wait used the previous limit: {}",
        result.stderr
    );
    Ok(())
}

/// A new small pane uses its invocation's history budget.
fn history_rows(server: &Server) -> Result<usize, String> {
    let answer = server.ok(&[
        "agent-opts",
        "80",
        "5",
        "size",
        "str",
        "/bin/sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        "i=0; while [ \"$i\" -lt 40 ]; do echo row; i=$((i+1)); done",
        "arg",
        "run",
    ])?;
    let pane = answer.trim().split(':').next().ok_or("pane")?;
    server.exits(pane)?;
    Ok(server
        .ok(&[pane, "all-history", "capture"])?
        .lines()
        .count())
}

/// Local creation defaults never become server policy when selecting servers.
#[test]
fn launch_defaults_do_not_change_either_selected_server() -> Outcome {
    for named in [true, false] {
        let a = Server::start("")?;
        let b = Server::start("")?;
        let select = if named {
            "str fux server".to_owned()
        } else {
            format!(
                "str {} socket",
                fux::words::quote(&b.socket.to_string_lossy())
            )
        };
        let runtime = [("XDG_RUNTIME_DIR", b.dir.to_str().ok_or("runtime path")?)];
        let code = format!("str history-bytes 0 set {select}");
        let out = a.fux_env(&[&code], &runtime)?;
        assert_eq!(out.status, 0, "{}", out.stderr);
        assert!(history_rows(&a)? > 5, "local budget changed the old server");
        assert!(
            history_rows(&b)? > 5,
            "local budget changed the newly selected server"
        );
        let out = a.fux_env(&[&format!("{select} str history-bytes 0 set ls")], &runtime)?;
        assert_eq!(out.status, 0, "{}", out.stderr);
        assert!(history_rows(&a)? > 5);
        assert!(history_rows(&b)? > 5);
    }
    Ok(())
}

#[test]
fn key_words_refuse_the_command_line() -> Outcome {
    let server = Server::start("")?;
    for words in [
        &["str", "C-a", "prefix"][..],
        &["str", "q", "str", "me zoom", "bind"],
        &["str", "F5", "str", "me zoom", "bind-root"],
        &["str", "q", "unbind"],
        &["str", "F5", "unbind-root"],
        &["unbind-all"],
        &["str", "copy", "str", "g", "str", "top", "bind-in"],
        &["str", "copy", "str", "v", "unbind-in"],
        &["str", "name", "label"],
        &["str", "name", "group"],
        &["repeat"],
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
    assert!(String::from_utf8_lossy(&result.stdout).contains("str C-b prefix"));
    let diagnostic = String::from_utf8_lossy(&result.stderr);
    assert!(diagnostic.contains("SHELL"), "{diagnostic}");
    assert_eq!(diagnostic.lines().count(), 1, "{diagnostic}");
    Ok(())
}

#[test]
fn explicit_settings_survive_a_failed_attach_config() -> Outcome {
    let server = Server::start("")?;
    std::fs::write(server.dir.join("config/fux/config.fux"), "str Nope prefix").map_err(e)?;
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
            Ok(server.ok(&[pane, "capture"])?.contains(marker))
        })?;
        let saved = server.ok(&[pane, "info", "json"])?;
        assert!(
            saved.contains(&format!("printf {marker}; exec /bin/cat")),
            "{saved}"
        );
        assert!(
            server
                .fux(&[pane, "respawn"])?
                .stderr
                .contains("runs a shell")
        );
        assert!(
            server
                .fux(&[pane, "str", "TERM", "signal"])?
                .stderr
                .contains("only_shell")
        );
        server.ok(&[pane, "str", "C-d", "key", "send-keys", "drop"])?;
        eventually("shell closes", || {
            Ok(!server.panes()?.iter().any(|(p, _)| p == pane))
        })?;
    }
    assert!(
        server
            .ok(&["%1", "str", "horizontal", "axis", "split", "info", "json"])?
            .contains("\"launch_argv\":[\"/bin/sh\"]")
    );
    let before = server.panes()?.len();
    for (conflict, code) in [
        (
            "opts str main in 80 24 size list str /bin/sh end shell-program str /bin/cat program new-tab",
            "unused",
        ),
        (
            "opts str main in 80 24 size str /bin/cat program list str /bin/sh end shell-program new-tab",
            "unused",
        ),
        (
            "agent-opts 80 24 size list str /bin/sh end shell-program run",
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
    let code = "str copy-environment list str F4_VALUE str ENV end set \
        agent-opts 80 24 size str /bin/sh program str -c arg \
        str 'printf \"%s|%s\" \"$F4_VALUE\" \"${ENV-unset}\"' arg run";
    let out = std::process::Command::new(FUX)
        .arg(code)
        .env("FUX_SOCKET", &server.socket)
        .env_remove("FUX_PANE")
        .env_remove("FUX_SERVER")
        .env_remove("ENV")
        .env("F4_VALUE", "saved")
        .output()
        .map_err(e)?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let answer = String::from_utf8(out.stdout).map_err(e)?;
    let pane = answer.trim().split(':').next().ok_or("program pane")?;
    server.exits(pane)?;
    assert_eq!(server.ok(&[pane, "capture"])?.trim(), "saved|unset");
    // A new caller's selection never replaces the saved patch on respawn.
    server.ok(&[&format!(
        "str copy-environment list str HOME end set {pane} respawn drop"
    )])?;
    server.exits(pane)?;
    assert_eq!(server.ok(&[pane, "capture"])?.trim(), "saved|unset");
    let dump = server.ok(&["all", "dump"])?;
    assert!(dump.contains("list end copy-environment"), "{dump}");
    assert!(dump.contains("str ENV unset-env"), "{dump}");
    assert!(dump.contains("str F4_VALUE str saved env"), "{dump}");
    // Changing local selection before replay cannot add a new selected name.
    let replay = format!("str copy-environment list str F4_LEAK end set\n{dump}");
    let _ = server.ok(&[&replay])?;
    assert_eq!(
        server.ok(&["all", "dump"])?.matches("str F4_LEAK").count(),
        0
    );
    Ok(())
}

/// A remote client action commits while its subsequent wait is still pending.
#[test]
fn presentation_updates_only_the_addressed_view_before_a_wait_finishes() -> Outcome {
    let server = Server::start("")?;
    let mut a = server.attach(8, 60)?;
    let mut b = server.attach(8, 60)?;
    a.wait_for("$")?;
    b.wait_for("$")?;
    let mut requests = fux::client::Requests::open(&server.socket_path()?).map_err(e)?;
    let asked = fux::protocol::Asked {
        request: fux::command::Command::Client {
            client: Some("c1".parse().map_err(e)?),
            action: fux::command::ClientAction::Run(
                "str titles true set %1 30 timeout wait drop".into(),
            ),
        }
        .into(),
        context: Default::default(),
        after: None,
    };
    let reply = requests.ask(&asked, &mut std::io::sink()).map_err(e)?;
    assert_eq!(reply.end(), fux::protocol::End::Done);
    eventually("committed title before the wait finishes", || {
        a.pump()?;
        b.pump()?;
        Ok(String::from_utf8_lossy(&a.painted).contains("\x1b]2;"))
    })?;
    assert!(!String::from_utf8_lossy(&b.painted).contains("\x1b]2;"));
    Ok(())
}

#[test]
fn pane_creation_overrides_preserve_zero_and_false_and_dump_replays_them() -> Outcome {
    let server = Server::start("")?;
    let created = server.ok(&["str history-bytes 4096 set str program-clipboard true set agent-opts 40 6 size 0 history-bytes false program-clipboard str /bin/cat program run"])?;
    let pane = created.trim().split(':').next().ok_or("pane")?;
    assert_eq!(
        server.ok(&[pane, "str", "history_bytes", "get"])?.trim(),
        "0"
    );
    assert_eq!(
        server
            .ok(&[pane, "str", "program_clipboard", "get"])?
            .trim(),
        "false"
    );
    server.ok(&[
        "str",
        "history-bytes",
        "1024",
        "set",
        "str",
        "program-clipboard",
        "true",
        "set",
    ])?;
    assert_eq!(
        server.ok(&[pane, "str", "history_bytes", "get"])?.trim(),
        "0"
    );
    let recipe = server.ok(&[pane, "tab", "dump"])?;
    assert!(
        recipe.contains("0 history-bytes false program-clipboard"),
        "{recipe}"
    );
    let replay = server.dir.join("pane-policy.fux");
    std::fs::write(
        &replay,
        format!("str history-bytes 8192 set str program-clipboard true set\n{recipe}"),
    )
    .map_err(e)?;
    server.ok(&["--file", &replay.to_string_lossy()])?;
    assert_eq!(
        server.ok(&["%3", "str", "history_bytes", "get"])?.trim(),
        "0"
    );
    assert_eq!(
        server
            .ok(&["%3", "str", "program_clipboard", "get"])?
            .trim(),
        "false"
    );
    Ok(())
}
