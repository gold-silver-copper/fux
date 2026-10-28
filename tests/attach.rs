//! Milestone 2: one pane. Attach, detach and reattach, resize, and
//! `capture`.
mod support;
use std::os::fd::AsFd;
use support::*;

#[test]
fn a_client_sees_the_shell_and_types_into_it() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 40)?;
    client.wait_for("$")?;
    client.keys("echo one-two\r")?;
    client.wait("the echo's output", |t| t.lines().any(|l| l == "one-two"))?;
    let bar = client.bar();
    assert!(bar.contains("main"), "{bar}");
    assert!(bar.contains("%1") && !bar.contains("%1 %1"), "{bar}");
    // The pane is the client's size, less the bar.
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("%1 40x9"), "{ls}");
    assert!(ls.contains("client c1 40x10 +1 @1 %1"), "{ls}");
    Ok(())
}

/// A client that only watches sees the screen, but its keys reach no
/// pane and open nothing: a key says it only watches, and the prefix and
/// the detach key detach it.
#[test]
fn a_read_only_client_watches_and_can_only_detach() -> Outcome {
    let server = Server::start("")?;
    let mut watcher = server.watch(10, 40)?;
    watcher.wait_for("$")?;
    server.type_line("%1", "echo shown")?;
    watcher.wait("output typed elsewhere", |t| {
        t.lines().any(|l| l == "shown")
    })?;
    watcher.keys("echo typed\r")?;
    watcher.wait("the notice", |t| t.contains("watching only"))?;
    watcher.keys("\x02c")?;
    let screen = server.ok(&["str", "%1", "pane", "capture"])?;
    assert!(
        !screen.contains("typed"),
        "its keys reached the pane: {screen}"
    );
    assert!(!server.ok(&["ls"])?.contains("%2"), "no window opened");
    watcher.keys("\x02d")?;
    assert_eq!(watcher.wait_exit()?, "detached");
    Ok(())
}

#[test]
fn detach_and_reattach_keep_the_shell_and_its_screen() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 40)?;
    client.wait_for("$")?;
    client.keys("echo before-detach\r")?;
    client.wait("output", |t| t.lines().any(|l| l == "before-detach"))?;
    client.keys("\x02d")?;
    assert_eq!(client.wait_exit()?, "detached");
    let ls = server.ok(&["ls"])?;
    assert!(!ls.contains("client"), "the client is gone: {ls}");
    let mut again = server.attach(10, 40)?;
    again.wait("the old output", |t| {
        t.lines().any(|l| l == "before-detach")
    })?;
    again.keys("echo after\r")?;
    again.wait("new output", |t| t.lines().any(|l| l == "after"))?;
    // detach from the command line needs the client.
    let out = server.fux(&["str", "c2", "client", "detach"])?;
    assert_eq!(out.status, 0, "{}", out.stderr);
    assert_eq!(again.wait_exit()?, "detached");
    Ok(())
}

/// However a connection ends, its client goes with it: a client that breaks
/// the protocol (a second Attach, a Hello) is dropped and detached, and so
/// is one that detaches and attaches again in one batch.
#[test]
fn a_connection_that_ends_takes_its_client_with_it() -> Outcome {
    use fux::protocol::{Attach, Frame, Hello, PROTOCOL, Role};
    let server = Server::start("")?;
    let clients = || -> Result<usize, String> {
        Ok(server
            .ok(&["ls"])?
            .lines()
            .filter(|l| l.starts_with("client "))
            .count())
    };
    let mut keep = server.attach(10, 40)?;
    keep.wait_for("$")?;
    let attach = Attach {
        of: None,
        size: sized(5, 20)?,
        workspace: None,
        read_only: false,
        initial: None,
    };
    let hello = Hello {
        protocol: PROTOCOL,
        version: "test".into(),
        role: Role::Attach,
    };
    for broken in [attach.encode().map_err(e)?, hello.encode().map_err(e)?] {
        let mut client = server.attach(5, 20)?;
        client.wait_for("$")?;
        eventually("two clients", || Ok(clients()? == 2))?;
        client.write(&broken)?;
        eventually("the broken client to go", || Ok(clients()? == 1))?;
    }
    // Detach and Attach together, in one write: the Attach comes after the
    // end. (Two writes race the server closing the connection.)
    let mut client = server.attach(5, 20)?;
    client.wait_for("$")?;
    let detach = fux::protocol::AttachedFrame::Detach.encode().map_err(e)?;
    client.write(&[detach, attach.encode().map_err(e)?].concat())?;
    assert_eq!(client.wait_exit()?, "detached");
    eventually("only the first client", || Ok(clients()? == 1))?;
    // The one left still sizes its pane alone.
    keep.keys("stty size\r")?;
    keep.wait("its size", |t| t.lines().any(|l| l == "9 40"))?;
    Ok(())
}

/// A screen that did not change is not painted again: a command that
/// changes nothing a client shows sends it nothing, not even an empty paint.
#[test]
fn an_unchanged_screen_is_not_painted_again() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 40)?;
    client.wait_for("$")?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    client.pump()?;
    let before = client.painted.len();
    // Selecting the pane already selected changes nothing on screen.
    server.ok(&["str", "c1", "client", "str", "%1", "pane", "focus-pane"])?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    client.pump()?;
    assert_eq!(
        client.painted.len(),
        before,
        "painted {:?}",
        String::from_utf8_lossy(client.painted.get(before..).unwrap_or_default())
    );
    Ok(())
}

#[test]
fn a_resize_reaches_the_program() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 40)?;
    client.wait_for("$")?;
    client.resize(20, 60)?;
    client.keys("stty size\r")?;
    client.wait("the new size", |t| t.lines().any(|l| l == "19 60"))?;
    assert!(server.ok(&["ls"])?.contains("%1 60x19"));
    // Past 4096 a way, refused and said: the view keeps its size.
    client.resize(20, 5000)?;
    client.wait("the refusal", |t| {
        t.contains("5000 columns by 20 rows: a client is")
    })?;
    assert!(server.ok(&["ls"])?.contains("client c1 60x20"));
    Ok(())
}

