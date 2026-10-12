//! Milestone 7: the command column, the choosers, action menus, the `:`
//! prompt, rename and confirmations.
mod support;
use support::*;

const PREFIX: &str = "\x02";
const UP: &str = "\x1b[A";
const DOWN: &str = "\x1b[B";
/// Backspaces enough to empty a prompt holding a name: prompts have no key
/// that clears them.
const CLEAR: &str = "\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f";

fn panes(server: &Server) -> Result<Vec<String>, String> {
    Ok(server
        .ok(&["ls"])?
        .lines()
        .filter_map(|l| l.trim_start().split(' ').next())
        .filter(|w| w.starts_with('%'))
        .map(str::to_owned)
        .collect())
}

/// Waits until `test` holds, while the client takes what the server sends
/// it: programs its prompt runs, the mode its view is in. Once it holds, the
/// client takes what was sent with it, so its next keys go where they
/// should.
fn pumped(
    client: &mut Client,
    what: &str,
    mut test: impl FnMut() -> Result<bool, String>,
) -> Outcome {
    eventually(what, || {
        let held = test()?;
        client.pump()?;
        Ok(held)
    })
}

fn bar_has(client: &mut Client, what: &str) -> Outcome {
    let needle = what.to_owned();
    client.wait(what, move |t| {
        t.lines().last().is_some_and(|b| b.contains(&needle))
    })
}

#[test]
fn the_column_runs_the_selected_command_and_explains_unavailable_ones() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(40, 100)?;
    client.wait_for("$")?;
    client.keys(PREFIX)?;
    client.wait("the column", |t| {
        t.contains("Commands") && t.contains("split side by side")
    })?;
    // Down to the second binding, `split stacked`, and Enter.
    client.keys(DOWN)?;
    client.keys("\r")?;
    pumped(&mut client, "a stacked split", || {
        Ok(focused(&server)? == "%2")
    })?;
    client.wait("the column closed", |t| !t.contains("Commands"))?;
    // Arrows, Home/End and PageUp move; Esc closes without running anything.
    client.keys(&format!("{PREFIX}{DOWN}{DOWN}{UP}\x1b[F\x1b[H\x1b[5~"))?;
    client.keys("\x1b")?;
    client.wait("closed by Esc", |t| !t.contains("Commands"))?;
    assert_eq!(panes(&server)?.len(), 2);
    // An unavailable command is shown dimmed, and running it says why.
    server.ok(&["str", "%2", "pane", "kill"])?;
    client.keys(PREFIX)?;
    client.wait_for("next pane")?;
    let painted = String::from_utf8_lossy(&client.painted).into_owned();
    let dimmed = painted
        .split("\x1b[0;2")
        .any(|chunk| chunk.contains("next pane"));
    assert!(dimmed, "next pane is dimmed with only one pane");
    // Enter on it: `next pane` is the 12th entry (after 7 pane bindings
    // and 4 directions of focus).
    client.keys("\x1b[H")?;
    client.keys(&std::iter::repeat_n(DOWN, 11).collect::<String>())?;
    client.keys("\r")?;
    bar_has(&mut client, "only one pane")?;
    // And by its key, straight from the column.
    client.keys(PREFIX)?;
    client.wait_for("Commands")?;
    client.keys("o")?;
    bar_has(&mut client, "only one pane")?;
    Ok(())
}

#[test]
fn closing_asks_first_and_acts_on_what_it_asked_about() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 90)?;
    client.wait_for("$")?;
    server.ok(&["str", "%1", "pane", "opts", "horizontal", "split"])?;
    server.ok(&["str", "c1", "client", "str", "%2", "pane", "focus-pane"])?;
    client.keys(&format!("{PREFIX}x"))?;
    client.wait_for("close pane %2 sh?")?;
    client.keys("n")?;
    client.wait("closed without closing", |t| !t.contains("close pane"))?;
    assert_eq!(panes(&server)?, ["%1", "%2"]);
    // Ask about %2, move focus to %1 meanwhile: y still closes %2.
    client.keys(&format!("{PREFIX}x"))?;
    client.wait_for("close pane %2 sh?")?;
    server.ok(&["str", "c1", "client", "str", "%1", "pane", "focus-pane"])?;
    client.keys("y")?;
    pumped(&mut client, "%2 closed", || Ok(panes(&server)? == ["%1"]))?;
    // A confirmation whose item disappears closes, saying so.
    server.ok(&["str", "%1", "pane", "opts", "horizontal", "split"])?;
    server.ok(&["str", "c1", "client", "str", "%3", "pane", "focus-pane"])?;
    client.keys(&format!("{PREFIX}x"))?;
    client.wait_for("close pane %3")?;
    server.ok(&["str", "%3", "pane", "kill"])?;
    bar_has(&mut client, "%3 is gone")?;
    client.keys("y")?;
    // Something that shows, typed after the y: once it shows, the y has
    // been handled.
    client.keys(&format!("{PREFIX}e"))?;
    client.wait_for("Enter accepts")?;
    client.keys("\x1b")?;
    assert_eq!(panes(&server)?, ["%1"], "a late y closes nothing");
    Ok(())
}

