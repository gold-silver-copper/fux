//! Milestone 8: copy/select mode (motions, search, character, line and
//! block selection), paste buffers, and OSC 52.
mod support;
use support::*;

const PREFIX: &str = "\x02";

/// A client whose pane shows `lines` at its top, the prompt after them.
fn prepared(server: &Server, rows: u16, cols: u16, lines: &[&str]) -> Result<Client, String> {
    let mut client = server.attach(rows, cols)?;
    client.wait_for("$")?;
    let printf = lines.iter().map(|l| format!("{l}\\n")).collect::<String>();
    client.keys(&format!("printf '\\033[H\\033[2J{printf}'\r"))?;
    let last = lines.last().copied().unwrap_or_default().to_owned();
    client.wait("the text", move |t| {
        t.lines().any(|l| l == last) && !t.contains("printf")
    })?;
    Ok(client)
}

/// Waits until the client is out of copy mode: its keys go to the pane
/// again only once it has been told so.
fn left(client: &mut Client) -> Outcome {
    client.wait("copy mode to end", |t| {
        t.lines().last().is_some_and(|b| !b.contains("COPY"))
    })
}

fn copy_mode(client: &mut Client) -> Outcome {
    left(client)?;
    client.keys(&format!("{PREFIX}c"))?;
    client.wait("copy mode", |t| {
        t.lines().last().is_some_and(|b| b.contains("COPY"))
    })
}

/// Types `keys` that open a search, then `term` once the search is open:
/// keys typed before the client knows of it would be motions.
fn search(client: &mut Client, keys: &str, term: &str) -> Outcome {
    client.keys(keys)?;
    client.wait("the search", |t| {
        t.lines().last().is_some_and(|b| b.contains("Enter accept"))
    })?;
    client.keys(&format!("{term}\r"))?;
    client.wait("the search to end", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("COPY") && !b.contains("Enter accept"))
    })
}

fn buffer(server: &Server) -> Result<String, String> {
    server
        .ok(&["newest", "buffer"])
        .map(|text| text.strip_suffix('\n').unwrap_or(&text).to_owned())
}

#[test]
fn copy_errors_do_not_name_default_selection_keys() -> Outcome {
    let server = Server::start(
        "str copy str v unbind-in str copy str s unbind-in str copy str x unbind-in str copy str V str select-char bind-in str copy str Y str yank bind-in",
    )?;
    let mut client = prepared(&server, 12, 100, &["sample"])?;
    copy_mode(&mut client)?;
    client.keys("Y")?;
    client.wait_for("nothing selected: start a selection before copying")
}

#[test]
fn a_word_a_line_and_a_block_are_copied() -> Outcome {
    let server = Server::start("")?;
    let mut client = prepared(
        &server,
        12,
        40,
        &["alpha beta gamma", "second line here", "third row x"],
    )?;
    // The cursor starts at the prompt, below the three lines.
    copy_mode(&mut client)?;
    client.keys("kkk\x1b[Hvllll")?;
    client.wait("a selection", |t| {
        t.lines().last().is_some_and(|b| b.contains("COPY select"))
    })?;
    client.keys("y")?;
    client.wait("copied", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("copied 5 characters"))
    })?;
    assert_eq!(buffer(&server)?, "alpha");
    // It went to the client's clipboard as OSC 52.
    let painted = String::from_utf8_lossy(&client.painted).into_owned();
    assert!(
        painted.contains("\x1b]52;c;YWxwaGE=\x07"),
        "no OSC 52 for alpha"
    );
    // Lines: s over two rows.
    copy_mode(&mut client)?;
    client.keys("kkksj\r")?;
    eventually("two lines", || {
        Ok(buffer(&server)? == "alpha beta gamma\nsecond line here")
    })?;
    // A block: columns 6..=9 of all three rows.
    copy_mode(&mut client)?;
    client.keys("kkk\x1b[Hwwbb")?;
    client.keys("\x1b[Hllllllxjjlll")?;
    client.keys("y")?;
    // Columns 6..=9 of "alpha beta gamma", "second line here", "third row x",
    // each line's trailing blanks trimmed.
    eventually("a block", || Ok(buffer(&server)? == "beta\n lin\nrow"))?;
    // One character is one character.
    copy_mode(&mut client)?;
    client.keys("kkk\x1b[Hvy")?;
    client.wait("the singular", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("copied 1 character") && !b.contains("characters"))
    })?;
    // Buffers keep metadata newest first, with stable ids.
    let list = server.ok(&["buffers", "json"])?;
    assert!(list.starts_with("[{\"id\":\"b4:"), "{list}");
    assert!(list.contains("\"bytes\":1,\"preview\":\"a\""), "{list}");
    assert!(
        list.contains("\"bytes\":13,\"preview\":\"beta  lin row\""),
        "{list}"
    );
    assert!(list.contains("\"bytes\":5,\"preview\":\"alpha\""), "{list}");
    Ok(())
}

