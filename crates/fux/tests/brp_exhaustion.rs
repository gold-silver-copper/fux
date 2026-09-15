//! Prompt section 5, BRP bullet, and 3.9: resource-exhaustion scenarios against a real
//! server on both HTTP transports. The `bounded_*` tests assert the contracts of fux's own
//! acceptor (`remote/http.rs`: body 1 MiB, batch 64, 256 connections, 10 s head and body
//! deadlines) as well as the prompt's contract that other requests keep being answered; the
//! `bevy_remote_*` tests run the same scenarios against `RemoteHttpPlugin` and assert only the
//! prompt's contract, recording what they observe. Numbers travel in the assertion messages and
//! on stderr; `docs/verification.md` ("BRP resource exhaustion") and `docs/security.md` carry
//! them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "integration-test helpers; the observed numbers are the record"
)]
mod common;

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use common::Server;
use fux::remote::HttpTransport;
use fux::remote::client::{self, ClientError};
use fux::remote::http::{MAX_BATCH, MAX_BODY, MAX_CONNECTIONS, READ_TIMEOUT};
use serde_json::{Value, json};

/// The prompt's contract: a request made while a scenario is in flight is answered within this.
const ANSWER_WITHIN: Duration = Duration::from_secs(2);
const MIB: usize = 1024 * 1024;
/// hyper's default `header_read_timeout` (the only deadline `RemoteHttpPlugin` sets) is 30 s;
/// waiting a little past it separates "reaped by that timer" from "held forever".
const REAP_WINDOW: Duration = Duration::from_secs(36);

fn addr(server: &Server) -> SocketAddr {
    format!(
        "{}:{}",
        server.descriptor.http.host, server.descriptor.http.port
    )
    .parse()
    .unwrap()
}

fn connect_within(server: &Server, timeout: Duration) -> std::io::Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&addr(server), timeout)?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    Ok(stream)
}

fn connect(server: &Server) -> TcpStream {
    connect_within(server, Duration::from_secs(5)).unwrap()
}

fn request_head(content_length: usize) -> String {
    format!(
        "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
    )
}

/// A `fux/server.info` request with the server's own credentials, as the client sends it.
fn info_request(server: &Server) -> Vec<u8> {
    client::encode_request(
        "fux/server.info",
        json!({
            "token": server.descriptor.token,
            "instance": server.descriptor.instance,
        }),
    )
    .unwrap()
}

/// `n` copies of the `fux/server.info` request as one JSON-RPC batch.
fn info_batch(server: &Server, n: usize) -> Vec<u8> {
    let element = serde_json::from_slice::<Value>(&info_request(server)).unwrap();
    let batch: Vec<Value> = (0..n)
        .map(|i| {
            let mut e = element.clone();
            e["id"] = json!(i);
            e
        })
        .collect();
    serde_json::to_vec(&batch).unwrap()
}

/// One `fux/server.info` round trip and how long it took.
fn probe(server: &Server) -> (Result<Value, ClientError>, Duration) {
    let started = Instant::now();
    let result = server.call("fux/server.info", json!({}));
    (result, started.elapsed())
}

fn assert_answers_within(server: &Server, within: Duration, during: &str) -> Duration {
    let (result, took) = probe(server);
    assert!(
        result.is_ok() && took <= within,
        "{during}: fux/server.info took {} ms, result {result:?}",
        took.as_millis()
    );
    took
}

fn assert_answers(server: &Server, during: &str) -> Duration {
    assert_answers_within(server, ANSWER_WITHIN, during)
}

/// Resident set size of this process in bytes (the server runs in-process on its own thread).
fn rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let line = status.lines().find(|l| l.starts_with("VmRSS:")).unwrap();
        let kib: u64 = line
            .split_whitespace()
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap();
        kib * 1024
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        let kib: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        kib * 1024
    }
}

fn mib(bytes: u64) -> u64 {
    bytes / MIB as u64
}

/// Reads one reply to EOF (`Connection: close`) and parses it.
fn read_reply(stream: &mut TcpStream) -> Result<Value, ClientError> {
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    client::parse_response(&raw)
}

fn http_status(reply: &Result<Value, ClientError>) -> Option<u16> {
    match reply {
        Err(ClientError::Http { status, .. }) => Some(*status),
        _ => None,
    }
}