#[test]
fn capture_pane_shows_the_screen_and_history() -> Outcome {
    let server = Server::start("str history-bytes 100000 set")?;
    let mut client = server.attach(6, 30)?;
    client.wait_for("$")?;
    client.keys("for i in 1 2 3 4 5 6 7 8; do echo line$i; done\r")?;
    client.wait("the loop", |t| t.lines().any(|l| l == "line8"))?;
    let screen = server.ok(&["str", "%1", "pane", "capture"])?;
    assert!(screen.contains("line8"), "{screen}");
    assert!(
        !screen.contains("line1\n"),
        "line1 has scrolled into history: {screen}"
    );
    let history = server.ok(&["str", "%1", "pane", "20", "history", "capture"])?;
    assert!(history.contains("line1\n"), "{history}");
    let json = server.ok(&["str", "%1", "pane", "capture", "json"])?;
    assert!(
        json.starts_with(
            "{\"pane\":\"%1\",\"screen_rows\":5,\"screen_cols\":30,\"screen_cursor\":["
        ),
        "{json}"
    );
    assert!(json.contains("\"line8\""), "{json}");
    // From inside the pane, FUX_PANE (of the server FUX_SERVER names) is
    // `here`.
    let version = server.version("%1")?;
    let run = version.rsplit(':').next().unwrap_or_default();
    let inside = server.fux_env(
        &["here", "capture"],
        &[("FUX_PANE", "%1:0"), ("FUX_SERVER", run)],
    )?;
    assert_eq!(inside.status, 0);
    assert!(inside.stdout.contains("line8"));
    Ok(())
}

/// With `str titles true set`, the real client's terminal is given its pane's
/// title, its own saved first and restored on leaving; a bell rings it.
#[test]
fn the_real_client_gets_titles_and_bells_and_its_title_back() -> Outcome {
    let server = Server::start("str titles true set")?;
    let mut terminal = Terminal::attach(&server, 12, 50, &[])?;
    terminal.wait_for("%1")?;
    terminal.type_bytes(b"printf '\\033]2;my-title\\007\\007'; echo rang''-it\r")?;
    terminal.wait_for("rang-it")?;
    eventually("the title and the bell", || {
        terminal.pump();
        let output = String::from_utf8_lossy(&terminal.output).into_owned();
        Ok(output.contains("\x1b]2;my-title\x1b\\") && output.contains('\x07'))
    })?;
    terminal.type_bytes(b"\x02d")?;
    let status = terminal.wait_exit()?;
    assert!(status.success(), "{status}");
    let output = String::from_utf8_lossy(&terminal.output).into_owned();
    let push = String::from_utf8_lossy(fux::outer::TITLE_PUSH).into_owned();
    let pop = String::from_utf8_lossy(fux::outer::TITLE_POP).into_owned();
    let (pushed, popped) = (output.find(&push), output.rfind(&pop));
    assert!(
        pushed.is_some() && popped > pushed && output.matches(&push).count() == 1,
        "{output:?}"
    );
    Ok(())
}

#[test]
fn the_real_client_attaches_restores_its_terminal_and_reattaches() -> Outcome {
    let server = Server::start("")?;
    let mut terminal = Terminal::attach(&server, 12, 50, &[])?;
    terminal.wait_for("%1")?;
    terminal.type_bytes(b"echo real-client\r")?;
    terminal.wait_for("real-client")?;
    terminal.resize(15, 70)?;
    terminal.type_bytes(b"stty size\r")?;
    terminal.wait_for("14 70")?;
    // A program asks for the mouse, and the terminal is detached with it on.
    terminal.type_bytes(b"printf '\\033[?1002h\\033[?1006h'; echo mouse-ask''ed\r")?;
    terminal.wait_for("mouse-asked")?;
    terminal.type_bytes(b"\x02d")?;
    let status = terminal.wait_exit()?;
    assert!(status.success(), "{status}");
    let output = String::from_utf8_lossy(&terminal.output).into_owned();
    // Leaving restores the modes it set: bracketed paste, focus events,
    // autowrap, the cursor and the main screen.
    let leave = output
        .rfind("\x1b[?1049l")
        .ok_or("never left the alternate screen")?;
    assert!(
        output
            .get(..leave)
            .is_some_and(|o| o.contains("\x1b[?2004l") && o.contains("\x1b[?1004l"))
    );
    // And mouse reporting, which the server turned on for the program: off
    // again after it was last turned on.
    let on = output
        .rfind("\x1b[?1002h\x1b[?1006h")
        .ok_or("mouse reporting was never turned on")?;
    assert!(
        output
            .get(on..leave)
            .is_some_and(|o| o.contains(fux::render::MOUSE_OFF)),
        "{output:?}"
    );
    assert!(output.contains("[detached]"), "{output:?}");
    // And the shell is still there for the next attach.
    let mut again = Terminal::attach(&server, 12, 50, &[])?;
    again.wait_for("real-client")?;
    Ok(())
}

#[test]
fn attaching_inside_a_pane_is_refused_unless_unset() -> Outcome {
    let server = Server::start("")?;
    for words in [
        &[][..],
        &["opts", "attach"],
        &["opts", "str", "main", "in", "attach"],
    ] {
        let out = server.fux_env(words, &[("FUX_PANE", "%1:0")])?;
        assert_eq!(out.status, 1);
        assert!(out.stderr.contains("unset FUX_PANE"), "{}", out.stderr);
    }
    // With FUX_PANE emptied it would attach, if it had a terminal.
    let out = server.fux_env(&["opts", "attach"], &[("FUX_PANE", "")])?;
    assert_eq!(out.status, 1);
    assert!(out.stderr.contains("needs a terminal"), "{}", out.stderr);
    Ok(())
}

