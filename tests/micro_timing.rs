//! Micro-timings for the integration hot paths. They only measure when `FUX_MICRO_TIMING=1`
//! is set (`FUX_MICRO_TIMING=1 cargo test --release --test micro_timing -- --nocapture`);
//! otherwise each test returns immediately so ordinary suites stay fast.
#![allow(clippy::print_stdout)]
#![allow(
    clippy::expect_used,
    reason = "timing setup failures abort the measurement"
)]
use std::time::Instant;

fn enabled() -> bool {
    std::env::var_os("FUX_MICRO_TIMING").as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// `FUX_MICRO_ROUNDS` scales every loop so a profiler can sample a longer run.
fn rounds(base: usize) -> usize {
    std::env::var("FUX_MICRO_ROUNDS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(base, |factor| base.saturating_mul(factor.max(1)))
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

#[test]
fn grid_refresh_timing() {
    if !enabled() {
        return;
    }
    use fux::terminal::ServerTerminal;
    for (rows, cols, fill) in [
        (24u16, 80u16, 79usize),
        (24, 80, 20),
        (60, 200, 199),
        (60, 200, 40),
    ] {
        let mut terminal = ServerTerminal::new(rows, cols, 1000);
        let filler = "x".repeat(fill);
        for _ in 0..rows {
            terminal.process(format!("{filler}\r\n").as_bytes());
        }
        terminal.refresh_grid("", None);
        let rounds = rounds(2000);
        let started = Instant::now();
        let mut changed = 0;
        for _ in 0..rounds {
            changed += usize::from(terminal.refresh_grid("", None));
        }
        let unchanged = started.elapsed();
        assert_eq!(changed, 0);
        let started = Instant::now();
        for round in 0..rounds {
            terminal.process(format!("\u{1b}[1;1H{round:08}").as_bytes());
            changed += usize::from(terminal.refresh_grid("", None));
        }
        let one_row = started.elapsed();
        assert_eq!(changed, rounds);
        println!(
            "grid_refresh {rows}x{cols} fill {fill}: unchanged {:.2} us, one changed row {:.2} us (per refresh)",
            unchanged.as_secs_f64() * 1e6 / rounds as f64,
            one_row.as_secs_f64() * 1e6 / rounds as f64
        );
    }
}

#[test]
fn ecs_step_timing() {
    if !enabled() {
        return;
    }
    use fux::config::Config;
    use fux::ecs::messages::{Effect, Inbound, ManagerAction, ViewerRequest};
    use fux::ecs::{ServerIdentity, Session};
    use fux::ids::ViewerId;
    let config = Config::from_toml("default-command = { argv = [\"/bin/sh\"] }").expect("config");
    let mut session = Session::new(&config).expect("session");
    session.set_identity(ServerIdentity {
        instance_nonce: "micro".into(),
        ..Default::default()
    });
    let mut now = 1_000u64;
    let mut step = |session: &mut Session, inbound: Vec<Inbound>| {
        now += 10;
        session.step(now, inbound)
    };
    let effects = step(
        &mut session,
        vec![Inbound::Manager {
            action: ManagerAction::Resolve {
                name: Some("micro".into()),
            },
            token: 1,
        }],
    );
    let pane = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::SpawnPane { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("pane spawn");
    step(
        &mut session,
        vec![Inbound::SpawnCompleted {
            pane,
            result: Ok(4242),
        }],
    );
    let viewer = ViewerId(1);
    step(
        &mut session,
        vec![Inbound::ViewerAttached {
            viewer,
            workspace: "micro".into(),
            rows: 24,
            cols: 80,
        }],
    );
    let steps = u32::try_from(rounds(5_000)).expect("steps");
    let started = Instant::now();
    let mut frames = 0usize;
    for index in 0..steps {
        let effects = step(
            &mut session,
            vec![Inbound::ViewerRequest {
                viewer,
                request: ViewerRequest::Input(b"a".to_vec()),
            }],
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::WriteInput { .. }))
        );
        let effects = step(
            &mut session,
            vec![Inbound::PaneOutput {
                pane,
                bytes: if index % 40 == 39 {
                    b"a\r\n".to_vec()
                } else {
                    b"a".to_vec()
                },
            }],
        );
        frames += effects
            .iter()
            .filter(|effect| matches!(effect, Effect::ToViewer { .. }))
            .count();
    }
    let elapsed = started.elapsed();
    assert!(
        frames >= usize::try_from(steps).expect("count"),
        "frames {frames}"
    );
    println!(
        "ecs_step: {:.2} us per keystroke (input step + output step, {} frames, {:?})",
        elapsed.as_secs_f64() * 1e6 / f64::from(steps),
        frames,
        elapsed
    );
}
