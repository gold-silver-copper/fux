//! Milestone 4: workspaces and tabs; two clients with independent views; a
//! PTY's size is the smallest rectangle among the clients showing it.
mod support;
use support::*;

const PREFIX: &str = "\x02";

fn client_line(server: &Server, id: &str) -> Result<String, String> {
    let ls = server.ok(&["ls"])?;
    ls.lines()
        .find(|l| l.starts_with(&format!("client {id} ")))
        .map(str::to_owned)
        .ok_or_else(|| format!("no client {id}: {ls}"))
}

#[test]
fn tabs_are_created_and_switched_by_key() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    client.keys(&format!("{PREFIX}tn"))?;
    client.wait("a second tab", |t| {
        t.lines().last().is_some_and(|b| b.contains("tab-2"))
    })?;
    eventually("on @2", || Ok(client_line(&server, "c1")?.contains(" @2 ")))?;
    client.keys("echo on-two\r")?;
    client.wait_for("on-two")?;
    client.keys(&format!("{PREFIX}b"))?;
    eventually("back on @1", || {
        Ok(client_line(&server, "c1")?.contains(" @1 "))
    })?;
    client.wait("tab 1's screen", |t| !t.contains("on-two"))?;
    client.keys(&format!("{PREFIX}n"))?;
    eventually("on @2 again", || {
        Ok(client_line(&server, "c1")?.contains(" @2 "))
    })?;
    client.wait_for("on-two")?;
    // Next wraps around; the tab layer's `l` is next too.
    client.keys(&format!("{PREFIX}tl"))?;
    eventually("wrapped to @1", || {
        Ok(client_line(&server, "c1")?.contains(" @1 "))
    })?;
    // new-tab from the CLI, with a name and a command typed into it.
    server.ok(&["new-tab", "-t", "+1", "-n", "logs", "--", "echo", "in logs"])?;
    client.wait("the named tab", |t| {
        t.lines().last().is_some_and(|b| b.contains("logs"))
    })?;
    Ok(())
}

#[test]
fn workspaces_are_created_and_switched_by_key() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    client.keys(&format!("{PREFIX}wn"))?;
    client.wait("the new workspace", |t| {
        t.lines().last().is_some_and(|b| b.contains("workspace-2"))
    })?;
    eventually("on +2", || Ok(client_line(&server, "c1")?.contains(" +2 ")))?;
    client.keys(&format!("{PREFIX}wh"))?;
    eventually("back on +1", || {
        Ok(client_line(&server, "c1")?.contains(" +1 "))
    })?;
    client.keys(&format!("{PREFIX}wl"))?;
    eventually("+2 again", || {
        Ok(client_line(&server, "c1")?.contains(" +2 "))
    })?;
    server.ok(&["new-workspace", "-n", "third"])?;
    server.ok(&["select-workspace", "-c", "c1", "-t", "third"])?;
    client.wait("the third", |t| {
        t.lines().last().is_some_and(|b| b.contains("third"))
    })?;
    // Attaching to a workspace by name.
    let mut other = Client::attach(&server.socket, 10, 60, Some("third"))?;
    other.wait("attached to third", |t| {
        t.lines().last().is_some_and(|b| b.contains("third"))
    })?;
    Ok(())
}