#[test]
fn menus_act_on_the_item_they_were_opened_for() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(30, 100)?;
    client.wait_for("$")?;
    server.ok(&["str", "%1", "pane", "opts", "horizontal", "split"])?;
    server.ok(&["str", "c1", "client", "str", "%2", "pane", "focus-pane"])?;
    client.keys(&format!("{PREFIX}a"))?;
    client.wait_for("pane %2 sh")?;
    client.wait_for("move to a new tab")?;
    // Focus moves to %1; the menu still acts on %2.
    server.ok(&["str", "c1", "client", "str", "%1", "pane", "focus-pane"])?;
    // `move to a new tab` is the sixth entry.
    for _ in 0..5 {
        client.keys(DOWN)?;
    }
    client.keys("\r")?;
    pumped(&mut client, "%2 in a new tab", || {
        Ok(server.ok(&["ls"])?.contains("@2 tab-2\n    %2"))
    })?;
    // A menu whose item disappears closes, saying so, and runs nothing.
    client.keys(&format!("{PREFIX}a"))?;
    client.wait_for("pane %2")?;
    server.ok(&["str", "%2", "pane", "kill"])?;
    bar_has(&mut client, "%2 is gone")?;
    // Tab and workspace menus.
    client.keys(&format!("{PREFIX}ta"))?;
    client.wait_for("tab @1 main")?;
    // Escape and a prefix within the Escape delay would read as M-C-b.
    client.keys("\x1b")?;
    client.wait("the tab menu closed", |t| !t.contains("tab @1 main"))?;
    client.keys(&format!("{PREFIX}wa"))?;
    client.wait_for("workspace +1 main")?;
    client.keys(&format!("{DOWN}{DOWN}\r"))?;
    pumped(&mut client, "a new workspace", || {
        Ok(server.ok(&["ls"])?.contains("+2 workspace-2"))
    })?;
    Ok(())
}

#[test]
fn choosers_select_rename_and_close() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(30, 100)?;
    client.wait_for("$")?;
    server.ok(&[
        "opts",
        "str",
        "+1",
        "workspace",
        "in",
        "str",
        "second",
        "name",
        "new-tab",
    ])?;
    server.ok(&[
        "opts",
        "str",
        "+1",
        "workspace",
        "in",
        "str",
        "third",
        "name",
        "new-tab",
    ])?;
    client.keys(&format!("{PREFIX}tg"))?;
    client.wait("the tab chooser, current marked", |t| {
        t.contains("* @1 main") && t.contains("@3 third")
    })?;
    client.keys(&format!("{DOWN}\r"))?;
    pumped(&mut client, "on @2", || {
        Ok(server.ok(&["ls"])?.contains("client c1 100x30 +1 @2"))
    })?;
    // r renames the selected tab.
    client.keys(&format!("{PREFIX}tg"))?;
    client.wait_for("* @2 second")?;
    client.keys(&format!("{DOWN}r"))?;
    client.wait_for("rename tab @3")?;
    client.keys(&format!("{CLEAR}renamed\r"))?;
    pumped(&mut client, "renamed", || {
        Ok(server.ok(&["ls"])?.contains("@3 renamed"))
    })?;
    // x closes it, after asking.
    client.keys(&format!("{PREFIX}tg"))?;
    client.wait_for("Enter selects")?;
    client.keys(&format!("{DOWN}{DOWN}x"))?;
    client.wait_for("close tab @3 renamed?")?;
    client.keys("y")?;
    pumped(&mut client, "closed", || {
        Ok(!server.ok(&["ls"])?.contains("@3"))
    })?;
    // The workspace chooser lists every workspace with its panes.
    server.ok(&["opts", "str", "elsewhere", "name", "new-workspace"])?;
    client.keys(&format!("{PREFIX}wg"))?;
    client.wait("the workspace chooser", |t| {
        t.contains("+2 elsewhere") && t.contains("* +1 main")
    })?;
    client.keys(&format!("{DOWN}\r"))?;
    bar_has(&mut client, "elsewhere")?;
    // The move choosers from the pane menu move the pane there.
    server.ok(&["str", "%4", "pane", "opts", "horizontal", "split"])?;
    client.keys(&format!("{PREFIX}a"))?;
    client.wait_for("move to workspace")?;
    for _ in 0..6 {
        client.keys(DOWN)?;
    }
    client.keys("\r")?;
    client.wait_for("move %5 to workspace")?;
    // The chooser starts on the current workspace; +1 is above it.
    client.keys(&format!("{UP}\r"))?;
    pumped(&mut client, "%5 moved to +1", || {
        let ls = server.ok(&["ls"])?;
        Ok(ls.find("%5").unwrap_or(usize::MAX) < ls.find("+2").unwrap_or(0))
    })?;
    Ok(())
}