/// stdin and stdout must be one terminal: with stdout another, attach is
/// refused, saying so.
#[test]
fn attach_needs_stdin_and_stdout_on_one_terminal() -> Outcome {
    let server = Server::start("")?;
    let (_keys, keys) = fuxix::pty::open(pty_size(10, 40)?).map_err(e)?;
    let (_shown, shown) = fuxix::pty::open(pty_size(10, 40)?).map_err(e)?;
    let stdio = |fd: &fuxix::pty::Slave| {
        fd.as_fd()
            .try_clone_to_owned()
            .map(std::process::Stdio::from)
            .map_err(e)
    };
    let out = std::process::Command::new(FUX)
        .args(["opts", "attach"])
        .env("FUX_SOCKET", &server.socket)
        .env_remove("FUX_PANE")
        .stdin(stdio(&keys)?)
        .stdout(stdio(&shown)?)
        .output()
        .map_err(e)?;
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("the same one"), "{stderr}");
    Ok(())
}

#[test]
fn attach_starts_a_server_when_none_answers() -> Outcome {
    let dir = fuxix::file::scratch()
        .map_err(e)?
        .join(format!("fux-auto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(e)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    let socket = dir.join("s").join("fux.sock");
    // A server handle that only cleans up; the client starts the real one.
    let (master, slave) = fuxix::pty::open(pty_size(10, 40)?).map_err(e)?;
    let stdio = |fd: &fuxix::pty::Slave| {
        fd.as_fd()
            .try_clone_to_owned()
            .map(std::process::Stdio::from)
            .map_err(e)
    };
    let mut child = std::process::Command::new(FUX)
        .env("FUX_SOCKET", &socket)
        .env("SHELL", "/bin/sh")
        .env("XDG_CONFIG_HOME", &dir)
        .env_remove("FUX_PANE")
        .stdin(stdio(&slave)?)
        .stdout(stdio(&slave)?)
        .stderr(stdio(&slave)?)
        .spawn()
        .map_err(e)?;
    drop(slave);
    // Read, as a terminal is: one never read fills (macOS's pty holds about
    // a kilobyte), and the client's last writes, with the server's, block.
    let mut terminal = std::fs::File::from(master.as_fd().try_clone_to_owned().map_err(e)?);
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        // The master is nonblocking: nothing to read yet is no end.
        loop {
            match std::io::Read::read(&mut terminal, &mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    let result = eventually("the auto-started server", || {
        let out = std::process::Command::new(FUX)
            .arg("ls")
            .env("FUX_SOCKET", &socket)
            .output()
            .map_err(e)?;
        Ok(String::from_utf8_lossy(&out.stdout).contains("client c1"))
    });
    let log = std::fs::read_to_string(dir.join("s").join("fux.log"));
    let _ = std::process::Command::new(FUX)
        .arg("force-kill-server")
        .env("FUX_SOCKET", &socket)
        .output();
    let _ = child.wait();
    drop(master);
    let _ = std::fs::remove_dir_all(&dir);
    result?;
    assert!(log.is_ok(), "the server logs beside its socket");
    Ok(())
}

#[test]
fn a_panes_program_inherits_only_stdio_and_a_clean_signal_mask() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    // Descriptors 3..9, whatever the server holds there, are not open here,
    // for reading or for writing; shells keep their own from 10 up. Any
    // that is open gets described in a file, so a failure says what leaked.
    let report = server.dir.join("open-fds");
    client.keys(&format!(
        "for n in 3 4 5 6 7 8 9; do {{ (: >&$n) || (: <&$n); }} 2>/dev/null && echo fd$n-open && \
         {{ lsof -a -p $$ -d $n || ls -l /proc/$$/fd/$n; }} >> '{}' 2>&1; done; echo fds-checked\r",
        report.display()
    ))?;
    client.wait("the check", |t| t.lines().any(|l| l == "fds-checked"))?;
    let open: Vec<String> = client
        .lines()
        .into_iter()
        .filter(|l| l.starts_with("fd") && l.ends_with("-open"))
        .collect();
    assert!(
        open.is_empty(),
        "{open:?}:\n{}",
        std::fs::read_to_string(&report).unwrap_or_default()
    );
    // The mask of a program the shell starts, not of the shell itself: bash
    // blocks SIGCHLD while it waits on a job, as it waits on `ps`.
    client.keys("sh -c 'exec ps -o sigmask= -p $$' | tr -d ' 0'; echo mask-checked\r")?;
    client.wait("the mask", |t| t.lines().any(|l| l == "mask-checked"))?;
    let lines = client.lines();
    let at = lines.iter().position(|l| l == "mask-checked").unwrap_or(0);
    assert_eq!(
        lines.get(at.wrapping_sub(1)).map(String::as_str),
        Some(""),
        "the mask is empty: {lines:?}"
    );
    Ok(())
}

#[test]
fn exiting_the_last_shell_stops_the_server_and_tells_the_client() -> Outcome {
    let mut server = Server::start("")?;
    let mut client = server.attach(10, 40)?;
    client.wait_for("$")?;
    client.keys("exit 3\r")?;
    let reason = client.wait_exit()?;
    assert!(
        reason.contains("the last pane closed"),
        "{reason}; the server log:\n{}",
        server.log()
    );
    assert!(server.wait_exit()?.success());
    assert!(!server.socket.exists());
    Ok(())
}

#[test]
fn focus_events_reach_a_pane_that_asked_and_cursor_shapes_pass_through() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    // A pane that did not ask gets nothing. The marker shows once the shell
    // has given the terminal back, so what follows reaches cat, not the
    // shell's line editor.
    client.keys("echo cat-runs; cat -v\r")?;
    client.wait("cat started", |t| t.lines().any(|l| l == "cat-runs"))?;
    client.send(b"\x1b[I")?;
    client.keys("before\r")?;
    client.wait("cat's echo", |t| t.lines().any(|l| l == "before"))?;
    assert!(!client.text().contains("^[[I"), "{}", client.text());
    client.keys("\x03")?;
    // Typing before the prompt is back would race the interrupt's flush.
    client.wait("the prompt again", |t| {
        // The last line is the bar; the prompt is the last one above it.
        t.lines()
            .rev()
            .skip(1)
            .find(|l| !l.is_empty())
            .is_some_and(|l| l == "$")
    })?;
    // One that turns on ?1004 hears focus in and out.
    // The marker is printed after ?1004h, so once it shows the pane has
    // asked; reports sent before cat starts wait in the terminal for it.
    client.keys("printf '\\033[?1004h\\033[5 q'; echo asked-for-focus; cat -v\r")?;
    client.wait("focus reporting on", |t| {
        t.lines().any(|l| l == "asked-for-focus")
    })?;
    client.send(b"\x1b[I")?;
    client.send(b"\x1b[O")?;
    client.keys("\r")?;
    client.wait("the reports", |t| t.lines().any(|l| l == "^[[I^[[O"))?;
    // Its cursor shape reaches the client's terminal.
    let painted = String::from_utf8_lossy(&client.painted).into_owned();
    assert!(painted.contains("\x1b[5 q"), "DECSCUSR 5 was passed on");
    client.keys("\x03")?;
    Ok(())
}

#[test]
fn a_protocol_mismatch_refuses_attach_but_accepts_force_kill() -> Outcome {
    use fux::protocol::{Frame, Greeting, Hello};
    use std::io::{Read, Write};
    let server = Server::start("")?;
    let mut stream = std::os::unix::net::UnixStream::connect(&server.socket).map_err(e)?;
    // Set before writing: macOS refuses a timeout on a socket its peer
    // has closed, as the server may have by the time the writes are done.
    stream.set_read_timeout(Some(PATIENCE)).map_err(e)?;
    let hello = Hello {
        protocol: 999,
        version: "0.0.0".into(),
        role: fux::protocol::Role::Attach,
    };
    stream.write_all(&hello.encode().map_err(e)?).map_err(e)?;
    let mut decoder = fux::protocol::Decoder::default();
    let mut buffer = [0u8; 4096];
    let mut exit = None;
    loop {
        // A signal interrupts a read with a timeout rather than restarting
        // it; under emulation that happens nearly every run.
        let n = match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("reading the server's answer: {error}")),
        };
        decoder.push(buffer.get(..n).unwrap_or_default());
        while let Some(frame) = decoder.frame().map_err(e)? {
            if let Greeting::Exit(reason) = frame {
                exit = Some(reason.to_owned());
            }
        }
    }
    let reason = exit.ok_or("no Exit frame")?;
    assert!(
        reason.contains(&format!("protocol {}", fux::protocol::PROTOCOL))
            && reason.contains("fux force-kill-server"),
        "{reason}"
    );
    // The frozen Kill greeting works even with that same wrong version.
    let mut client = server.attach(10, 40)?;
    let mut stream = std::os::unix::net::UnixStream::connect(&server.socket).map_err(e)?;
    let kill = Hello {
        role: fux::protocol::Role::Kill,
        ..hello
    };
    stream.write_all(&kill.encode().map_err(e)?).map_err(e)?;
    assert!(client.wait_exit()?.contains("force-kill-server"));
    Ok(())
}

