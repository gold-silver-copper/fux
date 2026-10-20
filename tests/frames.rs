//! Synchronized output (`CSI ? 2026 h` … `l`): a client is shown a frame a
//! program draws in it whole, never half of one, and a frame that never
//! ends is shown once its timeout passes
//! (`references/modern/mode_2026_synchronized_output.md`).
mod support;
use std::time::{Duration, Instant};
use support::*;

/// Types a line into a pane once its shell shows a prompt, answering at
/// once (`0 timeout`): what it starts keeps running, so it never settles.
fn type_now(server: &Server, pane: &str, line: &str) -> Outcome {
    eventually(&format!("a prompt in {pane}"), || {
        Ok(server.ok(&["str", pane, "pane", "screen"])?.contains('$'))
    })?;
    server.ok(&[
        "str",
        pane,
        "pane",
        "str",
        line,
        "text",
        "str",
        "Enter",
        "key",
        "0",
        "timeout",
        "send-keys",
    ])?;
    Ok(())
}

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
    type_now(
        &server,
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
    type_now(&server, "%1", &frame_line("C", &mark, "sleep 3"))?;
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

/// A frame whose reading takes several turns, as one spending more than a
/// parser call's work budget does (THREATS.md 4.4), is shown only once
/// read whole: the client keeps the last frame shown meanwhile ("keep
/// rendering the last rendered state", the spec's "Rendering"). Each REP
/// is spent as 65,535 copies: 4,000 of them spend two budgets. A row of
/// eight X's is the frame half read: the line typed has none.
#[test]
fn a_frame_read_over_several_turns_is_shown_only_whole() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    type_now(
        &server,
        "%1",
        "printf '\\033[?2026h'; printf 'X\\033[65535b%.0s' $(seq 4000); \
         printf '\\033[2J\\033[HDO%sNE\\033[?2026l' ''",
    )?;
    let deadline = after(Duration::from_secs(60));
    loop {
        client.pump()?;
        let text = client.text();
        assert!(
            !text.contains("XXXXXXXX"),
            "half a frame was shown:\n{text}"
        );
        if text.contains("DONE") {
            return Ok(());
        }
        assert!(Instant::now() < deadline, "no frame shown:\n{text}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// A pane drawing frame after frame, each read over several turns, holds
/// up only itself (THREATS.md 4.4): the client is painted the rest of its
/// screen meanwhile, that pane as it was last painted, never half a frame.
#[test]
fn a_pane_part_way_through_frames_holds_up_no_other() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    server.ok(&["str", "%1", "pane", "vertical", "split"])?;
    type_now(
        &server,
        "%2",
        "sleep 2; while :; do printf '\\033[?2026h'; printf 'X\\033[65535b%.0s' $(seq 4000); \
         printf '\\033[2J\\033[HDO%sNE\\033[?2026l' ''; done",
    )?;
    // Both lines are typed before the frames begin: in an unoptimized
    // build a turn takes seconds, past a new client's patience for the
    // server's greeting. HELLO comes once they have.
    type_now(&server, "%1", "sleep 4; echo HE''LLO")?;
    // How long a frame of the flood takes in this build (two budgets and
    // more: seconds unoptimized, on a slow runner more) sets the wait. The
    // flood never ends, so a pane it starved would show HELLO never; one
    // painted between the flood's turns shows it within a few frames of
    // its echo, which comes once the first frame does if not before.
    let started = Instant::now();
    let first = after(Duration::from_secs(300));
    let mut deadline = None;
    loop {
        client.pump()?;
        let text = client.text();
        assert!(
            !text.contains("XXXXXXXX"),
            "half a frame was shown:\n{text}"
        );
        if text.contains("HELLO") {
            return Ok(());
        }
        if deadline.is_none() && text.contains("DONE") {
            let frame = started.elapsed();
            deadline = Some(after(Duration::from_secs(4) + frame * 5));
        }
        let by = deadline.unwrap_or(first);
        assert!(
            Instant::now() < by,
            "the other pane was not painted:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}