#[test]
fn the_prompt_runs_commands_and_shows_their_output_or_error() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 100)?;
    client.wait_for("$")?;
    client.keys(&format!("{PREFIX}e"))?;
    client.wait_for("Enter accepts")?;
    client.keys("me focused opts vertical str echo program str 'from the prompt' arg split\r")?;
    client.wait("typed into the new pane", |t| {
        t.lines().any(|l| l == "from the prompt")
    })?;
    client.keys(&format!("{PREFIX}e"))?;
    client.wait_for("Enter accepts")?;
    client.keys("str %1 pane str left rename\r")?;
    pumped(&mut client, "renamed", || {
        Ok(server.ok(&["ls"])?.contains("%1 left"))
    })?;
    client.keys(&format!("{PREFIX}e"))?;
    client.wait_for("Enter accepts")?;
    client.keys("nope\r")?;
    bar_has(&mut client, "nope: no such word")?;
    // Editing: typing, moving and deleting before Enter.
    client.keys(&format!("{PREFIX}e"))?;
    client.wait_for("Enter accepts")?;
    client.keys(
        "opts str edited nameX\x7f new-tab\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x1b[H\x1b[F\r",
    )?;
    pumped(&mut client, "the edited command ran", || {
        Ok(server.ok(&["ls"])?.contains("edited"))
    })?;
    // A paste goes into the prompt, one line of it, never run by itself.
    client.keys(&format!("{PREFIX}e"))?;
    client.wait_for("Enter accepts")?;
    client.keys("\x1b[200~str %1 pane str pasted rename\nkill-server\x1b[201~")?;
    client.wait_for("str %1 pane str pasted rename▏")?;
    assert!(server.ok(&["ls"])?.contains("%1 left"), "nothing ran yet");
    client.keys("\r")?;
    pumped(&mut client, "renamed by the pasted line", || {
        Ok(server.ok(&["ls"])?.contains("%1 pasted"))
    })?;
    Ok(())
}

#[test]
fn rename_prompts_start_from_the_current_name() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 100)?;
    client.wait_for("$")?;
    // Renaming a pane has no key of its own (the pane menu offers it).
    server.ok(&["str", "c1", "client", "str", "%1", "pane", "rename-prompt"])?;
    client.wait_for("rename pane %1")?;
    client.wait_for("sh▏")?;
    client.keys("ell\r")?;
    pumped(&mut client, "renamed", || {
        Ok(server.ok(&["ls"])?.contains("%1 shell"))
    })?;
    // An empty name is refused, and says so.
    server.ok(&["str", "c1", "client", "str", "%1", "pane", "rename-prompt"])?;
    client.wait_for("rename pane %1")?;
    client.keys(&format!("{CLEAR}\r"))?;
    bar_has(&mut client, "a name cannot be empty")?;
    server.ok(&["str", "c1", "client", "str", "@1", "tab", "rename-prompt"])?;
    client.wait_for("rename tab @1")?;
    client.keys("\x1b")?;
    Ok(())
}

#[test]
fn detaching_with_an_overlay_open_is_safe() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 100)?;
    client.wait_for("$")?;
    client.keys(&format!("{PREFIX}a"))?;
    client.wait_for("pane %1")?;
    server.ok(&["str", "c1", "client", "detach"])?;
    assert_eq!(client.wait_exit()?, "detached");
    let mut other = server.attach(20, 100)?;
    other.wait_for("$")?;
    other.keys(&format!("{PREFIX}tg"))?;
    other.wait_for("* @1")?;
    other.detach()?;
    assert!(server.ok(&["ls"])?.contains("%1"));
    Ok(())
}
