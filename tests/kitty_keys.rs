//! The kitty keyboard protocol between fux and a client's terminal, and
//! between fux and a pane's program: the real client on a PTY, answering as
//! Ghostty does, sends keys in the protocol's forms; each pane gets them as
//! its program asked (`src/encode.rs`).
mod support;
use support::*;

/// Pushes the kitty flags its second argument gives, if any, in raw mode,
/// prints `ready` and its first argument, reads what is typed until it has
/// been quiet for a while, pops the flags and prints `got:` and the bytes
/// in hex.
const PROBE: &str = r#"
import os, select, sys, termios, time, tty
tag = sys.argv[1]
flags = sys.argv[2] if len(sys.argv) > 2 else ""
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
tty.setraw(fd)
got = b""
try:
    if flags:
        os.write(1, ("\x1b[>" + flags + "u").encode())
    os.write(1, ("ready" + tag + "\r\n").encode())
    end = time.time() + 5
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.3 if got else 0.05)
        if ready:
            got += os.read(fd, 256)
        elif got:
            break
    if flags:
        os.write(1, b"\x1b[<u")
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
print("got:" + got.hex())
"#;

/// How Ghostty answers the server's questions at attach: mode 2031 known,
/// its colours, its kitty flags (none pushed yet), and DA1.
const GHOSTTY_ANSWERS: &[u8] = b"\x1b[?2031;2$y\x1b]10;rgb:ffff/ffff/ffff\x1b\\\
    \x1b]11;rgb:1e1e/1e1e/2020\x1b\\\x1b[?0u\x1b[?62;22;52c";

/// The real client attached on a PTY that answers as Ghostty does; once the
/// server has pushed the kitty flags it reads keys in the protocol's forms.
fn ghostty(server: &Server) -> Result<Terminal, String> {
    let mut terminal = Terminal::attach(server, 12, 100, &[])?;
    terminal.wait_for("%1 sh")?;
    terminal.wait_for("\x1b[?u\x1b[c")?;
    terminal.type_bytes(GHOSTTY_ANSWERS)?;
    // Disambiguate and alternate keys.
    terminal.wait_for("\x1b[>5u")?;
    Ok(terminal)
}

/// Runs the probe in pane `%1`, pushing `flags` (if not empty), and types
/// `keys` once it is ready; it prints the hex of what it read.
fn probe(server: &Server, terminal: &mut Terminal, flags: &str, keys: &[u8]) -> Outcome {
    let path = server.dir.join("probe.py");
    std::fs::write(&path, PROBE).map_err(e)?;
    let tag = format!("-{}-{flags}", keys.len());
    server.type_line(
        "%1",
        &format!("clear; python3 {} {tag} {flags}", path.display()),
    )?;
    terminal.wait_for(&format!("ready{tag}"))?;
    terminal.type_bytes(keys)
}

/// Shift-Enter and Ctrl-I, as Ghostty sends them with the flags fux
/// pushed, reach a pane whose program pushed disambiguate as the kitty spec
/// says (`CSI 13 ; 2 u`, `CSI 105 ; 5 u`), and a legacy pane as Enter and Tab,
/// the bytes it got before. Escape is a key at once.
#[test]
fn keys_reach_each_pane_as_its_program_asked() -> Outcome {
    let server = Server::start("")?;
    let mut terminal = ghostty(&server)?;
    let keys = b"\x1b[13;2u\x1b[105;5u\x1b[27u";
    probe(&server, &mut terminal, "", keys)?;
    terminal.wait_for("got:0d091b")?;
    probe(&server, &mut terminal, "1", keys)?;
    terminal.wait_for("got:1b5b31333b32751b5b3130353b35751b5b323775")?;
    // Report all keys and alternate keys (13): text keys too, with their
    // shifted keys.
    probe(&server, &mut terminal, "13", b"aA\r")?;
    terminal.wait_for("got:1b5b3937751b5b39373a36353b32751b5b313375")?;
    Ok(())
}

/// The prefix and a binding work as Ghostty sends them in the protocol
/// (`CSI 98 ; 5 u` is Ctrl-B), and the client pops the flags as it leaves,
/// on the alternate screen, before leaving it.
#[test]
fn the_prefix_works_with_the_protocol_and_the_flags_are_popped_on_leaving() -> Outcome {
    let server = Server::start("")?;
    let mut terminal = ghostty(&server)?;
    terminal.type_bytes(b"echo typed\r")?;
    terminal.wait_for("typed")?;
    terminal.type_bytes(b"\x1b[98;5ud")?;
    let status = terminal.wait_exit()?;
    assert!(status.success(), "{status}");
    let output = String::from_utf8_lossy(&terminal.output).into_owned();
    assert!(output.contains("[detached]"), "{output:?}");
    let pushed = output.find("\x1b[>5u").ok_or("never pushed")?;
    let popped = output.rfind("\x1b[<u").ok_or("never popped")?;
    let left = output.rfind("\x1b[?1049l").ok_or("never left")?;
    assert!(pushed < popped && popped < left, "{output:?}");
    Ok(())
}

/// A terminal that does not answer `CSI ? u` gets nothing pushed, and its
/// keys work as before.
#[test]
fn a_terminal_without_the_protocol_gets_nothing_pushed() -> Outcome {
    let server = Server::start("")?;
    let mut terminal = Terminal::attach(&server, 12, 100, &[])?;
    terminal.wait_for("%1 sh")?;
    terminal.wait_for("\x1b[?u\x1b[c")?;
    terminal.type_bytes(b"\x1b[?62;22c")?;
    probe(&server, &mut terminal, "1", b"\r\t")?;
    // A pane that pushed disambiguate still gets Enter and Tab as they are.
    terminal.wait_for("got:0d09")?;
    assert!(!String::from_utf8_lossy(&terminal.output).contains("\x1b[>5u"));
    Ok(())
}