#[test]
fn motions_and_search_move_the_cursor() -> Outcome {
    let server = Server::start("")?;
    let mut client = prepared(
        &server,
        12,
        50,
        &["one two.three  Four", "second LINE here", "last"],
    )?;
    copy_mode(&mut client)?;
    // To the first line's end with e, back a word with b, select to e.
    client.keys("kkkebvey")?;
    eventually("Four", || Ok(buffer(&server)? == "Four"))?;
    // w stops at punctuation ("two", then ".").
    copy_mode(&mut client)?;
    client.keys("kkk\x1b[Hwwvy")?;
    eventually("the punctuation", || Ok(buffer(&server)? == "."))?;
    // Search: f forward, literal and exact, then n and p.
    copy_mode(&mut client)?;
    search(&mut client, "tf", "LINE")?;
    client.keys("vllly")?;
    eventually("the exact match", || Ok(buffer(&server)? == "LINE"))?;
    copy_mode(&mut client)?;
    search(&mut client, "tf", "e")?;
    client.keys("nnp")?;
    client.keys("vy")?;
    eventually("the second e", || Ok(buffer(&server)? == "e"))?;
    copy_mode(&mut client)?;
    search(&mut client, "f", "nothing-like-this")?;
    client.wait("not found", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("not found: nothing-like-this"))
    })?;
    // v selects, and v again clears.
    client.keys("tv")?;
    client.wait("select", |t| {
        t.lines().last().is_some_and(|b| b.contains("COPY select"))
    })?;
    client.keys("v")?;
    client.wait("cleared", |t| {
        t.lines().last().is_some_and(|b| !b.contains("COPY select"))
    })?;
    client.keys("q")?;
    client.wait("left", |t| {
        t.lines().last().is_some_and(|b| !b.contains("COPY"))
    })?;
    // y with nothing selected says so and stays.
    copy_mode(&mut client)?;
    client.keys("y")?;
    client.wait("the hint", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("nothing selected"))
    })?;
    client.keys("\x1b")?;
    client.wait("left by Escape", |t| {
        t.lines().last().is_some_and(|b| !b.contains("COPY"))
    })?;
    Ok(())
}

#[test]
fn the_pane_holds_still_for_the_copying_client_only() -> Outcome {
    let server = Server::start("str history-bytes 100000 set")?;
    let mut copier = prepared(&server, 12, 40, &["first", "second"])?;
    let mut watcher = server.attach(12, 40)?;
    watcher.wait_for("second")?;
    copy_mode(&mut copier)?;
    server.type_line("%1", "seq 100 140")?;
    // A line that is exactly 140: the echoed command contains "140" too, and
    // would match before any output.
    watcher.wait("the output's end", |t| t.lines().any(|l| l == "140"))?;
    // A key of the copier's own, after the output: once its effect shows,
    // the copier's screen is drawn from after the output too.
    let bar = copier.bar();
    copier.keys("j")?;
    copier.wait("the cursor to move", |t| {
        t.lines().last().is_some_and(|b| b != bar)
    })?;
    // Forty lines of output would have scrolled "first" away; for the
    // copying client the view is anchored to its rows and does not move.
    let now = copier.lines();
    assert_eq!(now.first().map(String::as_str), Some("first"), "{now:?}");
    assert_eq!(now.get(1).map(String::as_str), Some("second"), "{now:?}");
    assert!(
        !watcher.text().contains("second"),
        "the other client sees it live"
    );
    // z goes to the live bottom; leaving returns to the live screen.
    copier.keys("z")?;
    copier.wait("the live bottom", |t| t.lines().any(|l| l == "140"))?;
    copier.keys("q")?;
    copier.wait("live", |t| {
        t.contains("140") && t.lines().last().is_some_and(|b| !b.contains("COPY"))
    })?;
    // Paging back from the live screen reaches the history.
    copy_mode(&mut copier)?;
    copier.keys("\x1b[5~\x1b[5~\x1b[5~")?;
    copier.wait_for("second")?;
    Ok(())
}

#[test]
fn copy_mode_ends_when_its_pane_closes_or_its_rows_are_evicted() -> Outcome {
    let server = Server::start("str history-bytes 1000 set")?;
    let mut client = prepared(&server, 10, 40, &["keep"])?;
    server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
    server.ok(&["str", "c1", "client", "str", "%1", "pane", "focus-pane"])?;
    copy_mode(&mut client)?;
    // Hold the top of the history, then push it out.
    client.keys("t")?;
    server.type_line("%1", "seq 1 200")?;
    client.wait("ended", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("copy mode ended: the history"))
    })?;
    copy_mode(&mut client)?;
    server.ok(&["str", "%1", "pane", "kill"])?;
    client.wait("ended by the close", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("copy mode ended: its pane"))
    })?;
    Ok(())
}

#[test]
fn paste_buffers_paste_and_the_clipboard_can_be_off() -> Outcome {
    let server = Server::start("str terminal-clipboard false set")?;
    let mut client = prepared(&server, 12, 40, &["hello-paste"])?;
    copy_mode(&mut client)?;
    client.keys("k\x1b[Hvey")?;
    eventually("copied", || Ok(buffer(&server)? == "hello-paste"))?;
    let painted = String::from_utf8_lossy(&client.painted).into_owned();
    assert!(!painted.contains("\x1b]52"), "the clipboard is off");
    // C-b p pastes the newest buffer into the pane.
    left(&mut client)?;
    client.keys(&format!("{PREFIX}p"))?;
    client.wait("pasted at the prompt", |t| {
        t.lines().any(|l| l == "$ hello-paste")
    })?;
    // paste-buffer from the CLI, into another pane.
    server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
    let metadata = server.ok(&["buffers", "json"])?;
    let id = metadata
        .strip_prefix("[{\"id\":\"")
        .and_then(|s| s.split('"').next())
        .ok_or("no buffer id")?;
    server.ok(&["str", "%2", "pane", "str", id, "paste-buffer"])?;
    eventually("pasted into %2", || {
        Ok(server
            .ok(&["str", "%2", "pane", "capture"])?
            .contains("hello-paste"))
    })?;
    assert_eq!(
        server
            .fux(&["str", "%2", "pane", "str", "b1:0-0", "paste-buffer"])?
            .status,
        1
    );
    Ok(())
}
