//! The config: a fuxth file each client runs as it attaches. Its `set`s go
//! to the server, its `prefix`, `bind` and `unbind` make the client's keys;
//! a bad one is reported.
mod support;
use support::*;

#[test]
fn the_config_sets_the_prefix_and_bindings() -> Outcome {
    let server = Server::start(
        "str C-a prefix\n\
         str y str split-below bind\n\
         define split-below me focused vertical split drop end\n\
         str h unbind\n\
         str g str 'me focused horizontal str sh program str -c arg str \"echo from-g; exec sh\" arg split drop' bind\n\
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

/// A config's `set`s reach the server as it starts; `set` from the command
/// line changes a running one; bad values are refused.
#[test]
fn settings_reach_the_server() -> Outcome {
    // Three rows of two cells or more: 24 bytes a row and 8 a cell.
    let server = Server::start("str history-bytes 120 set")?;
    // History bytes apply to new panes.
    let pane = server.ok(&["str", "%1", "pane", "vertical", "split"])?;
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
    // A new shell program applies to new panes.
    server.ok(&[
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
    ])?;
    let custom = server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
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

/// Half a config is no config: one that fails after a `set` sets nothing,
/// and says why.
#[test]
fn a_config_that_fails_sets_nothing() -> Outcome {
    let server = Server::start("")?;
    let config = server.dir.join("half.fux");
    let text = "str shell list str /usr/bin/env str PS1=custom$ str /bin/sh end set\n\
                str nonsense 1 set\n";
    std::fs::write(&config, text).map_err(e)?;
    let socket = fux::socket::Choice::Path(server.socket.clone());
    let ran = fux::script::run_file(&config, Some(&mut fux::script::Server::new(socket)), None);
    let failed = ran.err().ok_or("the config ran")?;
    assert!(failed.to_string().contains("nonsense"), "{failed}");
    let pane = server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
    let pane = pane.split_whitespace().next().unwrap_or_default();
    let shown = || server.ok(&["str", pane, "pane", "capture"]);
    eventually("a prompt", || Ok(shown()?.contains('$')))?;
    assert!(!shown()?.contains("custom$"), "the shell was set");
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
    let ok = "str v str sr bind\ndefine sr me focused horizontal split drop end\n";
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
    assert!(server.ok(&["list-keys"])?.contains("prefix"));
    std::os::unix::fs::symlink("gone", &path).map_err(e)?;
    let broken = server.fux(&["list-keys"])?;
    assert_eq!(broken.status, 1);
    assert!(broken.stderr.contains("config.fux"), "{}", broken.stderr);
    std::fs::remove_file(&path).map_err(e)?;
    std::os::unix::fs::symlink("config.fux", &path).map_err(e)?;
    let looping = server.fux(&["list-keys"])?;
    assert_eq!(looping.status, 1);
    assert!(looping.stderr.contains("config.fux"), "{}", looping.stderr);
    let mut terminal = Terminal::attach(&server, 10, 80, &[])?;
    terminal.wait_for("read:")?;
    terminal.type_bytes(b"\x02")?;
    terminal.wait_for("Commands")?;
    terminal.type_bytes(b"d")?;
    terminal.wait_exit()?;
    std::fs::remove_file(&path).map_err(e)?;
    std::fs::create_dir(&path).map_err(e)?;
    assert_eq!(server.fux(&["list-keys"])?.status, 1);
    std::fs::remove_dir(&path).map_err(e)?;
    std::fs::write(&path, [0xff]).map_err(e)?;
    assert!(server.fux(&["list-keys"])?.stderr.contains("not UTF-8"));
    Ok(())
}

#[test]
fn config_notices_show_the_cause_once_before_the_path() -> Outcome {
    let server = Server::start("")?;
    let path = server.dir.join("config/fux/config.fux");
    std::fs::write(&path, "str bell 3 set").map_err(e)?;
    let failure = fux::script::run_file(&path, None, None)
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
    server.ok(&["%1", "split", "drop"])?;
    Ok(())
}

/// A supplied empty snapshot survives the request connection and server spawn.
#[test]
fn an_empty_caller_environment_is_not_replaced() -> Outcome {
    use fux::script::{Host, Server as Requests, compile, run_capped};
    let server = Server::start("str update-environment list str HOME str ENV end set")?;
    let tokens = fuxth::token::source(
        "test",
        "opts 80 24 size str /bin/sh program str -c arg str 'echo HOME=${HOME-unset} ENV=${ENV-unset}' arg run",
    )
    .map_err(e)?;
    let mut machine = compile(&tokens).map_err(e)?;
    let context = fux::protocol::Context {
        env: Some(Vec::new()),
        cwd: server.dir.clone(),
        ..Default::default()
    };
    let mut sink = Vec::new();
    let mut host = Host::new(context, None, &mut sink);
    let mut requests = Requests::new(fux::socket::Choice::Path(server.socket.clone()));
    let values =
        run_capped(&mut machine, &mut host, &mut requests, Some(10000), None).map_err(e)?;
    let version = values.first().ok_or("run result")?.text();
    let pane = version.split(':').next().ok_or("pane")?;
    server.exits(pane)?;
    let captured = server.ok(&["str", pane, "pane", "capture"])?;
    assert_eq!(captured.trim(), "HOME=unset ENV=unset");
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

/// `update-environment` names what a new pane takes from the client that
/// made it: one the client has is passed on, as the bytes it is, UTF-8 or
/// not; one it lacks is removed, even though the server has it (here
/// `ENV`, which the test server is given).
#[test]
fn update_environment_takes_the_clients_variables_or_none() -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let server =
        Server::start("str update-environment list str ENV str FUXT_GIVEN str FUXT_BYTES end set")?;
    let shown = "echo \"[${ENV-unset}] [${FUXT_GIVEN-unset}]\"; \
                 printf %s \"${FUXT_BYTES-unset}\" | od -An -tx1";
    let out = std::process::Command::new(FUX)
        .args([
            "opts",
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
    Ok(())
}
