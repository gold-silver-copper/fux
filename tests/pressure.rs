//! A server under pressure: out of descriptors, or with a pane whose
//! program is not reading.
mod support;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};
use support::*;

/// The tests here measure time and CPU, so they run one at a time: each
/// takes this first, and other tests' load is all they share.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn alone() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// How many log lines mention running out of descriptors.
fn pressure_lines(server: &Server) -> usize {
    server
        .log()
        .lines()
        .filter(|l| l.contains("file descriptors") || l.contains("Too many open files"))
        .count()
}

/// A pane's screen once it settles: the capture after the state line, which
/// names the pane (`%1 reading`) and so would pass for a zsh prompt.
fn screen(server: &Server, pane: &str) -> Result<String, String> {
    let answer = server.ok(&["str", pane, "pane", "capture"])?;
    Ok(answer
        .split_once('\n')
        .map(|(_, s)| s.to_owned())
        .unwrap_or_default())
}

/// Sends keys to a pane, answering at once (`0 timeout`): a program that is
/// stopped, sleeping or hung up never settles.
fn send_now(server: &Server, pane: &str, keys: &[&str]) -> Outcome {
    let mut words = vec!["str", pane, "pane"];
    words.extend_from_slice(keys);
    words.extend(["0", "timeout", "send-keys"]);
    server.ok(&words)?;
    Ok(())
}

/// Runs `fux ls`, timing it.
fn timed_ls(server: &Server) -> Result<(Duration, Output), String> {
    let start = Instant::now();
    let out = server.fux(&["ls"])?;
    Ok((start.elapsed(), out))
}

