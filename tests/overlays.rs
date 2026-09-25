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

fn focused(server: &Server) -> Result<String, String> {
    let ls = server.ok(&["ls"])?;
    let client = ls
        .lines()
        .find(|l| l.starts_with("client c1"))
        .ok_or("no client")?;
    Ok(client
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .to_owned())
}

fn panes(server: &Server) -> Result<Vec<String>, String> {
    Ok(server
        .ok(&["ls"])?
        .lines()
        .filter_map(|l| l.trim_start().split(' ').next())
        .filter(|w| w.starts_with('%'))
        .map(str::to_owned)
        .collect())
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
    eventually("a stacked split", || Ok(focused(&server)? == "%2"))?;
    client.wait("the column closed", |t| !t.contains("Commands"))?;
    // Arrows, Home/End and PageUp move; Esc closes without running anything.
    client.keys(&format!("{PREFIX}{DOWN}{DOWN}{UP}\x1b[F\x1b[H\x1b[5~"))?;
    client.keys("\x1b")?;
    client.wait("closed by Esc", |t| !t.contains("Commands"))?;
    assert_eq!(panes(&server)?.len(), 2);
    // An unavailable command is shown dimmed, and running it says why.
    server.ok(&["kill-pane", "-t", "%2"])?;
    client.keys(PREFIX)?;
    client.wait_for("next pane")?;
    let painted = String::from_utf8_lossy(&client.painted).into_owned();
    let dimmed = painted
        .split("\x1b[0;2")
        .any(|chunk| chunk.contains("next pane"));
    assert!(dimmed, "next pane is dimmed with only one pane");
    // Enter on it: `next pane` is the 14th entry (after 7 pane bindings,
    // the resize and move layers, and 4 directions of focus).
    client.keys("\x1b[H")?;
    client.keys(&std::iter::repeat_n(DOWN, 13).collect::<String>())?;
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
    server.ok(&["split", "-h", "-t", "%1"])?;
    server.ok(&["select-pane", "-c", "c1", "-t", "%2"])?;
    client.keys(&format!("{PREFIX}x"))?;
    client.wait_for("close pane %2 sh?")?;
    client.keys("n")?;
    client.wait("closed without closing", |t| !t.contains("close pane"))?;
    assert_eq!(panes(&server)?, ["%1", "%2"]);
    // Ask about %2, move focus to %1 meanwhile: y still closes %2.
    client.keys(&format!("{PREFIX}x"))?;
    client.wait_for("close pane %2 sh?")?;
    server.ok(&["select-pane", "-c", "c1", "-t", "%1"])?;
    client.keys("y")?;
    eventually("%2 closed", || Ok(panes(&server)? == ["%1"]))?;
    // A confirmation whose item disappears closes, saying so.
    server.ok(&["split", "-h", "-t", "%1"])?;
    server.ok(&["select-pane", "-c", "c1", "-t", "%3"])?;
    client.keys(&format!("{PREFIX}x"))?;
    client.wait_for("close pane %3")?;
    server.ok(&["kill-pane", "-t", "%3"])?;
    bar_has(&mut client, "%3 is gone")?;
    client.keys("y")?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(panes(&server)?, ["%1"], "a late y closes nothing");
    Ok(())
}

#[test]
fn menus_act_on_the_item_they_were_opened_for() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(30, 100)?;
    client.wait_for("$")?;
    server.ok(&["split", "-h", "-t", "%1"])?;
    server.ok(&["select-pane", "-c", "c1", "-t", "%2"])?;
    client.keys(&format!("{PREFIX}a"))?;
    client.wait_for("pane %2 sh")?;
    client.wait_for("move to a new tab")?;
    // Focus moves to %1; the menu still acts on %2.
    server.ok(&["select-pane", "-c", "c1", "-t", "%1"])?;
    // `move to a new tab` is the sixth entry.
    for _ in 0..5 {
        client.keys(DOWN)?;
    }
    client.keys("\r")?;
    eventually("%2 in a new tab", || {
        Ok(server.ok(&["ls"])?.contains("@2 tab-2\n    %2"))
    })?;
    // A menu whose item disappears closes, saying so, and runs nothing.
    client.keys(&format!("{PREFIX}a"))?;
    client.wait_for("pane %2")?;
    server.ok(&["kill-pane", "-t", "%2"])?;
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
    eventually("a new workspace", || {
        Ok(server.ok(&["ls"])?.contains("+2 workspace-2"))
    })?;
    Ok(())
}

