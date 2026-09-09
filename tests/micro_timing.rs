//! Micro-timings for the integration hot paths. They only measure when `FUX_MICRO_TIMING=1`
//! is set (`FUX_MICRO_TIMING=1 cargo test --release --test micro_timing -- --nocapture`);
//! otherwise each test returns immediately so ordinary suites stay fast.
#![allow(clippy::print_stdout)]
use std::time::Instant;

fn enabled() -> bool {
    std::env::var_os("FUX_MICRO_TIMING").as_deref() == Some(std::ffi::OsStr::new("1"))
}

#[test]
fn event_log_push_timing() {
    if !enabled() {
        return;
    }
    use fux::ecs::events::EventLog;
    use fux::proto::control::Event;
    let mut log = EventLog::new(1);
    let iterations = 200_000u32;
    let started = Instant::now();
    for _ in 0..iterations {
        let _ = log.push(Event::WorkspaceChanged { id: 0 });
    }
    let elapsed = started.elapsed();
    println!(
        "event_log_push: {:.1} ns/push ({} pushes, {:?})",
        elapsed.as_nanos() as f64 / f64::from(iterations),
        iterations,
        elapsed
    );
}

#[test]
fn terminal_process_timing() {
    if !enabled() {
        return;
    }
    use fux::terminal::ServerTerminal;
    let mut terminal = ServerTerminal::new(24, 80, 1000);
    let line = "\u{1b}[1;31mstyled 日本語 text with wide glyphs and 0123456789 \u{1b}[0m\r\n";
    let chunk: Vec<u8> = line.repeat(200).into_bytes();
    let rounds = 200;
    let started = Instant::now();
    for _ in 0..rounds {
        terminal.process(&chunk);
    }
    let elapsed = started.elapsed();
    let bytes = chunk.len() * rounds;
    println!(
        "terminal_process: {:.2} ns/byte ({} bytes, {:?})",
        elapsed.as_nanos() as f64 / bytes as f64,
        bytes,
        elapsed
    );
    let started = Instant::now();
    for _ in 0..rounds {
        for piece in chunk.chunks(7) {
            terminal.process(piece);
        }
    }
    let elapsed = started.elapsed();
    println!(
        "terminal_process split7: {:.2} ns/byte ({:?})",
        elapsed.as_nanos() as f64 / bytes as f64,
        elapsed
    );
}
