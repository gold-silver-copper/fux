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
