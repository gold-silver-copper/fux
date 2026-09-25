//! A server under pressure: out of descriptors, or with a pane whose
//! program is not reading.
mod support;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};
use support::*;

/// How many log lines mention running out of descriptors.
fn pressure_lines(server: &Server) -> usize {
    server
        .log()
        .lines()
        .filter(|l| l.contains("file descriptors") || l.contains("Too many open files"))
        .count()
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
    let server = Server::start_limited("", Some(64))?;
    let pid = server.pid().ok_or("the server's pid")?;
    assert_eq!(server.fux(&["ls"])?.status, 0);
    let held: Vec<UnixStream> = (0..100)
        .map(|_| UnixStream::connect(&server.socket).map_err(e))
        .collect::<Result<_, _>>()?;
    // Settle: the server takes what it can.
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
    // A second at most for any client, no spinning, and the condition
    // reported once however many clients were refused.
    assert!(
        slowest < Duration::from_secs(1) && unclear.is_empty() && cpu < 0.5 && lines <= 2,
        "{report}"
    );
    eprintln!("{report}");
    drop(held);
    eventually("served again", || Ok(server.fux(&["ls"])?.status == 0))?;
    let (took, out) = timed_ls(&server)?;
    assert_eq!(out.status, 0, "{}", out.stderr);
    assert!(took < Duration::from_secs(1), "{took:?}");
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
    assert!(took < Duration::from_secs(1), "{took:?}");
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
    let server = Server::start("")?;
    let mut client = server.attach(20, 100)?;
    let (cat, out) = recorder(&server)?;
    let _reap = Reap(vec![cat]);
    let mut expected = Vec::new();

    // Keys, one piece each.
    signal(cat, rustix::process::Signal::STOP);
    for i in 0..3000u32 {
        let key = b'a'.saturating_add(u8::try_from(i % 26).map_err(e)?);
        client.send(&[key])?;
        expected.push(key);
    }
    for i in 0..200u32 {
        let key = char::from(b'A'.saturating_add(u8::try_from(i % 26).map_err(e)?));
        server.ok(&["send-keys", "-t", "%1", "-l", &key.to_string()])?;
        expected.extend(key.to_string().bytes());
    }
    client.pump()?;
    assert!(!client.bar().contains("not reading"), "{}", client.bar());
    signal(cat, rustix::process::Signal::CONT);
    received(&out, &expected)?;

    // Eighty pastes of 2000 bytes.
    signal(cat, rustix::process::Signal::STOP);
    for i in 0..80 {
        let text = pasted(i, 2000);
        client.send(&paste(&text))?;
        expected.extend(text);
    }
    signal(cat, rustix::process::Signal::CONT);
    received(&out, &expected)?;

    // Past the bound: pastes of 60,000 bytes, more than fit.
    signal(cat, rustix::process::Signal::STOP);
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
    signal(cat, rustix::process::Signal::CONT);
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