/// A socket in a private directory of its own, where a test plays the
/// server: the directory, removed when done, and the listener.
fn fake_server(
    name: &str,
) -> Result<(std::path::PathBuf, std::os::unix::net::UnixListener), String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = fuxix::file::scratch()
        .map_err(e)?
        .join(format!("fux-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(e)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    let listener = std::os::unix::net::UnixListener::bind(dir.join("fux.sock")).map_err(e)?;
    Ok((dir, listener))
}

/// The client of a server of another protocol is told how to restart it,
/// even when that server hangs up as soon as it has said so.
#[test]
fn a_server_of_another_protocol_is_named_by_the_client() -> Outcome {
    use fux::protocol::{Frame, Greeting, Hello, Role};
    let (dir, listener) = fake_server("mismatch")?;
    let server = std::thread::spawn(move || -> Outcome {
        let (mut stream, _) = listener.accept().map_err(e)?;
        let hello = Greeting::Hello(Hello {
            protocol: 999,
            version: "9.9.9".into(),
            role: Role::Command,
        });
        let exit = Greeting::Exit("whatever the server says");
        // In one write: the client may hang up once it has read the Hello,
        // which is all it needs, and a second write would then fail.
        let mut said = hello.encode().map_err(e)?;
        said.extend(exit.encode().map_err(e)?);
        std::io::Write::write_all(&mut stream, &said).map_err(e)?;
        Ok(())
    });
    let out = std::process::Command::new(FUX)
        .arg("ls")
        .env("FUX_SOCKET", dir.join("fux.sock"))
        .output()
        .map_err(e)?;
    server.join().map_err(|_| "the fake server panicked")??;
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("protocol 999 (fux 9.9.9)") && stderr.contains("fux force-kill-server"),
        "{stderr}"
    );
    Ok(())
}