#[test]
fn choosers_select_rename_and_close() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(30, 100)?;
    client.wait_for("$")?;
    server.ok(&["new-tab", "-t", "+1", "-n", "second"])?;
    server.ok(&["new-tab", "-t", "+1", "-n", "third"])?;
    client.keys(&format!("{PREFIX}tg"))?;
    client.wait("the tab chooser, current marked", |t| {
        t.contains("* @1 main") && t.contains("@3 third")
    })?;
    client.keys(&format!("{DOWN}\r"))?;
    eventually("on @2", || {
        Ok(server.ok(&["ls"])?.contains("client c1 100x30 +1 @2"))
    })?;
    // r renames the selected tab.
    client.keys(&format!("{PREFIX}tg"))?;
    client.wait_for("* @2 second")?;
    client.keys(&format!("{DOWN}r"))?;
    client.wait_for("rename tab @3")?;
    client.keys(&format!("{CLEAR}renamed\r"))?;
    eventually("renamed", || Ok(server.ok(&["ls"])?.contains("@3 renamed")))?;
    // x closes it, after asking.
    client.keys(&format!("{PREFIX}tg"))?;
    client.keys(&format!("{DOWN}{DOWN}x"))?;
    client.wait_for("close tab @3 renamed?")?;
    client.keys("y")?;
    eventually("closed", || Ok(!server.ok(&["ls"])?.contains("@3")))?;
    // The workspace chooser lists every workspace with its panes.
    server.ok(&["new-workspace", "-n", "elsewhere"])?;
    client.keys(&format!("{PREFIX}wg"))?;
    client.wait("the workspace chooser", |t| {
        t.contains("+2 elsewhere") && t.contains("* +1 main")
    })?;
    client.keys(&format!("{DOWN}\r"))?;
    bar_has(&mut client, "elsewhere")?;
    // The move choosers from the pane menu move the pane there.
    server.ok(&["split", "-h", "-t", "%4"])?;
    client.keys(&format!("{PREFIX}a"))?;
    client.wait_for("move to workspace")?;
    for _ in 0..6 {
        client.keys(DOWN)?;
    }
    client.keys("\r")?;
    client.wait_for("move %5 to workspace")?;
    // The chooser starts on the current workspace; +1 is above it.
    client.keys(&format!("{UP}\r"))?;
    eventually("%5 moved to +1", || {
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
    client.keys("split -v -- echo 'from the prompt'\r")?;
    client.wait("typed into the new pane", |t| {
        t.lines().any(|l| l == "from the prompt")
    })?;
    client.keys(&format!("{PREFIX}erename -t %1 left\r"))?;
    eventually("renamed", || Ok(server.ok(&["ls"])?.contains("%1 left")))?;
    client.keys(&format!("{PREFIX}enope\r"))?;
    bar_has(&mut client, "unknown command \"nope\"")?;
    client.keys(&format!("{PREFIX}elist-buffers\r"))?;
    // Editing: typing, moving and deleting before Enter.
    client.keys(&format!(
        "{PREFIX}enew-tabX\x7f -n edited\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x1b[H\x1b[F\r"
    ))?;
    eventually("the edited command ran", || {
        Ok(server.ok(&["ls"])?.contains("edited"))
    })?;
    // A paste goes into the prompt, one line of it, never run by itself.
    client.keys(&format!("{PREFIX}e"))?;
    client.wait_for("Enter accepts")?;
    client.keys("\x1b[200~rename -t %1 pasted\nkill-server\x1b[201~")?;
    client.wait_for("rename -t %1 pasted▏")?;
    assert!(server.ok(&["ls"])?.contains("%1 left"), "nothing ran yet");
    client.keys("\r")?;
    eventually("renamed by the pasted line", || {
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
    server.ok(&["rename-prompt", "-c", "c1", "pane"])?;
    client.wait_for("rename pane %1")?;
    client.wait_for("sh▏")?;
    client.keys("ell\r")?;
    eventually("renamed", || Ok(server.ok(&["ls"])?.contains("%1 shell")))?;
    // An empty name is refused, and says so.
    server.ok(&["rename-prompt", "-c", "c1", "pane"])?;
    client.wait_for("rename pane %1")?;
    client.keys(&format!("{CLEAR}\r"))?;
    bar_has(&mut client, "a name cannot be empty")?;
    // From the CLI, a rename prompt needs -c.
    server.ok(&["rename-prompt", "-c", "c1", "tab"])?;
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
    server.ok(&["detach", "-c", "c1"])?;
    assert_eq!(client.wait_exit()?, "detached");
    let mut other = server.attach(20, 100)?;
    other.wait_for("$")?;
    other.keys(&format!("{PREFIX}tg"))?;
    other.wait_for("* @1")?;
    other.detach()?;
    assert!(server.ok(&["ls"])?.contains("%1"));
    Ok(())
}