#[test]
fn two_clients_have_independent_views() -> Outcome {
    let server = Server::start("")?;
    let mut one = server.attach(20, 80)?;
    one.wait_for("$")?;
    let mut two = server.attach(20, 80)?;
    two.wait_for("$")?;
    one.keys(&format!("{PREFIX}v"))?;
    eventually("one focuses the new pane", || {
        Ok(client_line(&server, "c1")?.ends_with("%2"))
    })?;
    // The other client sees the split but keeps its own focus.
    two.wait_for("│")?;
    assert!(
        client_line(&server, "c2")?.ends_with("%1"),
        "{}",
        client_line(&server, "c2")?
    );
    // Keys go to each client's own focused pane.
    one.keys("echo typed-by-one\r")?;
    two.keys("echo typed-by-two\r")?;
    for client in [&mut one, &mut two] {
        client.wait_for("typed-by-one")?;
        client.wait_for("typed-by-two")?;
    }
    let right = server.ok(&["capture-pane", "-t", "%2"])?;
    assert!(
        right.contains("typed-by-one") && !right.contains("typed-by-two"),
        "{right}"
    );
    // Switching tabs in one leaves the other where it was.
    one.keys(&format!("{PREFIX}tn"))?;
    eventually("one is on @2", || {
        Ok(client_line(&server, "c1")?.contains(" @2 "))
    })?;
    assert!(client_line(&server, "c2")?.contains(" @1 "));
    // Zoom is per client too.
    two.keys(&format!("{PREFIX}z"))?;
    two.wait("two is zoomed", |t| {
        t.lines().last().is_some_and(|b| b.contains("[zoom]"))
    })?;
    one.keys(&format!("{PREFIX}b"))?;
    one.wait("one is not", |t| {
        t.contains('│') && !t.lines().last().is_some_and(|b| b.contains("[zoom]"))
    })?;
    Ok(())
}

#[test]
fn a_pty_is_the_smallest_rectangle_among_the_clients_showing_it() -> Outcome {
    let server = Server::start("")?;
    let mut big = server.attach(30, 100)?;
    big.wait_for("$")?;
    let mut small = server.attach(10, 40)?;
    small.wait_for("$")?;
    eventually("the small size wins", || {
        Ok(server.ok(&["ls"])?.contains("%1 sh 40x9"))
    })?;
    // The big client shows the pane at the top left, the rest blank.
    big.keys("stty size\r")?;
    big.wait("the pty size", |t| t.lines().any(|l| l == "9 40"))?;
    let lines = big.lines();
    assert!(
        lines.iter().take(29).all(|l| l.chars().count() <= 40),
        "{lines:?}"
    );
    // When the small client looks elsewhere, the pane grows.
    small.keys(&format!("{PREFIX}tn"))?;
    eventually("grown to the big client", || {
        Ok(server.ok(&["ls"])?.contains("%1 sh 100x29"))
    })?;
    // And when it detaches, only the big client counts.
    small.keys(&format!("{PREFIX}b"))?;
    eventually("small again", || {
        Ok(server.ok(&["ls"])?.contains("%1 sh 40x9"))
    })?;
    small.detach()?;
    eventually("the small client gone", || {
        Ok(server.ok(&["ls"])?.contains("%1 sh 100x29"))
    })?;
    Ok(())
}

#[test]
fn a_view_repairs_itself_when_what_it_shows_is_removed() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 80)?;
    client.wait_for("$")?;
    // Three tabs; the client is on the middle one.
    server.ok(&["new-tab", "-t", "+1"])?;
    server.ok(&["new-tab", "-t", "+1"])?;
    server.ok(&["select-tab", "-c", "c1", "-t", "@2"])?;
    // Closing the tab it shows selects its neighbour.
    server.ok(&["kill-tab", "-t", "@2"])?;
    eventually("the next tab", || {
        Ok(client_line(&server, "c1")?.contains(" @3 "))
    })?;
    // Closing the focused pane focuses the last-focused surviving one.
    server.ok(&["split", "-h", "-t", "%3"])?;
    server.ok(&["split", "-h", "-t", "%4"])?;
    server.ok(&["select-pane", "-c", "c1", "-t", "%3"])?;
    server.ok(&["select-pane", "-c", "c1", "-t", "%5"])?;
    server.ok(&["kill-pane", "-t", "%5"])?;
    eventually("back to %3", || {
        Ok(client_line(&server, "c1")?.ends_with("%3"))
    })?;
    // Closing its workspace moves it to the first remaining one.
    server.ok(&["new-workspace", "-n", "doomed"])?;
    server.ok(&["select-workspace", "-c", "c1", "-t", "doomed"])?;
    server.ok(&["kill-workspace", "-t", "doomed"])?;
    eventually("on +1", || Ok(client_line(&server, "c1")?.contains(" +1 ")))?;
    Ok(())
}

