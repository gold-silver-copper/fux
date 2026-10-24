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

/// A pane whose `cat` writes what it reads, raw, into a file; the pid of
/// `cat`, and the file. `cat` replaces the shell: a job-control shell would
/// take the terminal back from a stopped job, and the job would then read
/// in the background.
fn recorder(server: &Server) -> Result<(i32, std::path::PathBuf), String> {
    let pid_file = server.dir.join("cat.pid");
    let out = server.dir.join("received");
    server.type_line(
        "%1",
        &format!(
            "stty raw -echo; exec sh -c 'echo $$ > \"$0\"; exec cat > \"$1\"' '{}' '{}'",
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
/// Past the queue's bound, input is refused whole with the documented
/// notice, and, as it says, nothing more is queued until the program reads:
/// what arrives is exactly what was accepted.
#[test]
fn input_waits_for_a_program_that_is_not_reading() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 100)?;
    client.ready()?;
    let (cat, out) = recorder(&server)?;
    let _reap = Reap(vec![cat]);
    let mut expected = Vec::new();

    // Keys, one piece each.
    signal(cat, fuxix::process::Signal::Stop);
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
    client.pump()?;
    assert!(!client.bar().contains("not reading"), "{}", client.bar());
    signal(cat, fuxix::process::Signal::Cont);
    received(&out, &expected)?;

    // Eighty pastes of 2000 bytes.
    signal(cat, fuxix::process::Signal::Stop);
    for i in 0..80 {
        let text = pasted(i, 2000);
        client.send(&paste(&text))?;
        expected.extend(text);
    }
    signal(cat, fuxix::process::Signal::Cont);
    received(&out, &expected)?;

    // Past the bound: pastes of 60,000 bytes, more than fit.
    signal(cat, fuxix::process::Signal::Stop);
    let mut pastes = Vec::new();
    for i in 0..20 {
        let text = pasted(i, 60_000);
        client.send(&paste(&text))?;
        pastes.push(text);
    }
    client.wait("the notice", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("not reading its input"))
    })?;
    // Nothing more is queued until the program reads: not even a key.
    client.send(b"Z")?;
    std::thread::sleep(Duration::from_millis(200));
    signal(cat, fuxix::process::Signal::Cont);
    // What arrives is whole pastes, in order, and the key is not among them.
    let before = expected.len();
    // Until the file stops growing.
    let mut last = 0;
    eventually("the accepted pastes", || {
        std::thread::sleep(Duration::from_millis(300));
        let now = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        let settled = now == last && now > u64::try_from(before).map_err(e)?;
        last = now;
        Ok(settled)
    })?;
    let got = std::fs::read(&out).map_err(e)?;
    let tail = got.get(before..).unwrap_or_default();
    let whole = pastes
        .iter()
        .scan(Vec::new(), |sofar, p| {
            sofar.extend_from_slice(p);
            Some(sofar.clone())
        })
        .position(|sofar| sofar == tail);
    let accepted = whole.ok_or_else(|| {
        format!(
            "{} bytes arrived after the stop, not a run of whole pastes{}",
            tail.len(),
            if tail.last() == Some(&b'Z') {
                ", and the key refused by the notice arrived"
            } else {
                ""
            }
        )
    })?;
    assert!(
        accepted < 19,
        "every paste fitted: the bound was never reached"
    );
    // Reading again, input is accepted again.
    client.send(b"Y")?;
    let mut expected = got;
    expected.push(b'Y');
    received(&out, &expected)?;
    Ok(())
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
    server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
    eventually(
        "a prompt in %2",
        || Ok(screen(&server, "%2")?.contains('$')),
    )?;
    // At once: settling would wait out the sleep.
    let line = "exec sleep 1 <&- >&- 2>&-";
    send_now(&server, "%2", &["str", line, "text", "str", "Enter", "key"])?;
    eventually("%2 to close when sleep exits", || {
        Ok(!server.ok(&["ls"])?.contains("%2 "))
    })?;
    assert_eq!(server.fux(&["ls"])?.status, 0);
    Ok(())
}

/// One client flooding a pane whose program does not read its input holds
/// up no one: the server reads at most a bounded amount from it per tick,
/// drops what the full queue refuses, and
/// gives back the memory the flood took. Other commands are served
/// meanwhile, and the server's memory stays small.
#[test]
fn a_client_flooding_a_pane_that_does_not_read_holds_up_no_one() -> Outcome {
    use fux::act::Act;
    use fux::protocol::{Attach, AttachedFrame, Frame, Hello, PROTOCOL, Role};
    use std::io::Write;
    let server = Server::start("")?;
    let pid = server.pid().ok_or("the server's pid")?;
    // sleep reads nothing, so the terminal's buffer and then the pane's
    // input queue fill. A sleeping program never settles: the keys wait
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
            size: sized(10, 40)?,
            workspace: None,
            read_only: false,
            env: Vec::new(),
        };
        stream.write_all(&hello.encode().map_err(e)?).map_err(e)?;
        stream.write_all(&attach.encode().map_err(e)?).map_err(e)?;
        let input = AttachedFrame::Act(Box::new(Act::Paste(
            std::iter::repeat_n('a', 500_000).collect(),
        )))
        .encode()
        .map_err(e)?;
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
    let memory = resident_mib(pid)?;
    assert!(memory < 64.0, "{memory:.0} MiB resident while flooded");
    Ok(())
}

/// Output that spends a parser call's work budget (THREATS.md 4.4) is read
/// on the pane's next turns, to its end. A program that prints past a read
/// budget and exits at once is told exited with all it printed on its
/// screen: its last line is there as the exit is told. 4,000 REPs of
/// 65,535 spend two budgets.
#[test]
fn a_run_answers_an_exit_with_the_last_of_the_output() -> Outcome {
    let server = Server::start("")?;
    let code = "printf 'X\\033[65535b%.0s' $(seq 4000); echo; echo DO''NE";
    let out = server.fux(&[
        "opts", "str", "sh", "program", "str", "-c", "arg", "str", code, "arg", "run",
    ])?;
    let pane = out.stdout.split_whitespace().next().ok_or("its pane")?;
    let exited = server.exits(pane)?;
    assert!(exited.contains("exited 0"), "{exited}");
    let out = server.fux(&["str", pane, "pane", "capture"])?;
    assert!(
        out.stdout.contains("DONE"),
        "no DONE on its screen: {}",
        out.stdout
    );
    Ok(())
}