/// Connections held open without a word exhaust the server's descriptors.
/// A new client is then refused at once, with a reason, rather than left to
/// time out; the server does not spin, and says so in its log once rather
/// than once a tick or a connection; and it recovers when they close
/// (bevy-final findings 006 and 012).
#[test]
fn a_server_out_of_descriptors_refuses_promptly_and_recovers() -> Outcome {
    let _alone = alone();
    let server = Server::start_limited("", Some(64))?;
    let pid = server.pid().ok_or("the server's pid")?;
    assert_eq!(server.fux(&["ls"])?.status, 0);
    // More than its descriptors allow, so that on Linux about a hundred
    // wait in the backlog (it holds 128): refused one a tick, as hunt 8
    // found (012), the clients below would wait seconds behind them. macOS
    // refuses a connection past its shorter backlog at once, which is no
    // failure here.
    let mut held: Vec<UnixStream> = Vec::new();
    for _ in 0..150 {
        match UnixStream::connect(&server.socket) {
            Ok(stream) => held.push(stream),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(error) => return Err(e(error)),
        }
    }
    // Past the server's 64 descriptors, whatever the backlog took.
    assert!(held.len() > 64, "only {} connections held", held.len());
    // Settle: the server takes what it can, and refuses the rest.
    std::thread::sleep(Duration::from_millis(300));
    let lines_before = pressure_lines(&server);
    let cpu_before = cpu_seconds(pid)?;
    // Thirty clients over about two seconds: each refused within a second,
    // with a reason that names the cause. Everything is measured before
    // anything is judged, so a failure reports all of it.
    let mut slowest = Duration::ZERO;
    let mut unclear = Vec::new();
    for _ in 0..30 {
        let (took, out) = timed_ls(&server)?;
        slowest = slowest.max(took);
        if out.status == 0 || !out.stderr.contains("file descriptors") {
            unclear.push(format!("status {}: {:?}", out.status, out.stderr.trim()));
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    let cpu = cpu_seconds(pid)? - cpu_before;
    let lines = pressure_lines(&server);
    let report = format!(
        "slowest client {slowest:?}; {} of 30 refusals unclear ({:?}); \
         {cpu:.2} s of server CPU; {lines} log lines about descriptors ({lines_before} before the clients)",
        unclear.len(),
        unclear.first()
    );
    // Three seconds at most for any client (a fux process starts for
    // each, on a loaded machine), no spinning, and the condition
    // reported once however many clients were refused.
    assert!(
        slowest < Duration::from_secs(3) && unclear.is_empty() && cpu < 0.5 && lines <= 2,
        "{report}"
    );
    eprintln!("{report}");
    drop(held);
    eventually("served again", || Ok(server.fux(&["ls"])?.status == 0))?;
    let (took, out) = timed_ls(&server)?;
    assert_eq!(out.status, 0, "{}", out.stderr);
    assert!(took < Duration::from_secs(3), "{took:?}");
    eventually("the recovery reported", || {
        Ok(server.log().contains("file descriptors available again"))
    })?;
    Ok(())
}

/// The same connections, each closed at once, leave the server reachable.
/// A burst of them can still run it short for a moment, since it accepts
/// what waits before reading any close; that is reported, not a failure.
#[test]
fn connections_that_close_at_once_leave_the_server_reachable() -> Outcome {
    let _alone = alone();
    let server = Server::start_limited("", Some(64))?;
    for _ in 0..200 {
        // A full backlog is refused by the kernel at once, which is fine.
        match UnixStream::connect(&server.socket) {
            Ok(stream) => drop(stream),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(error) => return Err(e(error)),
        }
    }
    eventually("served", || Ok(server.fux(&["ls"])?.status == 0))?;
    let (took, out) = timed_ls(&server)?;
    assert_eq!(out.status, 0, "{}", out.stderr);
    assert!(took < Duration::from_secs(3), "{took:?}");
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
/// what arrives is exactly what was accepted (bevy-final finding 021).
#[test]
fn input_waits_for_a_program_that_is_not_reading() -> Outcome {
    let _alone = alone();
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
/// exits. The server stops polling it rather than spin, and closes the
/// pane when the program exits.
///
/// On Linux, closing the last descriptor on the terminal is enough. On
/// macOS the server holds the terminal open itself (`fuxix::pty::Master`),
/// so it never hangs up there: the pane still closes when the program
/// exits.
#[test]
fn a_program_that_closes_its_terminal_does_not_make_the_server_spin() -> Outcome {
    let _alone = alone();
    let server = Server::start("")?;
    let pid = server.pid().ok_or("the server's pid")?;
    eventually("a prompt", || {
        let screen = screen(&server, "%1")?;
        Ok(screen.contains('$'))
    })?;
    server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
    eventually("a prompt in %2", || {
        let screen = screen(&server, "%2")?;
        Ok(screen.contains('$'))
    })?;
    // At once: settling would wait out the sleep, past the second measured.
    send_now(
        &server,
        "%2",
        &[
            "str",
            "exec sleep 2 <&- >&- 2>&-",
            "text",
            "str",
            "Enter",
            "key",
        ],
    )?;
    // Let the shell exec, then measure a second of the server's CPU.
    std::thread::sleep(Duration::from_millis(300));
    let before = cpu_seconds(pid)?;
    std::thread::sleep(Duration::from_secs(1));
    let cpu = cpu_seconds(pid)? - before;
    assert!(cpu < 0.2, "{cpu:.2} s of server CPU in one second");
    eventually("%2 to close when sleep exits", || {
        Ok(!server.ok(&["ls"])?.contains("%2 "))
    })?;
    Ok(())
}

/// One client flooding a pane whose program does not read its input holds
/// up no one: the server reads at most a bounded amount from it per tick,
/// drops what the full queue refuses, and
/// gives back the memory the flood took. Other commands are served
/// meanwhile, and the server's memory stays small.
#[test]
fn a_client_flooding_a_pane_that_does_not_read_holds_up_no_one() -> Outcome {
    let _alone = alone();
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
        "str",
        "sleep-started",
        "wait-text",
        "send-keys",
    ])?;
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
    let mut slowest = Duration::ZERO;
    for _ in 0..5 {
        let (took, out) = timed_ls(&server)?;
        assert_eq!(out.status, 0, "{}", out.stderr);
        slowest = slowest.max(took);
        std::thread::sleep(Duration::from_millis(200));
    }
    let memory = resident_mib(pid)?;
    assert!(
        slowest < Duration::from_secs(2) && memory < 64.0,
        "slowest fux ls {slowest:?} while flooded; {memory:.0} MiB resident"
    );
    eprintln!("slowest fux ls {slowest:?} while flooded; {memory:.0} MiB resident");
    Ok(())
}

/// Output that spends a parser call's work budget (THREATS.md 4.4) is read
/// on the pane's next turns, the rest of it and what came after: the pane
/// goes on to its end, and the server, with nothing more to read, rests.
/// Each REP is spent as 65,535 copies: 2,000 of them spend a budget. The
/// wait is asked first: an unoptimized build spends seconds on a budget,
/// past a client's patience for the server's greeting.
#[test]
fn output_past_a_read_budget_is_read_on_the_next_turns() -> Outcome {
    let _alone = alone();
    let server = Server::start("")?;
    let pid = server.pid().ok_or("the server's pid")?;
    let code = "sleep 1; printf 'X\\033[65535b%.0s' $(seq 2000); echo; echo DO''NE; sleep 30";
    let pane = server
        .ok(&[
            "opts", "str", "sh", "program", "str", "-c", "arg", "str", code, "arg", "0", "timeout",
            "run",
        ])?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let done = server.fux(&[
        "str",
        &pane,
        "pane",
        "str",
        "DONE",
        "wait-text",
        "60",
        "timeout",
        "wait",
    ])?;
    assert_eq!(done.status, 0, "{}", done.stderr);
    std::thread::sleep(Duration::from_millis(300));
    let before = cpu_seconds(pid)?;
    std::thread::sleep(Duration::from_secs(1));
    let cpu = cpu_seconds(pid)? - before;
    assert!(cpu < 0.2, "{cpu:.2} s of server CPU in one second");
    Ok(())
}

/// A program that prints past a read budget and exits at once is told
/// exited with all it printed on its screen: `run`'s answer, which tells
/// the exit once (#198), has its last line. 4,000 REPs of 65,535 spend
/// two budgets.
#[test]
fn a_run_answers_an_exit_with_the_last_of_the_output() -> Outcome {
    let _alone = alone();
    let server = Server::start("")?;
    let code = "printf 'X\\033[65535b%.0s' $(seq 4000); echo; echo DO''NE";
    let out = server.fux(&[
        "opts", "str", "sh", "program", "str", "-c", "arg", "str", code, "arg", "60", "timeout",
        "run",
    ])?;
    assert!(
        out.stdout.contains("exited 0"),
        "{}{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("DONE"),
        "no DONE in the answer: {}",
        out.stderr
    );
    Ok(())
}
