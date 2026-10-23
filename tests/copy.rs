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
        t.lines().last().is_some_and(|b| b.contains("Enter search"))
    })?;
    client.keys(&format!("{term}\r"))?;
    client.wait("the search to end", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("COPY") && !b.contains("Enter search"))
    })
}

fn buffer(server: &Server) -> Result<String, String> {
    server
        .ok(&["0", "buffer"])
        .map(|text| text.strip_suffix('\n').unwrap_or(&text).to_owned())
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
    server.ok(&["1", "buffer"])?;
    // Buffers keep the newest first.
    let list = server.ok(&["buffers"])?;
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
    // To the first line's end with e, back a word with b, select to e.
    client.keys("kkkebvey")?;
    eventually("Four", || Ok(buffer(&server)? == "Four"))?;
    // w stops at punctuation ("two", then ".").
    copy_mode(&mut client)?;
    client.keys("kkk\x1b[Hwwvy")?;
    eventually("the punctuation", || Ok(buffer(&server)? == "."))?;
    // Keys are letters in either case.
    copy_mode(&mut client)?;
    client.keys("KKKEBVEY")?;
    eventually("in capitals", || Ok(buffer(&server)? == "Four"))?;
    // Search: f forward, literal, smart-case, then n and p.
    copy_mode(&mut client)?;
    search(&mut client, "tf", "line")?;
    client.keys("vllly")?;
    eventually("the smart-case match", || Ok(buffer(&server)? == "LINE"))?;
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
    let server = Server::start("str clipboard str off set")?;
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
    server.ok(&["str", "%2", "pane", "0", "paste-buffer"])?;
    eventually("pasted into %2", || {
        Ok(server
            .ok(&["str", "%2", "pane", "capture"])?
            .contains("hello-paste"))
    })?;
    assert_eq!(
        server
            .fux(&["str", "%2", "pane", "5", "paste-buffer"])?
            .status,
        1
    );
    Ok(())
}

/// An editor for `edit-history` that keeps a copy of the file it is given,
/// its mode, its arguments and its path, in `seen` and beside it, and exits
/// 3; with `--hold` it stays open, deaf to a hang-up. Its path has a space,
/// so `$VISUAL` must quote it.
fn editor(server: &Server) -> Result<(String, std::path::PathBuf), String> {
    use std::os::unix::fs::PermissionsExt;
    let seen = server.dir.join("seen");
    let path = server.dir.join("my editor");
    let script = format!(
        "#!/bin/sh\nfor f; do :; done\nprintf '%s\\n' \"$@\" > '{0}.args'\n\
         ls -l \"$f\" > '{0}.mode'; printf %s \"$f\" > '{0}.path'; cp \"$f\" '{0}'\n\
         [ \"$1\" = --hold ] && trap '' HUP && exec sleep 60\nexit 3\n",
        seen.display()
    );
    std::fs::write(&path, script).map_err(e)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    Ok((format!("'{}'", path.display()), seen))
}

/// The history files the server of `pid` has made and not removed.
fn history_files(pid: Option<u32>) -> Result<usize, String> {
    let made = format!("fux-{}-", pid.ok_or("the server's pid")?);
    let names = std::fs::read_dir(std::env::temp_dir()).map_err(e)?;
    Ok(names
        .filter_map(Result::ok)
        .filter(|f| f.file_name().to_string_lossy().starts_with(&made))
        .count())
}

/// `C-b i` in an attached terminal opens the focused pane's text, a line
/// wrapped at its width whole, in the terminal's `$VISUAL`, a shell
/// command (which may `exec`), from a file only the user reads; the
/// editor's pane closes as it exits, saying its status, and the file goes
/// with it.
#[test]
fn the_key_opens_the_focused_pane_in_the_editor() -> Outcome {
    let server = Server::start("str update-environment str VISUAL set")?;
    let (editor, seen) = editor(&server)?;
    let visual = format!("exec {editor} --flag 'two words'");
    let mut terminal = Terminal::start(12, 80, &[fux()], |command| {
        command
            .env("FUX_SOCKET", &server.socket)
            .env("XDG_CONFIG_HOME", server.dir.join("config"))
            .env("VISUAL", &visual)
            .env_remove("FUX_PANE")
            .env_remove("FUX_SERVER");
    })?;
    terminal.wait_for("$")?;
    let long: String = std::iter::repeat_n('7', 150).collect();
    let printed = format!("echo early; seq 1 40; echo {long}");
    server.ok(&["%1", "str", &printed, "line"])?;
    terminal.type_bytes(b"\x02i")?;
    terminal.wait_for("exited with status 3")?;
    let text = std::fs::read_to_string(&seen).map_err(e)?;
    // Scrolled off the screen long since, and wrapped at its width.
    assert!(text.lines().any(|l| l == "early"), "{text}");
    assert!(text.lines().any(|l| l == long), "{text}");
    let args = std::fs::read_to_string(server.dir.join("seen.args")).map_err(e)?;
    assert!(args.starts_with("--flag\ntwo words\n"), "{args}");
    let mode = std::fs::read_to_string(server.dir.join("seen.mode")).map_err(e)?;
    assert!(mode.starts_with("-rw-------"), "{mode}");
    let file = std::fs::read_to_string(server.dir.join("seen.path")).map_err(e)?;
    eventually("the file removed", || {
        Ok(!std::path::Path::new(&file).exists())
    })?;
    Ok(())
}

/// From the command line too, the text is the screen's alone, a
/// full-screen program's screen after the history; a dump leaves the file
/// out, closing the editor's pane removes it, and a split refused for want
/// of room makes none.
#[test]
fn edit_history_leaves_no_file_however_its_pane_goes() -> Outcome {
    let server = Server::start("str update-environment str VISUAL set")?;
    let (editor, seen) = editor(&server)?;
    let typed = "echo before; printf '\\033[?1049hon the alt screen'; read x";
    server.ok(&[
        "%1",
        "str",
        typed,
        "text",
        "str",
        "Enter",
        "key",
        "0",
        "timeout",
        "send-keys",
    ])?;
    eventually("the alt screen", || {
        Ok(server.ok(&["%1", "str", "alt_screen", "get"])?.trim() == "true")
    })?;
    let visual = format!("{editor} --hold");
    let env = [("VISUAL", visual.as_str())];
    assert_eq!(server.fux_env(&["%1", "edit-history"], &env)?.status, 0);
    eventually("the editor's copy", || {
        Ok(std::fs::read_to_string(&seen).is_ok_and(|t| t.contains("on the alt screen")))
    })?;
    let text = std::fs::read_to_string(&seen).map_err(e)?;
    let (before, alt) = (text.find("\nbefore\n"), text.find("\non the alt screen"));
    assert!(before.is_some() && before < alt, "{text}");
    assert!(!text.contains("%1 "), "no state line: {text}");
    assert_eq!(history_files(server.pid())?, 1);
    // The file is the pane's alone: a dump makes the editor again without it.
    let file = std::fs::read_to_string(server.dir.join("seen.path")).map_err(e)?;
    let dump = server.ok(&["dump"])?;
    assert!(!dump.contains(&file) && !dump.contains("history-file"), "{dump}");
    server.ok(&["%2", "kill"])?;
    eventually("the file removed", || Ok(history_files(server.pid())? == 0))?;
    server.ok(&["%1", "tab", "20", "3", "resize"])?;
    assert_ne!(server.fux_env(&["%1", "edit-history"], &env)?.status, 0);
    assert_eq!(history_files(server.pid())?, 0);
    Ok(())
}
