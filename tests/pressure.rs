//! A server under pressure: out of descriptors, or with a pane whose
//! program is not reading.
mod support;
use std::os::unix::net::UnixStream;
use std::time::Duration;
use support::*;

/// How many log lines mention running out of descriptors.
fn pressure_lines(server: &Server) -> usize {
    server
        .log()
        .lines()
        .filter(|l| l.contains("Too many open files"))
        .count()
}

/// A pane's screen.
fn screen(server: &Server, pane: &str) -> Result<String, String> {
    server.ok(&["str", pane, "pane", "capture"])
}

/// Sends keys to a pane.
fn send_now(server: &Server, pane: &str, keys: &[&str]) -> Outcome {
    let mut words = vec!["str", pane, "pane"];
    words.extend_from_slice(keys);
    words.extend(["send-keys"]);
    server.ok(&words)?;
    Ok(())
}

/// Connections held open without a word exhaust the server's descriptors.
/// The server says so in its log once, not once a tick or a connection, and
/// serves again when they close; as it does after a burst of connections
/// each closed at once.
#[test]
fn a_server_out_of_descriptors_recovers() -> Outcome {
    let server = Server::start_limited("", Some(64))?;
    assert_eq!(server.fux(&["ls"])?.status, 0);
    // A full backlog is refused by the kernel at once, which is fine.
    let connect = || match UnixStream::connect(&server.socket) {
        Ok(stream) => Ok(Some(stream)),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => Ok(None),
        Err(error) => Err(e(error)),
    };
    let mut held = Vec::new();
    for _ in 0..100 {
        held.extend(connect()?);
    }
    // Past the server's 64 descriptors, whatever the backlog took.
    assert!(held.len() > 64, "only {} connections held", held.len());
    eventually("the shortage logged", || Ok(pressure_lines(&server) > 0))?;
    drop(held);
    eventually("served again", || Ok(server.fux(&["ls"])?.status == 0))?;
    for _ in 0..200 {
        drop(connect()?);
    }
    eventually("served", || Ok(server.fux(&["ls"])?.status == 0))?;
    let lines = pressure_lines(&server);
    assert!(lines <= 2, "{lines} log lines about descriptors");
    Ok(())
}

/// A pane whose `cat` writes what it reads, raw, into a file, after the
/// shell writes `first` to the terminal; the pid of `cat`, and the file.
/// `cat` replaces the shell: a job-control shell would take the terminal
/// back from a stopped job, and the job would then read in the background.
fn recorder(server: &Server, first: &str) -> Result<(i32, std::path::PathBuf), String> {
    let pid_file = server.dir.join("cat.pid");
    let out = server.dir.join("received");
    server.type_line(
        "%1",
        &format!(
            "stty raw -echo; printf '{first}'; exec sh -c 'echo $$ > \"$0\"; exec cat > \"$1\"' '{}' '{}'",
            pid_file.display(),
            out.display()
        ),
    )?;
    Ok((pid_in(&pid_file)?, out))
}

/// Waits until the file holds exactly `expected`.
fn received(out: &std::path::Path, expected: &[u8]) -> Outcome {
    eventually("everything received", || {
        Ok(std::fs::read(out).unwrap_or_default().len() >= expected.len())
    })
    .map_err(|error| {
        format!(
            "{error}: {} of {} bytes",
            std::fs::read(out).unwrap_or_default().len(),
            expected.len()
        )
    })?;
    // Settled: nothing more is on its way.
    std::thread::sleep(Duration::from_millis(200));
    let got = std::fs::read(out).map_err(e)?;
    if got != expected {
        return Err(format!(
            "{} bytes received, {} expected; the first difference is at {}",
            got.len(),
            expected.len(),
            got.iter()
                .zip(expected)
                .position(|(a, b)| a != b)
                .unwrap_or(got.len().min(expected.len()))
        ));
    }
    Ok(())
}

/// The real binding worker sends its first request with the attachment's
/// prefix, and held keys follow the completed program.
#[test]
fn a_bound_program_follows_the_keys_before_it() -> Outcome {
    let server = Server::start("str F5 str 'me focused str Z text send-keys drop' bind-root")?;
    let mut client = server.attach(20, 100)?;
    client.ready()?;
    let (cat, out) = recorder(&server, "")?;
    let _reap = Reap(vec![cat]);
    let mut typed = vec![b'a'; 3000];
    typed.extend_from_slice(b"\x1b[15~x");
    client.send(&typed)?;
    let mut expected = vec![b'a'; 3000];
    expected.extend_from_slice(b"Zx");
    received(&out, &expected)
}

/// Stops `pid` and waits until it has stopped, so it reads nothing more.
fn stop(pid: i32) -> Outcome {
    signal(pid, fuxix::process::Signal::Stop);
    eventually("stopped", || {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map_err(e)?;
        Ok(out.stdout.starts_with(b"T"))
    })
}

