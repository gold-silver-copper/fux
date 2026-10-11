//! Milestone 1: a server that answers `ls` and stops on `kill-server`, the
//! CLI's exit statuses, and the socket's single owner.
mod support;
use support::*;

#[test]
fn a_server_answers_ls_and_stops_on_kill_server() -> Outcome {
    let mut server = Server::start("")?;
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("+1 main"), "{ls}");
    assert!(ls.contains("@1 main"), "{ls}");
    assert!(ls.contains("%1 sh"), "{ls}");
    let json = server.ok(&["ls", "--json"])?;
    assert!(
        json.starts_with("{\"workspaces\":[{\"id\":\"+1\""),
        "{json}"
    );
    assert!(json.contains("\"clients\":[]"), "{json}");
    let killed = server.fux(&["kill-server"])?;
    assert_eq!(killed.status, 0, "{}", killed.stderr);
    // The socket goes as the server starts stopping, before its panes'
    // programs have ended: a command right after finds no server, and
    // says so.
    assert!(!server.socket.exists(), "the socket is removed at once");
    let out = server.fux(&["ls"])?;
    assert_eq!(out.status, 1);
    assert!(
        out.stderr.contains("no fux server is running"),
        "{}",
        out.stderr
    );
    assert!(server.wait_exit()?.success());
    assert!(
        server.log().contains("stopped by fux kill-server"),
        "{}",
        server.log()
    );
    Ok(())
}

#[test]
fn usage_errors_exit_2_and_failures_exit_1() -> Outcome {
    let server = Server::start("")?;
    let usage = server.fux(&["frobnicate"])?;
    assert_eq!(usage.status, 2, "{}", usage.stderr);
    assert!(usage.stderr.contains("unknown command"), "{}", usage.stderr);
    let usage = server.fux(&["split"])?;
    assert_eq!(usage.status, 2);
    assert!(
        usage.stderr.contains("-h (side by side) or -v (stacked)"),
        "{}",
        usage.stderr
    );
    let failed = server.fux(&["kill-pane", "-t", "%99"])?;
    assert_eq!(failed.status, 1);
    assert_eq!(
        failed.stderr.trim(),
        "fux: no pane %99".trim_start_matches("fux: ")
    );
    // A command that needs a target fails without -t or FUX_PANE, naming -t.
    let missing = server.fux(&["kill-pane"])?;
    assert_eq!(missing.status, 1);
    assert!(missing.stderr.contains("-t %N"), "{}", missing.stderr);
    // A client-screen command from the CLI needs -c.
    let client = server.fux(&["zoom"])?;
    assert_eq!(client.status, 1);
    assert!(client.stderr.contains("-c CLIENT"), "{}", client.stderr);
    Ok(())
}

#[test]
fn a_second_server_on_the_same_socket_is_refused() -> Outcome {
    let server = Server::start("")?;
    let out = std::process::Command::new(FUX)
        .args(["server", "--socket"])
        .arg(&server.socket)
        .output()
        .map_err(e)?;
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("another fux server"), "{stderr}");
    assert_eq!(
        server.ok(&["ls"])?.lines().count(),
        3,
        "the first server is untouched"
    );
    Ok(())
}

#[test]
fn a_bad_config_starts_on_the_defaults_and_a_reload_is_all_or_nothing() -> Outcome {
    let server = Server::start("set prefix C-a\nset prefix Nope")?;
    assert!(server.log().contains(":3: unknown key"), "{}", server.log());
    let keys = server.ok(&["list-keys"])?;
    assert!(
        keys.contains("after the prefix, C-b"),
        "defaults apply: {keys}"
    );
    std::fs::write(
        server.dir.join("fux.conf"),
        "set shell /bin/sh\nset prefix C-a\n",
    )
    .map_err(e)?;
    assert!(server.ok(&["reload"])?.contains("reloaded"));
    assert!(server.ok(&["list-keys"])?.contains("after the prefix, C-a"));
    std::fs::write(server.dir.join("fux.conf"), "set prefix C-x\nbind\n").map_err(e)?;
    let failed = server.fux(&["reload"])?;
    assert_eq!(failed.status, 1);
    assert!(failed.stderr.contains("fux.conf:2:"), "{}", failed.stderr);
    assert!(
        server.ok(&["list-keys"])?.contains("after the prefix, C-a"),
        "the old config is kept whole"
    );
    Ok(())
}

/// A pane's program is told it runs in fux, whatever terminal fux runs in:
/// `TERM_PROGRAM` is `fux`, and `TERM_PROGRAM_VERSION` fux's version.
#[test]
fn a_pane_is_told_it_runs_in_fux() -> Outcome {
    let server = Server::start("")?;
    server.type_line(
        "%1",
        "echo \"is-$TERM_PROGRAM-$TERM_PROGRAM_VERSION-$TERM\"",
    )?;
    let expected = format!("is-fux-{}-xterm-256color", env!("CARGO_PKG_VERSION"));
    eventually("the pane's environment", || {
        Ok(server
            .ok(&["capture-pane", "-t", "%1"])?
            .lines()
            .any(|l| l == expected))
    })
}

/// A client that sends its command and then shuts its side of the socket,
/// as a script piping one in does, still gets the output and the status.
#[test]
fn a_client_that_shuts_its_side_still_gets_its_answer() -> Outcome {
    use fux::protocol::{Command, Decoder, Frame, Hello, PROTOCOL, Role, ServerFrame};
    use std::io::{Read, Write};
    let server = Server::start("")?;
    let mut stream = std::os::unix::net::UnixStream::connect(&server.socket).map_err(e)?;
    let hello = Hello {
        protocol: PROTOCOL,
        version: "0",
        role: Role::Command,
    };
    let command = Command {
        argv: vec!["ls".into()],
        cwd: String::new(),
        pane: None,
    };
    stream.write_all(&hello.encode().map_err(e)?).map_err(e)?;
    stream.write_all(&command.encode().map_err(e)?).map_err(e)?;
    stream.shutdown(std::net::Shutdown::Write).map_err(e)?;
    stream.set_read_timeout(Some(PATIENCE)).map_err(e)?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).map_err(e)?;
    let mut decoder = Decoder::default();
    decoder.push(&answer);
    let (mut stdout, mut status) = (String::new(), None);
    while let Some(frame) = decoder.frame::<ServerFrame>().map_err(e)? {
        match frame {
            ServerFrame::Stdout(bytes) => stdout.push_str(&String::from_utf8_lossy(bytes)),
            ServerFrame::Done { status: s } => status = Some(s),
            ServerFrame::Hello(_)
            | ServerFrame::Paint(_)
            | ServerFrame::Exit(_)
            | ServerFrame::Stderr(_)
            | ServerFrame::Terminal { .. } => {}
        }
    }
    assert_eq!(status, Some(0), "{stdout}");
    assert!(stdout.contains("+1 main"), "{stdout}");
    Ok(())
}
