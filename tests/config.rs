//! The config: a fuxth file each client runs as it attaches. Its `set`s go
//! to the server, its `prefix`, `bind` and `unbind` make the client's keys;
//! a bad one is reported.
mod support;
use support::*;

#[test]
fn the_config_sets_the_prefix_and_bindings() -> Outcome {
    let server = Server::start(
        "str C-a prefix\n\
         str y str 'me focused opts vertical split drop' bind\n\
         str h unbind\n\
         str g str 'me focused opts horizontal str \"echo from-g\" typed split drop' bind\n\
         str Tools group\n",
    )?;
    let mut client = server.attach(24, 90)?;
    client.wait_for("$")?;
    client.ready()?;
    // C-a y splits; C-b is an ordinary key now.
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
    client.wait("from-g typed into a new pane", |t| {
        t.lines().any(|l| l.rsplit('│').next() == Some("from-g"))
    })?;
    Ok(())
}

/// A config's `set`s reach the server as it starts; `set` from the command
/// line changes a running one; bad values are refused.
#[test]
fn settings_reach_the_server() -> Outcome {
    // Three rows of two cells or more: 24 bytes a row and 8 a cell.
    let server = Server::start("str history-bytes 120 set")?;
    // History bytes apply to new panes.
    let pane = server.ok(&["str", "%1", "pane", "opts", "vertical", "split"])?;
    let pane = pane.split_whitespace().next().unwrap_or_default();
    server.type_line(
        pane,
        "for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do echo h$i; done",
    )?;
    eventually("the loop", || {
        Ok(server.ok(&["str", pane, "pane", "screen"])?.contains("h15"))
    })?;
    let history = server.ok(&["str", pane, "pane", "opts", "100", "history", "capture"])?;
    let rows = history.lines().count();
    let screen = server.ok(&["str", pane, "pane", "screen"])?.lines().count();
    assert!(rows <= screen + 3, "at most 3 lines of history: {history}");
    // A new shell program applies to new panes.
    server.ok(&[
        "str",
        "shell",
        "str",
        "/usr/bin/env PS1=custom$ /bin/sh",
        "set",
    ])?;
    let custom = server.ok(&["str", "%1", "pane", "opts", "horizontal", "split"])?;
    eventually("the custom prompt", || {
        Ok(server
            .ok(&[
                "str",
                custom.split_whitespace().next().unwrap_or_default(),
                "pane",
                "screen",
            ])?
            .contains("custom$"))
    })?;
    // Bad values are refused; keys are a client's, never the server's.
    for args in [
        &["str", "buffers", "0", "set"][..],
        &["str", "clipboard", "str", "maybe", "set"],
        &["str", "nonsense", "1", "set"],
        &["str", "C-a", "prefix"],
        &["str", "q", "str", "me zoom", "bind"],
    ] {
        let out = server.fux(args)?;
        assert_eq!(out.status, 1, "{args:?}: {}", out.stderr);
    }
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
    terminal.wait_for("config:")?;
    // The defaults: C-b opens the column.
    terminal.type_bytes(b"\x02")?;
    terminal.wait_for("Commands")?;
    terminal.type_bytes(b"d")?;
    terminal.wait_exit()?;
    // An unknown word is found before anything runs, naming its line.
    std::fs::write(&config, "str history-bytes 5 set\nfrobnicate\n").map_err(e)?;
    let path = config.to_string_lossy().into_owned();
    let checked = server.fux(&["str", &path, "check"])?;
    assert_eq!(checked.status, 1);
    assert!(
        checked.stderr.contains("config.fux:2: frobnicate"),
        "{}",
        checked.stderr
    );
    std::fs::write(&config, "str C-a prefix\n").map_err(e)?;
    assert!(server.ok(&["str", &path, "check"])?.ends_with(": ok\n"));
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
        let checked = server.fux(&["str", &path, "check"])?;
        assert_eq!(checked.status, 0, "{example}: {}", checked.stderr);
    }
    Ok(())
}

/// `update-environment` names what a new pane takes from the client that
/// made it: one the client has is passed on, as the bytes it is, UTF-8 or
/// not; one it lacks is removed, even though the server has it (here
/// `ENV`, which the test server is given).
#[test]
fn update_environment_takes_the_clients_variables_or_none() -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let server = Server::start("str update-environment str 'ENV FUXT_GIVEN FUXT_BYTES' set")?;
    let shown = "echo \"[${ENV-unset}] [${FUXT_GIVEN-unset}]\"; \
                 printf %s \"${FUXT_BYTES-unset}\" | od -An -tx1";
    let out = std::process::Command::new(FUX)
        .args([
            "opts",
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[unset] [yes]"), "{stdout}");
    // od spaces its bytes as each system likes.
    let bytes = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(bytes.contains("63 61 66 e9"), "{stdout}");
    Ok(())
}