/// A bracketed paste of `text`, as a terminal sends one.
fn paste(text: &[u8]) -> Vec<u8> {
    [b"\x1b[200~".as_slice(), text, b"\x1b[201~"].concat()
}

/// Paste `i` of `len` bytes: letters that tell pastes and their order apart.
fn pasted(i: usize, len: usize) -> Vec<u8> {
    (0..len)
        .map(|j| {
            b'a'.saturating_add(u8::try_from((i.wrapping_mul(7).wrapping_add(j)) % 26).unwrap_or(0))
        })
        .collect()
}

/// Input for a program that has stopped reading waits for it rather than
/// being lost: thousands of single keys, from a client and from
/// `send-keys`, and many pastes, all arrive in order once it reads again.
#[test]
fn input_waits_while_its_program_does_not_read() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 100)?;
    client.ready()?;
    let (cat, out) = recorder(&server, "")?;
    let _reap = Reap(vec![cat]);
    let mut expected = Vec::new();

    // Keys, one piece each.
    stop(cat)?;
    for i in 0..3000u32 {
        let key = b'a'.saturating_add(u8::try_from(i % 26).map_err(e)?);
        client.send(&[key])?;
        expected.push(key);
    }
    for i in 0..200u32 {
        let key = char::from(b'A'.saturating_add(u8::try_from(i % 26).map_err(e)?));
        send_now(&server, "%1", &["str", &key.to_string(), "text"])?;
        expected.extend(key.to_string().bytes());
    }
    signal(cat, fuxix::process::Signal::Cont);
    received(&out, &expected)?;

    // Eighty pastes of 2000 bytes.
    stop(cat)?;
    for i in 0..80 {
        let text = pasted(i, 2000);
        client.send(&paste(&text))?;
        expected.extend(text);
    }
    signal(cat, fuxix::process::Signal::Cont);
    received(&out, &expected)
}

/// Frames a client sends faster than a tick handles them wait for its
/// next turns: thousands in one write, with nothing sent after them, all
/// arrive in order.
#[test]
fn frames_past_a_turn_wait_for_the_next() -> Outcome {
    use fux_wire::act::Act;
    use fux_wire::protocol::{AttachedFrame, Frame};
    let server = Server::start("")?;
    let mut client = server.attach(20, 100)?;
    client.ready()?;
    let (cat, out) = recorder(&server, "")?;
    let _reap = Reap(vec![cat]);
    let expected = pasted(0, 3000);
    let mut frames = Vec::new();
    for &byte in &expected {
        let act = Act::Paste(char::from(byte).to_string());
        frames.extend(AttachedFrame::Act(Box::new(act)).encode());
    }
    client.write(&frames)?;
    received(&out, &expected)
}

/// A program that closes its terminal but keeps running leaves its PTY
/// hung up: the master reports the end on every poll until the program
/// exits. The server stops polling it, and closes the pane when the
/// program exits. (On macOS the server holds the terminal open itself,
/// `fuxix::pty::Master`, so it never hangs up there.)
#[test]
fn a_program_that_closes_its_terminal_closes_its_pane_at_exit() -> Outcome {
    let server = Server::start("")?;
    eventually("a prompt", || Ok(screen(&server, "%1")?.contains('$')))?;
    server.ok(&["str", "%1", "pane", "str", "horizontal", "axis", "split"])?;
    eventually(
        "a prompt in %2",
        || Ok(screen(&server, "%2")?.contains('$')),
    )?;
    // At once: settling would wait out the sleep.
    let line = "exec sleep 1 <&- >&- 2>&-";
    send_now(&server, "%2", &["str", line, "text", "str", "Enter", "key"])?;
    eventually("%2 to close when sleep exits", || {
        Ok(!server.ok(&["ls", "listing"])?.contains("%2 "))
    })?;
    assert_eq!(server.fux(&["ls"])?.status, 0);
    Ok(())
}