/// Time until the server closes `stream` (a read returning EOF), or `None` within `window`.
fn closed_within(stream: &mut TcpStream, since: Instant, window: Duration) -> Option<Duration> {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut byte = [0u8; 1];
    while since.elapsed() < window {
        match stream.read(&mut byte) {
            Ok(0) => return Some(since.elapsed()),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return Some(since.elapsed()),
        }
    }
    None
}

fn describe_close(reaped: Option<Duration>, window: Duration) -> String {
    reaped.map_or_else(
        || format!("still open after {} s", window.as_secs()),
        |t| format!("closed by the server after {:.1} s", t.as_secs_f64()),
    )
}

/// Lifts the soft `RLIMIT_NOFILE` so the scenario, not the shell's default (256 on macOS),
/// decides how many sockets this process holds; the 256 case is recorded in
/// `docs/verification.md`.
fn ensure_fd_limit(at_least: u64) {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};
    let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
    if soft < at_least {
        setrlimit(Resource::RLIMIT_NOFILE, at_least.min(hard), hard).unwrap();
    }
}

/// Opens up to `n` connections that never send; the ones the OS refused to queue are counted.
fn open_idle(server: &Server, n: usize) -> (Vec<TcpStream>, usize) {
    let mut idle = Vec::with_capacity(n);
    let mut refused = 0;
    for _ in 0..n {
        match connect_within(server, Duration::from_secs(1)) {
            Ok(stream) => idle.push(stream),
            Err(_) => refused += 1,
        }
    }
    (idle, refused)
}

// ---- fux's bounded acceptor (the default transport) -------------------------------------