/// A client whose server answers its Hello and then nothing more, while
/// its terminal is raw, is still detached by a signal, and puts its
/// terminal back.
#[test]
fn a_signal_detaches_a_client_waiting_on_a_stalled_server() -> Outcome {
    use fux::protocol::{Frame, Greeting, Hello, PROTOCOL, Role};
    let (dir, listener) = fake_server("stalled")?;
    let server = std::thread::spawn(move || -> Result<std::os::unix::net::UnixStream, String> {
        // `fux opts attach` first connects only to see that a server answers: the
        // client's connection is the one that says hello.
        let mut stream = loop {
            let (mut stream, _) = listener.accept().map_err(e)?;
            if std::io::Read::read(&mut stream, &mut [0u8; 64]).map_err(e)? > 0 {
                break stream;
            }
        };
        let hello = Greeting::Hello(Hello {
            protocol: PROTOCOL,
            version: "0".into(),
            role: Role::Attach,
        });
        std::io::Write::write_all(&mut stream, &hello.encode().map_err(e)?).map_err(e)?;
        Ok(stream)
    });
    let socket = dir.join("fux.sock");
    let mut terminal = Terminal::start(10, 40, &[FUX, "opts", "attach"], |command| {
        command
            .env("FUX_SOCKET", &socket)
            .env("XDG_CONFIG_HOME", &dir);
    })?;
    terminal.wait_for("\x1b[?1049h")?;
    // Held open, saying no more.
    let _stalled = server.join().map_err(|_| "the fake server panicked")??;
    let pid = i32::try_from(terminal.child.id()).map_err(e)?;
    signal(pid, fuxix::process::Signal::Term);
    let status = terminal.wait_exit()?;
    let _ = std::fs::remove_dir_all(&dir);
    let output = String::from_utf8_lossy(&terminal.output);
    assert!(status.success(), "{status}: {output:?}");
    assert!(
        output.contains("\x1b[?1049l"),
        "the terminal is put back: {output:?}"
    );
    assert!(output.contains("[detached by a signal]"), "{output:?}");
    Ok(())
}

/// A client detached by a signal writes what its server paints until it
/// lets the client go before it puts its terminal back, so that none of it
/// lands on the shell after (a mouse mode turned on again, say).
#[test]
fn a_signal_detached_client_writes_the_last_paint_before_leaving() -> Outcome {
    use fux::protocol::{Frame, Greeting, Hello, PROTOCOL, Role, ToAttached};
    use std::io::{Read, Write};
    let (dir, listener) = fake_server("last-paint")?;
    let server = std::thread::spawn(move || -> Outcome {
        // The connection that says hello, as above.
        let (mut stream, mut got) = loop {
            let (mut stream, _) = listener.accept().map_err(e)?;
            let mut buffer = [0u8; 4096];
            let n = stream.read(&mut buffer).map_err(e)?;
            if n > 0 {
                break (stream, buffer.get(..n).unwrap_or_default().to_vec());
            }
        };
        let hello = Greeting::Hello(Hello {
            protocol: PROTOCOL,
            version: "0".into(),
            role: Role::Attach,
        });
        let welcome = ToAttached::Welcome(
            "c1".parse().map_err(e)?,
            "1-2".parse().map_err(|()| "test run")?,
        );
        let ready = ToAttached::Paint(b"READY"[..].into());
        for frame in [hello.encode(), welcome.encode(), ready.encode()] {
            stream.write_all(&frame.map_err(e)?).map_err(e)?;
        }
        // Its Detach, the last frame it sends.
        let detach = [0, 0, 0, 1, 5];
        let mut buffer = [0u8; 4096];
        while !got.ends_with(&detach) {
            let n = stream.read(&mut buffer).map_err(e)?;
            if n == 0 {
                return Err("the client went without detaching".into());
            }
            got.extend_from_slice(buffer.get(..n).unwrap_or_default());
        }
        let last = ToAttached::Paint(b"LAST"[..].into());
        for frame in [last.encode(), ToAttached::Exit("detached").encode()] {
            stream.write_all(&frame.map_err(e)?).map_err(e)?;
        }
        Ok(())
    });
    let socket = dir.join("fux.sock");
    let mut terminal = Terminal::start(10, 40, &[FUX, "opts", "attach"], |command| {
        command
            .env("FUX_SOCKET", &socket)
            .env("XDG_CONFIG_HOME", &dir);
    })?;
    terminal.wait_for("READY")?;
    signal(
        i32::try_from(terminal.child.id()).map_err(e)?,
        fuxix::process::Signal::Term,
    );
    let status = terminal.wait_exit()?;
    server.join().map_err(|_| "the fake server panicked")??;
    let _ = std::fs::remove_dir_all(&dir);
    let output = String::from_utf8_lossy(&terminal.output);
    assert!(status.success(), "{status}: {output:?}");
    let (last, leave) = (output.find("LAST"), output.rfind("\x1b[?1049l"));
    assert!(
        last.zip(leave).is_some_and(|(last, leave)| last < leave),
        "{output:?}"
    );
    assert!(output.contains("[detached by a signal]"), "{output:?}");
    Ok(())
}

/// A pane's program ignores no signal fux ignores: the Rust runtime ignores
/// SIGPIPE and fux catches others, and neither reaches the program.
/// Dispositions fux itself inherited ignored (a container's, say) pass on,
/// as they would to any child, so the baseline is what a plain child of
/// this test inherits. Linux reports dispositions in /proc.
#[cfg(target_os = "linux")]
#[test]
fn a_panes_program_ignores_no_signal_fux_ignores() -> Outcome {
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let baseline = std::process::Command::new("sh")
        .args(["-c", "grep SigIgn /proc/self/status"])
        .output()
        .map_err(e)?;
    let baseline = squash(&String::from_utf8_lossy(&baseline.stdout));
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    client.keys("sh -c 'grep SigIgn /proc/self/status'\r")?;
    client.wait("the dispositions", |t| {
        t.lines().any(|l| l.starts_with("SigIgn:"))
    })?;
    let pane = client
        .lines()
        .into_iter()
        .find(|l| l.starts_with("SigIgn:"))
        .unwrap_or_default();
    assert_eq!(squash(&pane), baseline);
    Ok(())
}

#[test]
fn a_panes_program_has_the_pty_as_its_controlling_terminal() -> Outcome {
    // A shell that is not interactive opens no terminal itself, so what `ps`
    // says here is what fux gave it: as controlling terminal the PTY on its
    // stdin, with the program's own group in the foreground. (Without one,
    // macOS names some other terminal, and Linux none.)
    let server = Server::start("")?;
    let pane = server.ok(&[
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
        "echo $(tty) $(ps -o tty=,tpgid=,pgid= -p $$); echo checked; exec sleep 60",
        "arg",
        "run",
    ])?;
    let pane = pane.split(':').next().unwrap_or_default();
    let mut fields = Vec::new();
    eventually("the program's report", || {
        let screen = server.ok(&["str", pane, "pane", "capture"])?;
        let mut lines = screen.lines().skip_while(|l| l.trim().is_empty());
        fields = lines
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        Ok(lines.next() == Some("checked"))
    })?;
    let [stdin, controlling, foreground, group] = fields.as_slice() else {
        return Err(format!("unexpected output: {fields:?}"));
    };
    // `ps` shortens /dev/ttys003 to ttys003, and /dev/pts/3 to pts/3.
    assert_eq!(stdin.strip_prefix("/dev/"), Some(controlling.as_str()));
    assert_eq!(foreground, group);
    Ok(())
}

