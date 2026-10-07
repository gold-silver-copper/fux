//! Milestone 6: the config file, set, bind, unbind, reload, and the prefix.
mod support;
use support::*;

#[test]
fn the_config_file_sets_the_prefix_and_bindings() -> Outcome {
    let server = Server::start(
        "set prefix C-a\nbind y split -v\nunbind h\nbind -g Tools g split -h -- echo from-g",
    )?;
    let mut client = server.attach(24, 90)?;
    client.wait_for("$")?;
    // C-a y splits; C-b is an ordinary key now.
    client.keys("\x01y")?;
    eventually("split by C-a y", || Ok(focused(&server)? == "%2"))?;
    // The prefix twice sends it to the pane.
    client.keys("cat -v\r")?;
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
    client.wait("from-g typed into a new pane", |t| {
        t.lines()
            .any(|l| l.ends_with("from-g") && !l.contains("echo"))
    })?;
    Ok(())
}

/// Keys bound without the prefix act at once; keys after it match as typed,
/// and may be any key.
#[test]
fn keys_bind_by_case_any_key_and_without_the_prefix() -> Outcome {
    let server = Server::start("bind -n M-t new-tab\nbind V split -v\nbind Up split -h")?;
    let mut client = server.attach(24, 90)?;
    client.wait_for("$")?;
    // Alt-t, as a legacy terminal sends it.
    client.keys("\x1bt")?;
    eventually("a tab from M-t", || Ok(focused(&server)? == "%2"))?;
    client.keys("\x02V")?;
    eventually("a split from C-b V", || Ok(focused(&server)? == "%3"))?;
    client.keys("\x02\x1b[A")?;
    eventually("a split from C-b Up", || Ok(focused(&server)? == "%4"))?;
    // Caps Lock: an upper-case letter bound to nothing is not its lower case.
    client.keys("\x02T")?;
    client.wait("the notice", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("C-b T is not bound"))
    })?;
    client.keys("\x1b")?;
    let keys = server.ok(&["list-keys"])?;
    assert!(keys.contains("Without the prefix (bind -n):"), "{keys}");
    assert!(keys.contains("M-t  new-tab"), "{keys}");
    Ok(())
}

#[test]
fn set_and_bind_change_a_running_server() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(24, 90)?;
    client.wait_for("$")?;
    server.ok(&["bind", "q", "split", "-h"])?;
    client.keys("\x02q")?;
    eventually("split by the new binding", || Ok(focused(&server)? == "%2"))?;
    server.ok(&["unbind", "q"])?;
    assert!(!server.ok(&["list-keys"])?.contains("split -h\n        q"));
    // A new shell program applies to new panes.
    server.ok(&["set", "shell", "/usr/bin/env PS1=custom\\$ /bin/sh"])?;
    server.ok(&["split", "-v", "-t", "%2"])?;
    client.wait_for("custom$")?;
    // History lines apply to new panes.
    server.ok(&["set", "history-lines", "3"])?;
    server.ok(&["split", "-v", "-t", "%1"])?;
    server.ok(&[
        "send-keys",
        "-t",
        "%4",
        "for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do echo h$i; done",
        "Enter",
    ])?;
    eventually("the loop", || {
        Ok(server.ok(&["capture-pane", "-t", "%4"])?.contains("h15"))
    })?;
    let history = server.ok(&["capture-pane", "-t", "%4", "-S", "-100"])?;
    let rows = history.lines().count();
    let screen = server.ok(&["capture-pane", "-t", "%4"])?.lines().count();
    assert!(rows <= screen + 3, "at most 3 lines of history: {history}");
    // Bad values are refused and change nothing.
    for args in [
        &["set", "prefix", "Nope"][..],
        &["set", "buffers", "0"],
        &["set", "clipboard", "maybe"],
        &["set", "nonsense", "1"],
        &["bind", "x"],
        &["unbind", "Nope"],
    ] {
        let out = server.fux(args)?;
        assert_eq!(out.status, 1, "{args:?}: {}", out.stderr);
    }
    // A binding's command is checked when the binding is made.
    let out = server.fux(&["bind", "g", "no-such-command", "--flag"])?;
    assert_eq!(
        (out.status, out.stderr.as_str()),
        (
            1,
            "bind g: unknown command \"no-such-command\"; `fux help` lists commands\n"
        )
    );
    assert!(!server.ok(&["list-keys"])?.contains("no-such-command"));
    assert!(server.ok(&["list-keys"])?.contains("after the prefix, C-b"));
    server.ok(&["unbind-all"])?;
    client.keys("\x02")?;
    client.wait("an empty column", |t| t.contains("no bindings"))?;
    Ok(())
}

#[test]
fn a_key_that_cannot_be_bound_is_a_config_error_until_a_reload_fixes_it() -> Outcome {
    let server = Server::start("bind Escape resize-pane -L")?;
    let mut client = server.attach(10, 120)?;
    client.wait("the config error", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("config:") && b.contains("cannot be bound"))
    })?;
    std::fs::write(
        server.dir.join("fux.conf"),
        "set shell /bin/sh\nbind -r r h resize-pane -L\n",
    )
    .map_err(e)?;
    server.ok(&["reload"])?;
    client.detach()?;
    let mut again = server.attach(10, 120)?;
    again.wait_for("$")?;
    assert!(!again.bar().contains("config:"), "{}", again.bar());
    Ok(())
}

#[test]
fn an_invalid_config_is_shown_to_attaching_clients_until_a_reload_fixes_it() -> Outcome {
    let server = Server::start("bind")?;
    let mut client = server.attach(10, 100)?;
    client.wait("the config error", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("config:") && b.contains("fux.conf:2"))
    })?;
    // Layout commands are refused in a config file, naming the line.
    std::fs::write(server.dir.join("fux.conf"), "set shell /bin/sh\nsplit -h\n").map_err(e)?;
    let out = server.fux(&["reload"])?;
    assert!(
        out.stderr
            .contains("fux.conf:2: split changes panes or layout"),
        "{}",
        out.stderr
    );
    std::fs::write(
        server.dir.join("fux.conf"),
        "set shell /bin/sh\nset prefix C-a\n",
    )
    .map_err(e)?;
    server.ok(&["reload"])?;
    client.detach()?;
    let mut again = server.attach(10, 100)?;
    again.wait_for("$")?;
    assert!(!again.bar().contains("config:"), "{}", again.bar());
    again.keys("\x01")?;
    again.wait("the column under the new prefix", |t| {
        t.contains("Commands")
    })?;
    Ok(())
}