/// One client flooding a pane whose program does not read its input holds
/// up no one: the server reads a bounded amount from it per tick, and other
/// commands are served meanwhile.
#[test]
fn a_client_flooding_a_pane_that_does_not_read_holds_up_no_one() -> Outcome {
    use fux_wire::act::Act;
    use fux_wire::protocol::{Attach, AttachedFrame, Frame, Hello, PROTOCOL, Role};
    use std::io::Write;
    let server = Server::start("")?;
    // sleep reads nothing, so the terminal's buffer fills, and the pane's
    // input queue grows: memory is the user's and the OS's to bound. A sleeping program never settles: the keys wait
    // for what the shell prints just before it starts sleep, computed so
    // the echo never matches.
    server.ok(&[
        "str",
        "%1",
        "pane",
        "str",
        "printf 'sleep-%s\\n' started; sleep 60",
        "text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    server.shows("%1", "sleep-started")?;
    let socket = server.socket.clone();
    std::thread::spawn(move || -> Outcome {
        let mut stream = UnixStream::connect(&socket).map_err(e)?;
        let hello = Hello {
            protocol: PROTOCOL,
            version: "flood".into(),
            role: Role::Attach,
        };
        let attach = Attach {
            of: None,
            headless: false,
            size: sized(10, 40)?,
            target: None,
            read_only: false,
            initial: None,
            presentation: fux::script::default_state()
                .map_err(|e| e.to_string())?
                .preferences
                .presentation(),
            ancestry: Vec::new(),
        };
        stream.write_all(&hello.encode()).map_err(e)?;
        stream.write_all(&attach.encode()).map_err(e)?;
        let input = AttachedFrame::Act(Box::new(Act::Paste(
            std::iter::repeat_n('a', 500_000).collect(),
        )))
        .encode();
        // Until the server stops reading, or the test ends and it goes.
        for _ in 0..128 {
            stream.write_all(&input).map_err(e)?;
        }
        Ok(())
    });
    std::thread::sleep(Duration::from_millis(300));
    for _ in 0..5 {
        let out = server.fux(&["ls"])?;
        assert_eq!(out.status, 0, "{}", out.stderr);
    }
    Ok(())
}

/// Output that spends a parser call's work budget (THREATS.md 4) is read
/// on the pane's next turns, to its end. A program that prints past a read
/// budget and exits at once is told exited with all it printed on its
/// screen: its last line is there as the exit is told. 4,000 REPs of
/// 65,535 spend two budgets.
#[test]
fn a_run_answers_an_exit_with_the_last_of_the_output() -> Outcome {
    let server = Server::start("")?;
    let code = "printf 'X\\033[65535b%.0s' $(seq 4000); echo; echo DO''NE";
    let out = server.fux(&[
        "agent-opts",
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
        code,
        "arg",
        "new-tab",
    ])?;
    let pane = out.stdout.split(':').next().ok_or("its pane")?;
    assert_eq!(server.exits(pane)?, "0");
    let out = server.fux(&["str", pane, "pane", "capture"])?;
    assert!(
        out.stdout.contains("DONE"),
        "no DONE on its screen: {}",
        out.stdout
    );
    Ok(())
}

/// A program that exits leaving a writer on its terminal, in a group of its
/// own that ignores the hangup and keeps the terminal full, holds up no
/// one (D-90): its exit is told as its leader is reaped, the writer still
/// writing; its terminal is read on, its counts growing, while other panes
/// and commands are served; once the writer is killed and its terminal
/// drained, the pane is frozen.
#[test]
fn a_writer_left_on_a_terminal_holds_up_no_one() -> Outcome {
    let server = Server::start("")?;
    eventually("a prompt", || Ok(screen(&server, "%1")?.contains('$')))?;
    // DECALN, which `cat` writes faster than fux reads it.
    let fill = server.dir.join("decaln");
    let decaln: Vec<u8> = std::iter::repeat_n(*b"\x1b#8", 1 << 18).flatten().collect();
    std::fs::write(&fill, decaln).map_err(e)?;
    let pid_file = server.dir.join("writer.pid");
    let code = format!(
        "set -m; trap '' HUP; (while :; do cat '{}'; done) & echo $! > '{}'; sleep 1; exit 3",
        fill.display(),
        pid_file.display()
    );
    let words = ["agent-opts", "80", "24", "size", "str", "sh", "program"];
    let words = words
        .into_iter()
        .chain(["str", "-c", "arg", "str", &code, "arg", "new-tab"]);
    server.ok(&words.collect::<Vec<_>>())?;
    let writer = pid_in(&pid_file)?;
    let group = fuxix::process::Pid::from_raw(writer).ok_or("the writer's group")?;
    let _reap = Reap(vec![writer]);
    let listed = || server.ok(&["ls", "listing"]);
    eventually("the exit told, the writer still writing", || {
        Ok(listed()?.contains("%2 sh 80x24 exited 3"))
    })?;
    let screen_count = || -> Result<u64, String> {
        let counts = server.ok(&["str", "%2", "pane", "counts"])?;
        let count = counts.split_whitespace().nth(1).ok_or("its counts")?;
        count.parse().map_err(e)
    };
    // Read on: its screen's revision grows.
    let at_exit = screen_count()?;
    eventually("its terminal read on", || Ok(screen_count()? > at_exit))?;
    server.ok(&[
        "str",
        "%1",
        "pane",
        "str",
        "echo rea''dy",
        "text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    server.shows("%1", "ready")?;
    // Once the writer is killed, its terminal drained, it is frozen.
    let _ = fuxix::process::kill_group(group, fuxix::process::Signal::Kill);
    let mut last = screen_count()?;
    eventually("frozen once drained", || {
        std::thread::sleep(Duration::from_millis(200));
        let now = screen_count()?;
        let still = now == last;
        last = now;
        Ok(still)
    })?;
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(screen_count()?, last, "read on once drained");
    Ok(())
}