#[test]
fn moving_a_panes_out_leaves_an_empty_tab_with_a_hint() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 80)?;
    client.wait_for("$")?;
    server.ok(&["new-tab", "-t", "+1"])?;
    server.ok(&["select-tab", "-c", "c1", "-t", "@1"])?;
    server.ok(&["move-pane", "-t", "%1", "--to", "@2"])?;
    client.wait("the empty tab's hint", |t| {
        t.contains("empty tab: C-b v splits it, C-b t x closes it")
    })?;
    assert!(server.ok(&["ls"])?.contains("@1 main (empty)"));
    // Moving to a new tab and a new workspace creates them.
    server.ok(&["move-pane", "-t", "%2", "--to", "new-tab"])?;
    server.ok(&["move-pane", "-t", "%1", "--to", "new-workspace"])?;
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("+2 workspace-2"), "{ls}");
    // A split in the empty tab seeds it: it needs a pane to split, so the
    // tab chooser and moves are the way in; kill-tab closes it.
    server.ok(&["kill-tab", "-t", "@1"])?;
    assert!(!server.ok(&["ls"])?.contains("@1 main"));
    Ok(())
}

/// `capture-client` prints what a client's terminal shows: in every mode,
/// once its paints have arrived, the client's own screen and the capture
/// agree row for row, bar and overlays included.
#[test]
fn capture_client_shows_what_a_client_shows_in_every_mode() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["split", "-h", "-t", "%1"])?;
    let mut client = server.attach(20, 90)?;
    client.wait_for("$")?;
    let captured = |server: &Server| -> Result<Vec<String>, String> {
        Ok(server
            .ok(&["capture-client", "-c", "c1"])?
            .lines()
            .map(str::to_owned)
            .collect())
    };
    let agree = |client: &mut Client, server: &Server, mode: &str| -> Outcome {
        client
            .wait(mode, |t| {
                captured(server).is_ok_and(|rows| rows.join("\n") == t)
            })
            .map_err(|error| {
                format!(
                    "{error}\ncapture-client shows:\n{}",
                    captured(server).unwrap_or_default().join("\n")
                )
            })
    };
    agree(&mut client, &server, "normal")?;
    // Leaving a mode is done once the screen is as it was.
    let normal = client.text();
    let escape = |client: &mut Client| -> Outcome {
        client.keys("\x1b")?;
        client.wait("normal again", |t| t == normal)
    };
    for (keys, mode, shows) in [
        ("\x02", "the column", "Commands"),
        ("\x02t", "a layer", "Tabs"),
        ("\x02tg", "a chooser", "@1"),
        ("\x02a", "a menu", "pane"),
        ("\x02e", "the prompt", "Enter accepts"),
        ("\x02x", "a confirmation", "close"),
        ("\x02c", "copy mode", "COPY"),
    ] {
        client.keys(keys)?;
        client.wait_for(shows)?;
        agree(&mut client, &server, mode)?;
        escape(&mut client)?;
    }
    // A repeat mode, from its layer's first key: the bar names it.
    client.keys("\x02rh")?;
    client.wait("resize mode", |t| {
        t.lines().last().is_some_and(|b| b.contains("RESIZE"))
    })?;
    agree(&mut client, &server, "a repeat mode")?;
    // The resize moved the border, so the screen is not as it was.
    client.keys("\x1b")?;
    client.wait("the mode ended", |t| {
        !t.lines().last().is_some_and(|b| b.contains("RESIZE"))
    })?;
    // As JSON: the client, its size, the cursor or null, and the rows.
    let json = server.ok(&["capture-client", "-c", "c1", "--json"])?;
    assert!(
        json.starts_with("{\"client\":\"c1\",\"rows\":20,\"cols\":90,\"cursor\":"),
        "{json}"
    );
    let rows = json.split_once("\"lines\":[").map_or("", |(_, rows)| rows);
    assert_eq!(rows.matches("\",\"").count() + 1, 20, "twenty rows: {json}");
    // From the command line it needs -c; a client that is not there is an error.
    let out = server.fux(&["capture-client"])?;
    assert_eq!(out.status, 1, "{}", out.stderr);
    assert!(out.stderr.contains("-c"), "{}", out.stderr);
    assert_eq!(server.fux(&["capture-client", "-c", "c9"])?.status, 1);
    Ok(())
}
