//! Colour queries and colour-scheme reports (`src/outer.rs`): the server
//! asks each client's terminal its colours, and answers a pane's program
//! with them; a terminal that answers nothing costs nothing.
mod support;
use support::*;

/// Writes its first argument (with Python's escapes) to the terminal in
/// raw mode, prints `ready`, and reads the answer until it ends with the
/// second argument, or for two seconds; then prints `answer:` and the
/// answer, ESC and BEL spelled out, or `answer:none`.
const PROBE: &str = r#"
import codecs, os, select, sys, termios, time, tty
query = codecs.decode(sys.argv[1], "unicode_escape").encode("latin-1")
until = codecs.decode(sys.argv[2], "unicode_escape").encode("latin-1")
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
tty.setraw(fd)
got = b""
try:
    os.write(1, query + b"ready\r\n")
    end = time.time() + 2
    while time.time() < end and not got.endswith(until):
        ready, _, _ = select.select([fd], [], [], 0.05)
        if ready:
            got += os.read(fd, 256)
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
text = got.decode("latin-1").replace("\x1b", "ESC").replace("\x07", "BEL")
print("answer:" + (text or "none"))
"#;

/// Runs the probe in pane `%1`: it asks `query` and reads until `until`.
fn probe(server: &Server, query: &str, until: &str) -> Outcome {
    let path = server.dir.join("probe.py");
    std::fs::write(&path, PROBE).map_err(e)?;
    server.type_line(
        "%1",
        &format!("clear; python3 {} '{query}' '{until}'", path.display()),
    )
}

/// How Ghostty answers the server's questions at attach: its colours,
/// mode 2031 known and reset, and DA1.
const GHOSTTY_ANSWERS: &[u8] = b"\x1b[?2031;2$y\x1b]10;rgb:ffff/ffff/ffff\x1b\\\
    \x1b]11;rgb:1e1e/1e1e/2020\x1b\\\x1b[?62;22;52c";

fn painted(client: &Client) -> String {
    String::from_utf8_lossy(&client.painted).into_owned()
}

fn wait_painted(client: &mut Client, what: &str, count: usize) -> Outcome {
    eventually(&format!("{count} of {what:?} painted"), || {
        client.pump()?;
        Ok(painted(client).matches(what).count() >= count)
    })
}

#[test]
fn a_panes_colour_query_gets_the_client_terminals_colours() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 100)?;
    client.wait_for("$")?;
    // The server asked: mode 2031, the colours, and DA1 last.
    wait_painted(&mut client, "\x1b[?2031$p\x1b]10;?\x1b\\\x1b]11;?\x1b\\", 1)?;
    wait_painted(&mut client, "\x1b[c", 1)?;
    client.send(GHOSTTY_ANSWERS)?;
    // 2031 is known: reports on, and the scheme asked for.
    wait_painted(&mut client, "\x1b[?2031h\x1b[?996n", 1)?;
    client.send(b"\x1b[?997;1n")?;
    probe(
        &server,
        r"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b[?996n\x1b[c",
        "c",
    )?;
    client.wait_for(
        "answer:ESC]11;rgb:1e1e/1e1e/2020BELESC]10;rgb:ffff/ffff/ffffESC\\ESC[?997;1nESC[?62;22c",
    )?;
    // The answers were never keys: what is typed now arrives as typed.
    client.keys("echo typed-after\r")?;
    client.wait_for("typed-after\n")?;
    Ok(())
}

#[test]
fn a_terminal_that_answers_nothing_leaves_keys_working() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 100)?;
    // Keys typed at once, while the server waits for answers that never
    // come: an Escape among them is still a key (it ends the line).
    client.keys("echo dropped\x1b")?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    client.keys("\x15echo kept\r")?;
    client.wait_for("kept\n")?;
    assert!(!client.text().contains("dropped\n"), "{}", client.text());
    // A pane's question goes unanswered, as before; DA1 after it is
    // answered, by the pane itself.
    probe(&server, r"\x1b]11;?\x07\x1b[c", "c")?;
    client.wait_for("answer:ESC[?62;22c")?;
    // Nothing turned on that the terminal did not say it knows.
    assert!(!painted(&client).contains("\x1b[?2031h"));
    Ok(())
}

/// When the terminal reports a change of scheme, the server asks its
/// colours again, and once they are in, tells each program that set mode
/// 2031; a program that asks then gets the new colours.
#[test]
fn a_change_of_scheme_reaches_the_programs_that_asked_for_it() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 120)?;
    client.wait_for("$")?;
    wait_painted(&mut client, "\x1b[c", 1)?;
    client.send(GHOSTTY_ANSWERS)?;
    wait_painted(&mut client, "\x1b[?996n", 1)?;
    client.send(b"\x1b[?997;1n")?;
    probe(&server, r"\x1b[?2031h", "n")?;
    client.wait_for("ready")?;
    client.send(b"\x1b[?997;2n")?;
    // Asked again: the colours, and DA1.
    wait_painted(&mut client, "\x1b]10;?\x1b\\\x1b]11;?\x1b\\", 2)?;
    wait_painted(&mut client, "\x1b[c", 2)?;
    client.send(b"\x1b]10;rgb:0000/0000/0000\x1b\\\x1b]11;rgb:ffff/ffff/f0f0\x1b\\\x1b[?62c")?;
    client.wait_for("answer:ESC[?997;2n")?;
    probe(&server, r"\x1b]11;?\x1b\\\x1b[?996n", "n")?;
    client.wait_for("answer:ESC]11;rgb:ffff/ffff/f0f0ESC\\ESC[?997;2n")?;
    Ok(())
}

/// Whatever the server turned on in the terminal, the real client turns
/// off as it leaves.
#[test]
fn the_real_client_turns_off_colour_scheme_reports_as_it_leaves() -> Outcome {
    let server = Server::start("")?;
    let mut terminal = Terminal::attach(&server, 12, 60, &[])?;
    terminal.wait_for("%1 sh")?;
    terminal.wait_for("\x1b[?2031$p")?;
    terminal.type_bytes(GHOSTTY_ANSWERS)?;
    terminal.wait_for("\x1b[?2031h")?;
    terminal.type_bytes(b"\x1b[?997;1n")?;
    server.ok(&["detach", "-c", "c1"])?;
    terminal.wait_exit()?;
    let output = String::from_utf8_lossy(&terminal.output).into_owned();
    let on = output.find("\x1b[?2031h").ok_or("never turned on")?;
    let off = output.rfind("\x1b[?2031l").ok_or("never turned off")?;
    let left = output.rfind("\x1b[?1049l").ok_or("never left")?;
    assert!(on < off && off < left, "{output:?}");
    Ok(())
}
