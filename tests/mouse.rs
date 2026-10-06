//! The mouse: passed through to a program that asked for it, and to no
//! other; fux has no mouse actions of its own.
mod support;
use fux::render::MOUSE_OFF;
use support::*;

fn painted(client: &Client, from: usize) -> String {
    String::from_utf8_lossy(client.painted.get(from..).unwrap_or_default()).into_owned()
}

/// Waits until the paints since `from` hold `needle`.
fn painted_eventually(client: &mut Client, from: usize, needle: &str) -> Outcome {
    eventually(&format!("{needle:?} painted"), || {
        client.pump()?;
        Ok(painted(client, from).contains(needle))
    })
}

/// The client's terminal is asked for mouse reports only while the focused
/// pane's program wants them, and a report reaches that program, in its
/// pane's cells; copy mode takes the reports away until it ends.
#[test]
fn the_mouse_reaches_a_program_that_asked_for_it_and_no_other() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    // From nothing, the first paint says reporting is off.
    assert!(painted(&client, 0).contains(MOUSE_OFF));
    assert!(!painted(&client, 0).contains("\x1b[?1000h"));
    client.keys("printf '\\033[?1002h\\033[?1006h'; echo asked-for-mouse; cat -v\r")?;
    client.wait("mouse reporting on", |t| {
        t.lines().any(|l| l == "asked-for-mouse")
    })?;
    painted_eventually(&mut client, 0, "\x1b[?1002h\x1b[?1006h")?;
    // A press and release at row 3, column 5: the pane starts at the top
    // left, so its cells are the screen's.
    client.send(b"\x1b[<0;5;3M\x1b[<0;5;3m")?;
    client.keys("\r")?;
    client.wait("the report", |t| t.contains("^[[<0;5;3M^[[<0;5;3m"))?;
    // Copy mode has the keys: reporting goes off, and comes back after it.
    let before = client.painted.len();
    client.keys("\x02c")?;
    painted_eventually(&mut client, before, MOUSE_OFF)?;
    let before = client.painted.len();
    client.keys("q")?;
    painted_eventually(&mut client, before, "\x1b[?1002h\x1b[?1006h")?;
    // The program turns it off: so does the client's terminal.
    let before = client.painted.len();
    client.keys("\x03")?;
    // Typing before the prompt is back would race the interrupt's flush.
    client.wait("the prompt again", |t| {
        t.lines()
            .rev()
            .skip(1)
            .find(|l| !l.is_empty())
            .is_some_and(|l| l == "$")
    })?;
    client.keys("printf '\\033[?1002l'; echo mouse-off\r")?;
    client.wait("mouse reporting off", |t| {
        t.lines().any(|l| l == "mouse-off")
    })?;
    painted_eventually(&mut client, before, MOUSE_OFF)?;
    Ok(())
}
