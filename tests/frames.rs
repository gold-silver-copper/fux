//! Synchronized output (`CSI ? 2026 h` … `l`): a client is shown a frame a
//! program draws in it whole, never half of one, and a frame that never
//! ends is shown once its timeout passes
//! (`references/modern/mode_2026_synchronized_output.md`).
mod support;
use std::time::{Duration, Instant};
use support::*;

/// The shell echoes what it is typed, so the text each frame draws is
/// computed (`$((6*7))` prints as `42`) and the echo never matches it.
fn frame_line(first: &str, mark: &std::path::Path, then: &str) -> String {
    format!(
        "printf '\\033[?2026h%s{first}' $((6*7)); touch '{}'; {then}",
        mark.display()
    )
}

#[test]
fn a_client_is_shown_a_synchronized_frame_only_whole() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    let mark = server.dir.join("mid-frame");
    server.type_line(
        "%1",
        &frame_line("A", &mark, "sleep 0.6; printf '%sB\\033[?2026l' $((6*7))"),
    )?;
    eventually("the first half written", || Ok(mark.exists()))?;
    // Time for the server to read it, were it not held.
    std::thread::sleep(Duration::from_millis(200));
    client.pump()?;
    assert!(
        !client.text().contains("42A"),
        "half a frame was shown:\n{}",
        client.text()
    );
    client.wait("the whole frame", |t| t.contains("42A42B"))?;
    Ok(())
}

#[test]
fn a_frame_that_never_ends_is_shown_after_its_timeout() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    let mark = server.dir.join("begun");
    server.type_line("%1", &frame_line("C", &mark, "sleep 3"))?;
    eventually("the frame begun", || Ok(mark.exists()))?;
    let begun = Instant::now();
    std::thread::sleep(Duration::from_millis(300));
    client.pump()?;
    assert!(!client.text().contains("42C"), "shown before its timeout");
    client.wait("the frame, after its timeout", |t| t.contains("42C"))?;
    let waited = begun.elapsed();
    assert!(
        waited < Duration::from_millis(2500),
        "shown only after {waited:?}, though the timeout is a second"
    );
    Ok(())
}