/// (a) A 64 MiB `Content-Length` body is discarded, never buffered, and answered 413 once it
/// has arrived; a body without a length is cut off at the limit the same way.
#[test]
fn bounded_oversized_body_is_refused_unbuffered() {
    let server = Server::start();
    let before = rss_bytes();
    let total = 64 * MIB;
    let mut stream = connect(&server);
    stream.write_all(request_head(total).as_bytes()).unwrap();
    let chunk = vec![b' '; MIB];
    let started = Instant::now();
    let mut sent = 0;
    let mut peak = before;
    while sent < total {
        if stream.write_all(&chunk).is_err() {
            break;
        }
        sent += chunk.len();
        if sent % (16 * MIB) == 0 {
            peak = peak.max(rss_bytes());
        }
    }
    let reply = read_reply(&mut stream);
    let refused_in = started.elapsed();
    assert_eq!(
        http_status(&reply),
        Some(413),
        "a 64 MiB Content-Length must be answered 413 ({} MiB sent): {reply:?}",
        sent / MIB
    );
    let held = rss_bytes();
    assert!(
        peak.saturating_sub(before) < 32 * MIB as u64,
        "a 64 MiB body must not be buffered: rss {} -> {} MiB while it streamed",
        mib(before),
        mib(peak)
    );
    let took = assert_answers(&server, "after a refused 64 MiB request");

    // Chunked, no Content-Length: 2 MiB of whitespace in 64 KiB chunks.
    let mut chunked = connect(&server);
    chunked
        .write_all(b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
        .unwrap();
    let piece = vec![b' '; 64 * 1024];
    let mut chunked_sent = 0;
    while chunked_sent < 2 * MIB {
        if chunked
            .write_all(format!("{:x}\r\n", piece.len()).as_bytes())
            .and_then(|()| chunked.write_all(&piece))
            .and_then(|()| chunked.write_all(b"\r\n"))
            .is_err()
        {
            break;
        }
        chunked_sent += piece.len();
    }
    let _ = chunked.write_all(b"0\r\n\r\n");
    let reply = read_reply(&mut chunked);
    assert_eq!(
        http_status(&reply),
        Some(413),
        "an unannounced body over {MAX_BODY} bytes must be answered 413 (after {chunked_sent} bytes): {reply:?}"
    );
    let after = rss_bytes();
    eprintln!(
        "bounded oversized body: 64 MiB Content-Length answered 413 in {} ms with {} MiB sent; rss {} MiB before, peak {} MiB while it streamed, {} MiB after, {} MiB after a 2 MiB chunked body (413 after {} KiB sent); parallel server.info {} ms",
        refused_in.as_millis(),
        sent / MIB,
        mib(before),
        mib(peak),
        mib(held),
        mib(after),
        chunked_sent / 1024,
        took.as_millis()
    );
}

/// (b) A 10,000-element batch is refused (it is over the body limit before it is over the
/// batch limit); 65 elements are refused as a batch; 64 are answered.
#[test]
fn bounded_batch_is_limited() {
    let server = Server::start_on(HttpTransport::Bounded, true);
    let body = info_batch(&server, 10_000);
    let mut stream = connect(&server);
    stream
        .write_all(request_head(body.len()).as_bytes())
        .unwrap();
    let _ = stream.write_all(&body);
    let reply = read_reply(&mut stream);
    assert_eq!(
        http_status(&reply),
        Some(413),
        "a 10,000-element batch ({} bytes) must be refused: {reply:?}",
        body.len()
    );

    let body = info_batch(&server, MAX_BATCH + 1);
    let mut stream = connect(&server);
    stream
        .write_all(request_head(body.len()).as_bytes())
        .unwrap();
    stream.write_all(&body).unwrap();
    let reply = read_reply(&mut stream).unwrap();
    let error = reply["error"]["message"].as_str().unwrap_or_default();
    assert!(
        reply["id"].is_null() && error.contains("exceeds the limit of 64"),
        "a {}-element batch must be refused as a batch: {reply}",
        MAX_BATCH + 1
    );

    let body = info_batch(&server, MAX_BATCH);
    let mut stream = connect(&server);
    stream
        .write_all(request_head(body.len()).as_bytes())
        .unwrap();
    stream.write_all(&body).unwrap();
    let started = Instant::now();
    let batch = std::thread::spawn(move || (read_reply(&mut stream), started.elapsed()));
    let took = assert_answers(&server, "during a 64-element batch");
    let (reply, batch_took) = batch.join().unwrap();
    let replies = reply.unwrap();
    let replies = replies.as_array().unwrap();
    assert_eq!(replies.len(), MAX_BATCH, "batch reply count");
    assert!(
        replies.iter().all(|r| r.get("result").is_some()),
        "every element of a 64-element batch is answered with a result"
    );
    eprintln!(
        "bounded batch: 10,000 elements 413 (body {} KiB); 65 elements refused as a batch; 64 elements answered in {} ms ({} us per element); parallel server.info {} ms",
        info_batch(&server, 10_000).len() / 1024,
        batch_took.as_millis(),
        batch_took.as_micros() / MAX_BATCH as u128,
        took.as_millis()
    );
}

/// (c) 1,000 idle connections: 256 are served (and wait on the head deadline), 128 sit in
/// the OS backlog and the rest are refused by the kernel; so is a request made meanwhile, and
/// it gets through once the deadline has reaped the idle ones.
#[test]
fn bounded_idle_connections_are_reaped_and_the_next_request_is_answered() {
    ensure_fd_limit(4096);
    let server = Server::start();
    let before = rss_bytes();
    let opened = Instant::now();
    let (mut idle, refused) = open_idle(&server, 1000);
    let opening = opened.elapsed();
    let held = rss_bytes();
    // Retried every 250 ms: a kernel refusal while the cap is full is immediate, and the
    // contract is recovery within the head deadline.
    let mut attempts = 0;
    let mut first_failure = None;
    let (result, took) = loop {
        attempts += 1;
        let (result, _) = probe(&server);
        if result.is_ok() || opened.elapsed() > READ_TIMEOUT * 2 {
            break (result, opened.elapsed());
        }
        first_failure.get_or_insert(format!("{result:?}"));
        std::thread::sleep(Duration::from_millis(250));
    };
    assert!(
        result.is_ok(),
        "with {} idle connections held ({refused} refused by the kernel), a request must get through within {} s: {result:?} after {attempts} attempts",
        idle.len(),
        (READ_TIMEOUT * 2).as_secs()
    );
    let reaped = closed_within(&mut idle[0], opened, READ_TIMEOUT * 2);
    let after = rss_bytes();
    let again = assert_answers(&server, "after the idle connections' reap window");
    eprintln!(
        "bounded idle connections: {} of 1,000 opened in {} ms ({refused} refused by the kernel), cap {MAX_CONNECTIONS}; next server.info answered {:.1} s after the connections opened, on attempt {attempts}{}; first idle socket {}; rss {} MiB before, {} MiB with them open, {} MiB after; server.info afterwards {} ms",
        idle.len(),
        opening.as_millis(),
        took.as_secs_f64(),
        first_failure.map_or_else(String::new, |f| format!(" (first attempt: {f})")),
        describe_close(reaped, READ_TIMEOUT * 2),
        mib(before),
        mib(held),
        mib(after),
        again.as_millis()
    );
}

/// (d) Headers announcing a body, then silence: answered 408 at the body deadline while the
/// rest of the server carries on.
#[test]
fn bounded_stalled_body_is_cut_off() {
    let server = Server::start();
    let mut stalled = connect(&server);
    stalled.write_all(request_head(4096).as_bytes()).unwrap();
    let stalled_at = Instant::now();
    std::thread::sleep(Duration::from_millis(200));
    let mut probes = Vec::new();
    for i in 0..3 {
        probes.push(assert_answers(
            &server,
            &format!("probe {i} with a stalled body pending"),
        ));
    }
    let reply = read_reply(&mut stalled);
    let cut_after = stalled_at.elapsed();
    assert_eq!(
        http_status(&reply),
        Some(408),
        "a stalled body must be answered 408 (after {} ms): {reply:?}",
        cut_after.as_millis()
    );
    let late = assert_answers(&server, "after the stalled body was cut off");
    eprintln!(
        "bounded stalled body: probes {} ms; 408 after {:.1} s; server.info afterwards {} ms",
        probes
            .iter()
            .map(|p| p.as_millis().to_string())
            .collect::<Vec<_>>()
            .join("/"),
        cut_after.as_secs_f64(),
        late.as_millis()
    );
}

// ---- `RemoteHttpPlugin`, for the record --------------------------------------------------

fn bevy_remote(woken: bool) -> Server {
    Server::start_on(HttpTransport::BevyRemote, woken)
}

/// (a) A 64 MiB `Content-Length` body trickled in 1 MiB pieces and held one byte short of
/// complete: the server buffers all of it; memory is what the test measures.
#[test]
fn bevy_remote_oversized_body_is_buffered_but_others_are_answered() {
    let server = bevy_remote(false);
    let before = rss_bytes();
    let total = 64 * MIB;
    let request = info_request(&server);
    let mut stream = connect(&server);
    stream.write_all(request_head(total).as_bytes()).unwrap();
    stream.write_all(&request).unwrap();
    // Padding is whitespace: the whole body is still one valid JSON-RPC request.
    let chunk = vec![b' '; MIB];
    let mut sent = request.len();
    let trickle = Instant::now();
    while sent + chunk.len() < total {
        stream.write_all(&chunk).unwrap();
        sent += chunk.len();
        std::thread::sleep(Duration::from_millis(5));
    }
    let tail = vec![b' '; total - sent - 1];
    stream.write_all(&tail).unwrap();
    let trickled = trickle.elapsed();
    // Let hyper drain its socket buffer before the measurement.
    std::thread::sleep(Duration::from_millis(200));
    let held = rss_bytes();
    let took = assert_answers(
        &server,
        &format!(
            "with {} MiB of one request buffered (rss {} -> {} MiB)",
            mib((total - 1) as u64),
            mib(before),
            mib(held)
        ),
    );
    stream.write_all(b" ").unwrap();
    let reply = read_reply(&mut stream).and_then(client::unwrap_reply);
    let after = rss_bytes();
    assert!(
        reply.is_ok(),
        "the padded 64 MiB request itself was not answered: {reply:?}"
    );
    eprintln!(
        "bevy_remote oversized body: 64 MiB trickled in {} ms; rss {} MiB before, {} MiB with the body held, {} MiB after the reply; parallel server.info {} ms",
        trickled.as_millis(),
        mib(before),
        mib(held),
        mib(after),
        took.as_millis()
    );
}

/// (b) One HTTP request carrying a 10,000-element batch: `RemoteHttpPlugin` answers the
/// elements one round trip at a time, so a batch costs 10,000 ticks of the server.
#[test]
fn bevy_remote_ten_thousand_element_batch_is_answered_and_does_not_block_others() {
    let server = bevy_remote(true);
    let body = info_batch(&server, 10_000);
    let mut stream = connect(&server);
    stream
        .write_all(request_head(body.len()).as_bytes())
        .unwrap();
    stream.write_all(&body).unwrap();
    let started = Instant::now();
    let batch = std::thread::spawn(move || (read_reply(&mut stream), started.elapsed()));
    // Probe while the batch is in flight: every 250 ms until the batch reader is done.
    let mut probes = Vec::new();
    std::thread::sleep(Duration::from_millis(100));
    while !batch.is_finished() {
        probes.push(assert_answers(
            &server,
            &format!(
                "{} ms into a 10,000-element batch",
                started.elapsed().as_millis()
            ),
        ));
        std::thread::sleep(Duration::from_millis(250));
    }
    let (reply, took) = batch.join().unwrap();
    let replies = reply.unwrap();
    let replies = replies.as_array().unwrap();
    assert_eq!(replies.len(), 10_000, "batch reply count");
    assert!(
        replies.iter().all(|r| r.get("result").is_some()),
        "every element of the batch is answered with a result"
    );
    let worst = probes.iter().max().copied().unwrap_or_default();
    eprintln!(
        "bevy_remote batch: 10,000 x fux/server.info answered in {} ms ({} us per element); {} parallel probes during it, worst {} ms",
        took.as_millis(),
        took.as_micros() / 10_000,
        probes.len(),
        worst.as_millis()
    );
}

/// (c) 1,000 accepted connections that never send a byte, then a 1,001st real request; then
/// whether the idle ones are ever reaped.
#[test]
fn bevy_remote_thousand_idle_connections_do_not_block_the_next_request() {
    ensure_fd_limit(4096);
    let server = bevy_remote(false);
    let before = rss_bytes();
    let opened = Instant::now();
    let (mut idle, refused) = open_idle(&server, 1000);
    let opening = opened.elapsed();
    let held = rss_bytes();
    let took = assert_answers(
        &server,
        &format!(
            "with {} idle connections open ({refused} refused; opened in {} ms, rss {} -> {} MiB)",
            idle.len(),
            opening.as_millis(),
            mib(before),
            mib(held)
        ),
    );
    let last = idle.len() - 1;
    let reaped = closed_within(&mut idle[last], opened, REAP_WINDOW);
    let after = rss_bytes();
    let again = assert_answers(&server, "after the idle connections' reap window");
    eprintln!(
        "bevy_remote idle connections: {} of 1,000 opened in {} ms ({refused} refused); 1,001st server.info {} ms; rss {} MiB before, {} MiB with them open, {} MiB after {} s; last idle socket {}; server.info afterwards {} ms",
        idle.len(),
        opening.as_millis(),
        took.as_millis(),
        mib(before),
        mib(held),
        mib(after),
        REAP_WINDOW.as_secs(),
        describe_close(reaped, REAP_WINDOW),
        again.as_millis()
    );
}

/// (d) Headers announcing a body, then silence: the connection task waits on the body with
/// no deadline while the rest of the server carries on.
#[test]
fn bevy_remote_stalled_body_does_not_stop_the_server() {
    let server = bevy_remote(false);
    let request = info_request(&server);
    let total = request.len() + 4096;
    let mut stalled = connect(&server);
    stalled.write_all(request_head(total).as_bytes()).unwrap();
    let stalled_at = Instant::now();
    std::thread::sleep(Duration::from_millis(200));
    let mut probes = Vec::new();
    for i in 0..3 {
        probes.push(assert_answers(
            &server,
            &format!("probe {i} with a stalled body pending"),
        ));
    }
    let reaped = closed_within(&mut stalled, stalled_at, REAP_WINDOW);
    let late = assert_answers(&server, "after the stalled body's reap window");
    // If the server still holds the connection, finishing the body proves it was waiting.
    let completed = if reaped.is_none() {
        stalled
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stalled.write_all(&request).unwrap();
        stalled.write_all(&vec![b' '; 4096]).unwrap();
        Some(read_reply(&mut stalled).and_then(client::unwrap_reply))
    } else {
        None
    };
    if let Some(reply) = &completed {
        assert!(
            reply.is_ok(),
            "the stalled request, completed after {} s, was not answered: {reply:?}",
            REAP_WINDOW.as_secs()
        );
    }
    eprintln!(
        "bevy_remote stalled body: probes {} ms; the stalled connection {}{}; server.info after {} s: {} ms",
        probes
            .iter()
            .map(|p| p.as_millis().to_string())
            .collect::<Vec<_>>()
            .join("/"),
        describe_close(reaped, REAP_WINDOW),
        if completed.is_some() {
            " and answered once the body arrived"
        } else {
            ""
        },
        REAP_WINDOW.as_secs(),
        late.as_millis()
    );
}
