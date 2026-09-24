//! Milestone 2: one pane. Attach, detach and reattach, resize, and
//! `capture-pane`.
mod support;
use support::*;

#[test]
fn a_client_sees_the_shell_and_types_into_it() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 40)?;
    client.wait_for("$")?;
    client.keys("echo one-two\r")?;
    client.wait(&"the echo's output", |t| t.lines().any(|l| l == "one-two"))?;
    let bar = client.bar();
    assert!(bar.contains("main"), "{bar}");
    assert!(bar.contains("%1 sh"), "{bar}");
    // The pane is the client's size, less the bar.
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("%1 sh 40x9"), "{ls}");
    assert!(ls.contains("client c1 40x10 +1 @1 %1"), "{ls}");
    Ok(())
}

#[test]
fn detach_and_reattach_keep_the_shell_and_its_screen() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 40)?;
    client.wait_for("$")?;
    client.keys("echo before-detach\r")?;
    client.wait(&"output", |t| t.lines().any(|l| l == "before-detach"))?;
    client.keys("\x02d")?;
    assert_eq!(client.wait_exit()?, "detached");
    let ls = server.ok(&["ls"])?;
    assert!(!ls.contains("client"), "the client is gone: {ls}");
    let mut again = server.attach(10, 40)?;
    again.wait(&"the old output", |t| {
        t.lines().any(|l| l == "before-detach")
    })?;
    again.keys("echo after\r")?;
    again.wait(&"new output", |t| t.lines().any(|l| l == "after"))?;
    // detach from the command line needs the client.
    let out = server.fux(&["detach", "-c", "c2"])?;
    assert_eq!(out.status, 0, "{}", out.stderr);
    assert_eq!(again.wait_exit()?, "detached");
    Ok(())
}

#[test]
fn a_resize_reaches_the_program() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 40)?;
    client.wait_for("$")?;
    client.resize(20, 60)?;
    client.keys("stty size\r")?;
    client.wait(&"the new size", |t| t.lines().any(|l| l == "19 60"))?;
    assert!(server.ok(&["ls"])?.contains("%1 sh 60x19"));
    // Clamped to 1..=4096 each way; a one-row client has no room for panes.
    client.resize(1, 5000)?;
    eventually("the clamp", || {
        Ok(server.ok(&["ls"])?.contains("client c1 4096x1"))
    })?;
    Ok(())
}

#[test]
fn capture_pane_shows_the_screen_and_history() -> Outcome {
    let server = Server::start("set history-lines 100")?;
    let mut client = server.attach(6, 30)?;
    client.wait_for("$")?;
    client.keys("for i in 1 2 3 4 5 6 7 8; do echo line$i; done\r")?;
    client.wait(&"the loop", |t| t.lines().any(|l| l == "line8"))?;
    let screen = server.ok(&["capture-pane", "-t", "%1"])?;
    assert!(screen.contains("line8"), "{screen}");
    assert!(
        !screen.contains("line1\n"),
        "line1 has scrolled into history: {screen}"
    );
    let history = server.ok(&["capture-pane", "-t", "%1", "-S", "-20"])?;
    assert!(history.contains("line1\n"), "{history}");
    let json = server.ok(&["capture-pane", "-t", "%1", "--json"])?;
    assert!(
        json.starts_with("{\"pane\":\"%1\",\"rows\":5,\"cols\":30,\"cursor\":["),
        "{json}"
    );
    assert!(json.contains("\"line8\""), "{json}");
    // From inside the pane, FUX_PANE targets it without -t.
    let inside = server.fux_env(&["capture-pane"], &[("FUX_PANE", "%1")])?;
    assert_eq!(inside.status, 0);
    assert!(inside.stdout.contains("line8"));
    Ok(())
}

#[test]
fn the_real_client_attaches_restores_its_terminal_and_reattaches() -> Outcome {
    let server = Server::start("")?;
    let mut terminal = Terminal::attach(&server, 12, 50, &[])?;
    terminal.wait_for("%1 sh")?;
    terminal.type_bytes(b"echo real-client\r")?;
    terminal.wait_for("real-client")?;
    terminal.resize(15, 70)?;
    terminal.type_bytes(b"stty size\r")?;
    terminal.wait_for("14 70")?;
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
    assert!(output.contains("[detached]"), "{output:?}");
    // No mouse reporting is ever enabled.
    assert!(!output.contains("\x1b[?1000h") && !output.contains("\x1b[?1006h"));
    // And the shell is still there for the next attach.
    let mut again = Terminal::attach(&server, 12, 50, &[])?;
    again.wait_for("real-client")?;
    Ok(())
}

#[test]
fn attaching_inside_a_pane_is_refused_unless_nested() -> Outcome {
    let server = Server::start("")?;
    let out = server.fux_env(&["attach"], &[("FUX_PANE", "%1")])?;
    assert_eq!(out.status, 1);
    assert!(out.stderr.contains("--nested"), "{}", out.stderr);
    Ok(())
}

#[test]
fn attach_starts_a_server_when_none_answers() -> Outcome {
    let dir = std::env::temp_dir()
        .canonicalize()
        .map_err(e)?
        .join(format!("fux-auto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(e)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    let socket = dir.join("s").join("fux.sock");
    // A server handle that only cleans up; the client starts the real one.
    let (master, slave) = fux::process::open_pty(10, 40)?;
    let stdio =
        |fd: &std::os::fd::OwnedFd| fd.try_clone().map(std::process::Stdio::from).map_err(e);
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
        .arg("kill-server")
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
    // Descriptors 3..9, whatever the server holds there, are not open here.
    client.keys("for n in 3 4 5 6 7 8 9; do (: >&$n) 2>/dev/null && echo fd$n-open; done; echo fds-checked\r")?;
    client.wait(&"the check", |t| t.lines().any(|l| l == "fds-checked"))?;
    let open: Vec<String> = client
        .lines()
        .into_iter()
        .filter(|l| l.starts_with("fd") && l.ends_with("-open"))
        .collect();
    assert!(open.is_empty(), "{open:?}");
    client.keys("ps -o sigmask= -p $$ | tr -d ' 0'; echo mask-checked\r")?;
    client.wait(&"the mask", |t| t.lines().any(|l| l == "mask-checked"))?;
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
    assert!(reason.contains("the last pane closed"), "{reason}");
    assert!(server.wait_exit()?.success());
    assert!(!server.socket.exists());
    Ok(())
}