#[test]
fn a_panes_shell_has_the_pty_as_its_terminal_and_job_control() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    // Only a process with a controlling terminal can open /dev/tty.
    client.keys("(: </dev/tty) 2>/dev/null && echo has-a-tty\r")?;
    client.wait("the controlling terminal", |t| {
        t.lines().any(|l| l == "has-a-tty")
    })?;
    // C-z stops the foreground job and fg resumes it: the PTY's foreground
    // group is the shell's to hand out.
    // `sleep 37`, a command line no other test runs, so `ps` finds this one.
    client.keys("sleep 37\r")?;
    eventually("sleep in the foreground", || {
        job_state("sleep 37", foreground)
    })?;
    client.keys("\x1a")?;
    client.wait("the stopped job", |t| t.contains("Stopped"))?;
    client.keys("fg\r")?;
    eventually("sleep in the foreground again", || {
        job_state("sleep 37", foreground)
    })?;
    client.keys("\x03")?;
    // The prompt is back once the interrupted job has gone: typed sooner,
    // the next line would be echoed before it. It is a prompt below the
    // `^C` echo: the client may still be showing the one after C-z.
    eventually("the prompt after the job", || {
        client.pump()?;
        let (row, col) = client.terminal.screen().cursor_position();
        let lines = client.lines();
        let row = usize::from(row);
        Ok(col == 2
            && lines.get(row).is_some_and(|l| l.trim_end() == "$")
            && lines.iter().take(row).any(|l| l.starts_with("^C")))
    })?;
    client.keys("jobs; echo after-fg\r")?;
    client.wait("the prompt after fg", |t| {
        t.lines().any(|l| l == "after-fg")
    })?;
    // `jobs` printed nothing: the job ran to its end in the foreground.
    let lines = client.lines();
    let at = lines.iter().position(|l| l == "after-fg").unwrap_or(0);
    let before = at.checked_sub(1).and_then(|i| lines.get(i));
    assert!(
        before.is_some_and(|l| l.ends_with("jobs; echo after-fg")),
        "{lines:?}"
    );
    Ok(())
}

/// A program that cannot start is reported with the system's error as it
/// is, and where it was started: a missing program, a missing `#!`
/// interpreter and a missing directory are all the same `NotFound`, which
/// fux does not guess between; a directory is not checked first.
#[test]
fn a_program_that_cannot_start_is_reported_as_the_system_says() -> Outcome {
    let server = Server::start("")?;
    let missing = server.dir.join("no-such-shell");
    let not_executable = server.dir.join("server.log");
    let script = server.dir.join("script");
    std::fs::write(&script, "#!/no/such/sh -e\necho hi\n").map_err(e)?;
    let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&script, executable).map_err(e)?;
    let (here, nowhere) = (&server.dir, &server.dir.join("no-such-dir"));
    let not_found = "No such file or directory (os error 2)";
    let before = server.ok(&["ls"])?;
    for (program, cwd, error) in [
        (&missing, here, not_found),
        (&not_executable, here, "Permission denied (os error 13)"),
        (&script, here, not_found),
        (&std::path::PathBuf::from("/bin/sh"), nowhere, not_found),
        (
            &std::path::PathBuf::from("/bin/sh"),
            &not_executable,
            "Not a directory (os error 20)",
        ),
    ] {
        let program = program.to_string_lossy();
        let cwd_text = cwd.to_string_lossy();
        let out = server.fux(&[
            "str",
            "shell",
            "list",
            "str",
            &program,
            "end",
            "set",
            "opts",
            "80",
            "24",
            "size",
            "str",
            &cwd_text,
            "cwd",
            "new-workspace",
        ])?;
        assert_eq!(out.status, 1);
        assert!(
            out.stderr
                .ends_with(&format!(": starting {program} in {cwd:?}: {error}\n")),
            "{}",
            out.stderr
        );
    }
    // Nothing a failed start made is left.
    assert_eq!(server.ok(&["ls"])?, before);
    // The server is unharmed, and a good shell starts again.
    server.ok(&["str", "shell", "list", "str", "/bin/sh", "end", "set"])?;
    server.ok(&["opts", "80", "24", "size", "new-workspace"])?;
    Ok(())
}

