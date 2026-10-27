//! Milestone 1: a server that answers `ls` and stops on `kill-server`, the
//! raw protocol's request and reply, and the socket's single owner.
mod support;
use support::*;

#[test]
fn a_server_answers_ls_and_stops_on_kill_server() -> Outcome {
    let mut server = Server::start("")?;
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("+1 main"), "{ls}");
    assert!(ls.contains("@1 main"), "{ls}");
    assert!(ls.contains("%1 sh"), "{ls}");
    let json = server.ok(&["ls", "json"])?;
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
fn a_second_server_on_the_same_socket_is_refused() -> Outcome {
    let server = Server::start("")?;
    let out = std::process::Command::new(FUX)
        .arg(fux::SERVE)
        .arg(&server.socket)
        .output()
        .map_err(e)?;
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("another fux server"), "{stderr}");
    assert_eq!(
        server.ok(&["ls"])?.lines().count(),
        4,
        "the first server is untouched"
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
            .ok(&["str", "%1", "pane", "capture"])?
            .lines()
            .any(|l| l == expected))
    })
}

/// A client that sends its command and then shuts its side of the socket,
/// as a script piping one in does, still gets the output and the status.
#[test]
fn a_client_that_shuts_its_side_still_gets_its_answer() -> Outcome {
    use fux::command::Command;
    use fux::protocol::{
        Asked, Context, Decoder, Frame, Greeting, Hello, PROTOCOL, Reply, Role, ToCommand,
    };
    use std::io::{Read, Write};
    let server = Server::start("")?;
    let mut stream = std::os::unix::net::UnixStream::connect(&server.socket).map_err(e)?;
    // Set before writing: macOS refuses a timeout on a socket its peer
    // has closed, as the server may have by the time the writes are done.
    stream.set_read_timeout(Some(PATIENCE)).map_err(e)?;
    let hello = Hello {
        protocol: PROTOCOL,
        version: "0".into(),
        role: Role::Command,
    };
    let request = Asked {
        request: Command::Ls {
            target: None,
            get: None,
        }
        .into(),
        context: Context {
            of: None,
            cwd: std::path::PathBuf::new(),
            pane: None,
            server: None,
            env: None,
            client: None,
        },
    };
    stream.write_all(&hello.encode().map_err(e)?).map_err(e)?;
    stream.write_all(&request.encode().map_err(e)?).map_err(e)?;
    stream.shutdown(std::net::Shutdown::Write).map_err(e)?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).map_err(e)?;
    let mut decoder = Decoder::default();
    decoder.push(&answer);
    // The reply comes in pieces, whole at their end.
    let (mut parts, mut reply) = (Vec::new(), None);
    let hello = decoder.frame::<Greeting>().map_err(e)?;
    assert!(matches!(hello, Some(Greeting::Hello(_))), "{hello:?}");
    while let Some(frame) = decoder.frame::<ToCommand>().map_err(e)? {
        match frame {
            ToCommand::Part(bytes) => parts.extend_from_slice(&bytes),
            ToCommand::PartsEnd => reply = Some(fux::wire::Reader::new(&parts).whole::<Reply>()),
            ToCommand::Exit(_) | ToCommand::Stdout(_) => {}
        }
    }
    let reply = reply.ok_or("no reply")?.map_err(e)?;
    assert_eq!(reply.end, fux::protocol::End::Done);
    let fux::protocol::Body::Listing { listing, .. } = &reply.body else {
        return Err(format!("not a listing: {:?}", reply.body));
    };
    let listed = listing.render();
    assert!(listed.contains(r#""name":"main""#), "{listed}");
    Ok(())
}
