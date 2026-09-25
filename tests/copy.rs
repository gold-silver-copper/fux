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

fn copy_mode(client: &mut Client) -> Outcome {
    client.keys(&format!("{PREFIX}c"))?;
    client.wait("copy mode", |t| {
        t.lines().last().is_some_and(|b| b.contains("COPY"))
    })
}

fn buffer(server: &Server) -> Result<String, String> {
    server.ok(&["show-buffer"])
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
    client.keys("kkk0ve")?;
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
    // Lines: V over two rows.
    copy_mode(&mut client)?;
    client.keys("kkkVj\r")?;
    eventually("two lines", || {
        Ok(buffer(&server)? == "alpha beta gamma\nsecond line here")
    })?;
    // A block: columns 6..=9 of all three rows.
    copy_mode(&mut client)?;
    client.keys("kkk0wwbb")?;
    client.keys("0llllll\x16jjlll")?;
    client.keys("y")?;
    // Columns 6..=9 of "alpha beta gamma", "second line here", "third row x",
    // each line's trailing blanks trimmed.
    eventually("a block", || Ok(buffer(&server)? == "beta\n lin\nrow"))?;
    // One character is one character.
    copy_mode(&mut client)?;
    client.keys("kkk0vy")?;
    client.wait("the singular", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("copied 1 character") && !b.contains("characters"))
    })?;
    server.ok(&["show-buffer", "-b", "1"])?;
    // Buffers keep the newest first.
    let list = server.ok(&["list-buffers"])?;
    assert!(list.starts_with("0: 1 bytes: a"), "{list}");
    assert!(list.contains("1: 13 bytes: beta"), "{list}");
    assert!(list.contains("3: 5 bytes: alpha"), "{list}");
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
    // To the first line's end with $, back a word with b, select to e.
    client.keys("kkk$bvey")?;
    eventually("Four", || Ok(buffer(&server)? == "Four"))?;
    // w stops at punctuation ("two", then "."); W does not.
    copy_mode(&mut client)?;
    client.keys("kkk0wwvy")?;
    eventually("the punctuation", || Ok(buffer(&server)? == "."))?;
    copy_mode(&mut client)?;
    client.keys("kkk0WvEy")?;
    eventually("a WORD", || Ok(buffer(&server)? == "two.three"))?;
    // Search: literal, smart-case, then n and N.
    copy_mode(&mut client)?;
    client.keys("g/line\r")?;
    client.keys("vey")?;
    eventually("the smart-case match", || Ok(buffer(&server)? == "LINE"))?;
    copy_mode(&mut client)?;
    client.keys("g/e\rnnN")?;
    client.keys("vy")?;
    eventually("the second e", || Ok(buffer(&server)? == "e"))?;
    copy_mode(&mut client)?;
    client.keys("/nothing-like-this\r")?;
    client.wait("not found", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("not found: nothing-like-this"))
    })?;
    // ^ is the first non-blank, o swaps the ends, v again clears.
    client.keys("gv")?;
    client.wait("select", |t| {
        t.lines().last().is_some_and(|b| b.contains("COPY select"))
    })?;
    client.keys("v")?;
    client.wait("cleared", |t| {
        t.lines().last().is_some_and(|b| !b.contains("select"))
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
    let server = Server::start("set history-lines 1000")?;
    let mut copier = prepared(&server, 12, 40, &["first", "second"])?;
    let mut watcher = server.attach(12, 40)?;
    watcher.wait_for("second")?;
    copy_mode(&mut copier)?;
    server.ok(&["send-keys", "-t", "%1", "seq 100 140", "Enter"])?;
    // A line that is exactly 140: the echoed command contains "140" too, and
    // would match before any output.
    watcher.wait("the output's end", |t| t.lines().any(|l| l == "140"))?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    copier.pump()?;
    // Forty lines of output would have scrolled "first" away; for the
    // copying client the view is anchored to its rows and does not move.
    let now = copier.lines();
    assert_eq!(now.first().map(String::as_str), Some("first"), "{now:?}");
    assert_eq!(now.get(1).map(String::as_str), Some("second"), "{now:?}");
    assert!(
        !watcher.text().contains("second"),
        "the other client sees it live"
    );
    // G goes to the live bottom; leaving returns to the live screen.
    copier.keys("G")?;
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
    let server = Server::start("set history-lines 5")?;
    let mut client = prepared(&server, 10, 40, &["keep"])?;
    server.ok(&["split", "-h", "-t", "%1"])?;
    server.ok(&["select-pane", "-c", "c1", "-t", "%1"])?;
    copy_mode(&mut client)?;
    // Hold the top of the history, then push it out.
    client.keys("g")?;
    server.ok(&["send-keys", "-t", "%1", "seq 1 200", "Enter"])?;
    client.wait("ended", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("copy mode ended: the history"))
    })?;
    copy_mode(&mut client)?;
    server.ok(&["kill-pane", "-t", "%1"])?;
    client.wait("ended by the close", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("copy mode ended: its pane"))
    })?;
    Ok(())
}

#[test]
fn wide_glyphs_stay_whole_and_wrapped_lines_join() -> Outcome {
    let server = Server::start("")?;
    let long: String = std::iter::repeat_n("abcdefghij", 5).collect();
    let mut client = prepared(&server, 12, 30, &["x界y", &long, "end"])?;
    copy_mode(&mut client)?;
    // The wrapped line takes rows 1 and 2; V over them copies it without a
    // newline.
    client.keys("kkkVj\r")?;
    eventually("the joined line", || Ok(buffer(&server)? == long))?;
    // A wide glyph is one step for the cursor, and copied whole.
    copy_mode(&mut client)?;
    client.keys("kkkk0lvly")?;
    eventually("the wide glyph", || Ok(buffer(&server)? == "界y"))?;
    Ok(())
}

#[test]
fn paste_buffers_paste_and_the_clipboard_can_be_off() -> Outcome {
    let server = Server::start("set clipboard off")?;
    let mut client = prepared(&server, 12, 40, &["hello-paste"])?;
    copy_mode(&mut client)?;
    client.keys("k0v$y")?;
    eventually("copied", || Ok(buffer(&server)? == "hello-paste"))?;
    let painted = String::from_utf8_lossy(&client.painted).into_owned();
    assert!(!painted.contains("\x1b]52"), "the clipboard is off");
    // C-b P pastes the newest buffer into the pane.
    client.keys(&format!("{PREFIX}P"))?;
    client.wait("pasted at the prompt", |t| {
        t.lines().any(|l| l == "$ hello-paste")
    })?;
    // paste-buffer from the CLI, into another pane.
    server.ok(&["split", "-h", "-t", "%1"])?;
    server.ok(&["paste-buffer", "-t", "%2"])?;
    eventually("pasted into %2", || {
        Ok(server
            .ok(&["capture-pane", "-t", "%2"])?
            .contains("hello-paste"))
    })?;
    assert_eq!(
        server.fux(&["paste-buffer", "-b", "5", "-t", "%2"])?.status,
        1
    );
    Ok(())
}