#[test]
fn a_server_a_client_started_outlives_the_clients_terminal() -> Outcome {
    let dir = fuxix::file::scratch()
        .map_err(e)?
        .join(format!("fux-hangup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(e)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    let socket = dir.join("s").join("fux.sock");
    let ls = || {
        std::process::Command::new(FUX)
            .arg("ls")
            .env("FUX_SOCKET", &socket)
            .output()
            .map_err(e)
    };
    // The client leads its terminal's session, as under a terminal emulator.
    let mut terminal = Terminal::start(10, 40, &[FUX], |command| {
        command
            .env("FUX_SOCKET", &socket)
            .env("SHELL", "/bin/sh")
            .env("XDG_CONFIG_HOME", &dir);
    })?;
    let started = eventually("the auto-started server", || {
        Ok(String::from_utf8_lossy(&ls()?.stdout).contains("client c1"))
    });
    // Closing the terminal hangs up its session, and the kernel or the
    // shell signals the terminal's foreground job: the client goes, and the
    // server, in a session of its own, stays.
    terminal.hang_up();
    if let Some(client) = fuxix::process::Pid::of(&terminal.child) {
        let _ = fuxix::process::kill_group(client, fuxix::process::Signal::Hup);
    }
    let gone = eventually("the client to go", || {
        Ok(terminal.child.try_wait().map_err(e)?.is_some())
    });
    // The server answers once it has seen the client go: by then it has
    // handled whatever reached it with the hang-up, which would have
    // stopped it.
    let after = eventually("the server to see the client go", || {
        let out = ls()?;
        Ok(out.status.success() && !String::from_utf8_lossy(&out.stdout).contains("client c1"))
    });
    let _ = std::process::Command::new(FUX)
        .arg("force-kill-server")
        .env("FUX_SOCKET", &socket)
        .output();
    drop(terminal);
    let _ = std::fs::remove_dir_all(&dir);
    started?;
    gone?;
    after
}

/// The first pane uses the invoking client's directory, regardless of HOME.
#[test]
fn a_first_shell_starts_in_the_invocation_directory() -> Outcome {
    let dir = fuxix::file::scratch()
        .map_err(e)?
        .join(format!("fux-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(e)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    for (i, home) in [
        None,
        Some(""),
        Some("relative"),
        Some("/nonexistent/fux-home"),
    ]
    .into_iter()
    .enumerate()
    {
        let socket = dir.join(format!("s{i}")).join("fux.sock");
        let marker = dir.join(format!("cwd{i}"));
        let mut terminal = Terminal::start(10, 40, &[FUX], |command| {
            command
                .current_dir(&dir)
                .env("FUX_SOCKET", &socket)
                .env("SHELL", "/bin/sh")
                .env("XDG_CONFIG_HOME", &dir);
            match home {
                Some(home) => command.env("HOME", home),
                None => command.env_remove("HOME"),
            };
        })?;
        terminal.wait_for("%1")?;
        terminal.type_bytes(format!("pwd > '{}'; exit\r", marker.display()).as_bytes())?;
        let status = terminal.wait_exit();
        let _ = std::process::Command::new(FUX)
            .arg("force-kill-server")
            .env("FUX_SOCKET", &socket)
            .output();
        assert!(status?.success());
        assert_eq!(
            std::fs::read_to_string(&marker).map_err(e)?.trim(),
            dir.to_str().ok_or("cwd text")?
        );
    }
    std::fs::remove_dir_all(&dir).map_err(e)?;
    Ok(())
}

/// A caller whose own directory is gone sends none it knows: a start it
/// asks for that needs one fails saying so, rather than starting in
/// `$HOME` or, joined onto nothing, the server's `/`. One given whole still
/// starts.
#[test]
fn a_caller_with_no_directory_starts_nothing_that_needs_one() -> Outcome {
    let server = Server::start("")?;
    let before = server.ok(&["ls"])?;
    for (i, given) in ["", "str . cwd", "str tmp cwd"].into_iter().enumerate() {
        let gone = server.dir.join(format!("gone-{i}"));
        std::fs::create_dir_all(&gone).map_err(e)?;
        let script =
            format!(r#"cd "$1" && rmdir "$1" && fux opts str main in 80 24 size {given} new-tab"#);
        let gone = gone.to_string_lossy();
        let out = server.piped(
            std::path::Path::new("/bin/sh"),
            &["-c", &script, "sh", &gone],
            "",
        )?;
        assert_eq!(out.status, 1, "{given}: {}", out.stdout);
        assert!(
            out.stderr
                .ends_with("the caller's own directory is unknown\n"),
            "{given}: {}",
            out.stderr
        );
    }
    assert_eq!(server.ok(&["ls"])?, before);
    let whole = server.dir.join("gone-whole");
    std::fs::create_dir_all(&whole).map_err(e)?;
    let script = r#"cd "$1" && rmdir "$1" && fux opts str main in 80 24 size str "$2" cwd new-tab"#;
    let (gone, here) = (whole.to_string_lossy(), server.dir.to_string_lossy());
    let out = server.piped(
        std::path::Path::new("/bin/sh"),
        &["-c", script, "sh", &gone, &here],
        "",
    )?;
    assert_eq!(out.status, 0, "{}", out.stderr);
    Ok(())
}

/// Resizing a terminal signals its client, and a signal can interrupt a
/// read or write in flight. Typing while the window is resized, hundreds of
/// times, keeps the client attached and loses no byte (Linux once ended the attachment with "Interrupted system call").
#[test]
fn resizing_while_typing_keeps_the_client_attached_and_every_byte() -> Outcome {
    let server = Server::start("")?;
    let mut terminal = Terminal::attach(&server, 20, 60, &[])?;
    terminal.wait_for("%1")?;
    let out = server.dir.join("typed");
    // The marker is assembled when it runs, so the typed line never
    // matches it.
    terminal.type_bytes(
        format!(
            "stty -echo; echo ready-$((40+2)); cat > '{}'\r",
            out.display()
        )
        .as_bytes(),
    )?;
    terminal.wait_for("ready-42")?;
    let rounds = 400;
    let mut expected = String::new();
    for i in 0..rounds {
        let line = format!("line-{i}");
        // A line in two writes, with a resize, and so a SIGWINCH, between
        // them and after them.
        let (head, tail) = line.split_at_checked(3).ok_or("a line")?;
        terminal.type_bytes(head.as_bytes())?;
        terminal.resize(
            if i % 2 == 0 { 21 } else { 20 },
            if i % 3 == 0 { 61 } else { 60 },
        )?;
        terminal.type_bytes(tail.as_bytes())?;
        terminal.type_bytes(b"\r")?;
        terminal.resize(20, 60)?;
        // A signal straight to the client too, not only through the PTY.
        let pid = fuxix::process::Pid::of(&terminal.child).ok_or("the client's pid")?;
        let _ = fuxix::process::kill(pid, fuxix::process::Signal::Winch);
        expected.push_str(&line);
        expected.push('\n');
        terminal.pump();
        if let Some(status) = terminal.child.try_wait().map_err(e)? {
            return Err(format!(
                "the client ended after {i} rounds ({status}):\n{}",
                String::from_utf8_lossy(&terminal.output)
            ));
        }
    }
    // The terminal read as a terminal is, while the lines arrive: the
    // client writes the paints to it, and waits on one that takes no more.
    eventually("every line typed", || {
        terminal.pump();
        Ok(std::fs::read_to_string(&out).unwrap_or_default() == expected)
    })
    .map_err(|error| {
        let got = std::fs::read_to_string(&out).unwrap_or_default();
        format!(
            "{error}: {} of {rounds} lines arrived; the first difference is at byte {}",
            got.lines().count(),
            got.bytes()
                .zip(expected.bytes())
                .position(|(a, b)| a != b)
                .unwrap_or(got.len().min(expected.len()))
        )
    })?;
    assert!(
        terminal.child.try_wait().map_err(e)?.is_none(),
        "the client ended"
    );
    Ok(())
}

/// The prefix, then Esc, stops a key's program waiting at the server; the
/// next key's program asks on as before.
#[test]
fn a_stopped_key_program_leaves_the_next_one_asking() -> Outcome {
    let keys = "str j str 'me focused 600 timeout wait drop' bind \
                str k str 'me focused str next-''ok text send-keys' bind";
    let server = Server::start(keys)?;
    let mut terminal = Terminal::attach(&server, 12, 50, &[])?;
    terminal.wait_for("%1")?;
    terminal.type_bytes(b"echo sleep''ing; sleep 100\r")?;
    terminal.wait_for("sleeping")?;
    terminal.type_bytes(b"\x02j")?;
    // The wait under way at the server, not stopped between its steps.
    std::thread::sleep(std::time::Duration::from_millis(300));
    terminal.type_bytes(b"\x02\x1b")?;
    terminal.wait_for("stopped")?;
    terminal.type_bytes(b"\x02k")?;
    terminal.wait_for("next-ok")
}

/// Commands from the command line are not ended by signals either, when
/// one arrives while they wait for the server.
#[test]
fn a_signal_does_not_end_a_command_waiting_for_the_server() -> Outcome {
    let server = Server::start("")?;
    for _ in 0..100 {
        let child = std::process::Command::new(FUX)
            .arg("ls")
            .env("FUX_SOCKET", &server.socket)
            .env_remove("FUX_PANE")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(e)?;
        let pid = fuxix::process::Pid::of(&child).ok_or("a pid")?;
        for _ in 0..20 {
            let _ = fuxix::process::kill(pid, fuxix::process::Signal::Winch);
        }
        let out = child.wait_with_output().map_err(e)?;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// A server may inherit descriptors without close-on-exec from whatever
/// starts it: a shell that leaks them, or, in these tests, another thread
/// opening one in the moment before it sets the flag. Its panes' programs
/// inherit none of them. Before a pane's start marked every descriptor above
/// stderr close-on-exec, the tests above saw a PTY master and a socket of
/// the test process in a pane, about once in 450 runs.
#[test]
fn a_descriptor_the_server_inherited_does_not_reach_its_panes() -> Outcome {
    let (server, held) = Server::start_inheriting("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    // Found by what it is, not its number: shells keep descriptors of their
    // own from 10 up. The marker is made when it runs.
    let file = server.dir.join("inherited");
    client.keys(&format!(
        "if {{ lsof -p $$ 2>/dev/null || ls -l /proc/$$/fd; }} | grep -q '{}'; then echo held-$((1+1)); else echo free-$((2+2)); fi\r",
        file.display()
    ))?;
    client.wait("the answer", |t| {
        t.lines().any(|l| l == "held-2" || l == "free-4")
    })?;
    assert!(
        client.lines().iter().any(|l| l == "free-4"),
        "descriptor {held}, inherited by the server, is open in its pane"
    );
    Ok(())
}

/// Whether a process running `command` has a `ps` state that `test` takes.
fn job_state(command: &str, test: fn(&str) -> bool) -> Result<bool, String> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "stat=,command="])
        .output()
        .map_err(e)?;
    Ok(String::from_utf8_lossy(&out.stdout).lines().any(|line| {
        line.trim_start()
            .split_once(char::is_whitespace)
            .is_some_and(|(stat, c)| c.trim() == command && test(stat))
    }))
}

/// A `ps` state of a process running in its terminal's foreground group:
/// running or asleep (`R`, `S`), with `+`, whatever other flags it has
/// (`N` on a runner that lowers its priority).
fn foreground(stat: &str) -> bool {
    stat.starts_with(['R', 'S']) && stat.contains('+')
}

#[test]
fn an_explicit_headless_attach_reports_settings_forwarding_failure() -> Outcome {
    let server = Server::start("")?;
    let payload = server.dir.join("oversize-argument");
    std::fs::write(&payload, vec![b'x'; fux::protocol::MAX_FRAME]).map_err(e)?;
    let mut terminal = Terminal::attach(
        &server,
        10,
        80,
        &[
            "str",
            "shell",
            "list",
            "str",
            "/bin/sh",
            "str",
            &payload.to_string_lossy(),
            "read-file",
            "end",
            "set",
            "opts",
            "80",
            "24",
            "size",
            "attach",
        ],
    )?;
    eventually("a refused bridge or headless admission", || {
        terminal.pump();
        if terminal.child.try_wait().map_err(e)?.is_some() {
            return Ok(true);
        }
        if server
            .ok(&["ls"])?
            .lines()
            .any(|line| line.starts_with("client c1 "))
        {
            server.ok(&["str", "c1", "client", "detach"])?;
            return Ok(true);
        }
        Ok(false)
    })?;
    let status = terminal.wait_exit()?;
    let output = String::from_utf8_lossy(&terminal.output);
    assert!(!status.success(), "forwarding failure was lost: {output}");
    assert!(
        output.contains("frame"),
        "the original failure was lost: {output}"
    );
    Ok(())
}
