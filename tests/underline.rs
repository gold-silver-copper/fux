//! Underline styles (`src/outer.rs`, `STYLES`): the server asks each
//! client's terminal whether it draws them, and paints a pane's curly
//! underline to it as `4:3` if it does, as a plain underline if it does not.
//! A pane's program learns that fux keeps them as neovim asks (DECRQSS).
mod support;
use support::*;

/// How Ghostty answers the questions about styles (Ghostty at 7aa95917,
/// `src/termio/stream_handler.zig`): XTGETTCAP from its terminfo, which has
/// `Smulx` (hex `536d756c78`, its value `\E[4:%p1%dm` hex too), and DECRQSS
/// with a plain 4 for any underline (`Terminal.printAttributes`); then DA1.
const GHOSTTY: &[u8] = b"\x1bP1+r536d756c78=5c455b343a25703125646d\x1b\\\
    \x1bP1$r0;4m\x1b\\\x1b[?62;22;52c";

/// How xterm answers them (xterm 411, misc.c `do_dcs`): no `Smulx` in its
/// terminfo, and `4:3` dropped, so the pen it reports is plain; then DA1.
const XTERM: &[u8] = b"\x1bP0+r536d756c78\x1b\\\x1bP1$r0m\x1b\\\x1b[?64;1;2c";

/// What a program in the pane prints: a word curly underlined in red, as
/// neovim draws a diagnostic.
const CURLY: &str = "printf '\\033[4:3;58:2::255:0:0mcur''ly\\033[0m\\n'\r";

#[test]
fn a_terminal_that_draws_styles_is_painted_curly_underlines() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 100)?;
    client.wait_for("$")?;
    // The server asked: XTGETTCAP for Smulx, and the pen after a curly
    // underline, before DA1, the pen reset after.
    wait_painted(
        &mut client,
        "\x1bP+q536d756c78\x1b\\\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\\x1b[0m\x1b[?u\x1b[c",
        1,
    )?;
    client.send(GHOSTTY)?;
    // Learning it repaints the screen whole.
    wait_painted(&mut client, "\x1b[2J", 2)?;
    client.keys(CURLY)?;
    wait_painted(&mut client, "\x1b[0;4:3;58:2::255:0:0mcurly", 1)?;
    // The answers were never keys.
    client.keys("echo typed-after\r")?;
    client.wait_for("typed-after\n")?;
    Ok(())
}

#[test]
fn a_terminal_that_draws_no_styles_is_painted_plain_underlines() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 100)?;
    client.wait_for("$")?;
    wait_painted(&mut client, "\x1b[c", 1)?;
    client.send(XTERM)?;
    client.keys(CURLY)?;
    wait_painted(&mut client, "\x1b[0;4;58:2::255:0:0mcurly", 1)?;
    assert!(
        !painted(&client).contains("\x1b[0;4:3"),
        "{:?}",
        painted(&client)
    );
    // A terminal that answers nothing at all is painted plain too.
    let mut quiet = server.attach(10, 100)?;
    quiet.keys(CURLY)?;
    wait_painted(&mut quiet, "\x1b[0;4;58:2::255:0:0mcurly", 1)?;
    assert!(!painted(&quiet).contains("\x1b[0;4:3"));
    Ok(())
}

/// neovim's handshake (neovim 0.12.5, `tui_query_extended_underline` and
/// `handle_term_response`): a curly underline set, then DECRQSS of the
/// pen; it draws diagnostics curly only on `DCS 1 $ r 0 ; 4:3 m ST`. The
/// pane answers so, whatever the client's terminal is.
#[test]
fn a_pane_answers_neovims_question_about_styles() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 100)?;
    client.wait_for("$")?;
    let path = server.dir.join("ask.py");
    std::fs::write(&path, ASK).map_err(e)?;
    server.type_line("%1", &format!("clear; python3 {}", path.display()))?;
    client.wait_for("answer:ESCP1$r0;4:3mESC\\")?;
    Ok(())
}

/// Asks the pen as neovim does, in raw mode, and prints the answer, ESC
/// spelled out, once ST has come, or after two seconds.
const ASK: &str = r#"
import os, select, sys, termios, time, tty
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
tty.setraw(fd)
got = b""
try:
    os.write(1, b"\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\\x1b[0m")
    end = time.time() + 2
    while time.time() < end and not got.endswith(b"\x1b\\"):
        ready, _, _ = select.select([fd], [], [], 0.05)
        if ready:
            got += os.read(fd, 256)
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
print("answer:" + (got.decode("latin-1").replace("\x1b", "ESC") or "none"))
"#;
