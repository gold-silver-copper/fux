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
        t.lines().last().is_some_and(|b| b.contains("%2"))
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
    // new-tab from the CLI, with a name.
    server.ok(&[
        "opts",
        "60",
        "9",
        "size",
        "str",
        "+1",
        "workspace",
        "in",
        "str",
        "logs",
        "name",
        "new-tab",
    ])?;
    client.wait("the named tab", |t| {
        t.lines().last().is_some_and(|b| b.contains("logs"))
    })?;
    server.ok(&["str", "c1", "client", "str", "last", "step-tab"])?;
    assert!(client_line(&server, "c1")?.contains(" @2 "));
    client.wait_for("on-two")?;
    Ok(())
}

#[test]
fn workspaces_are_created_and_switched_by_key() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    client.keys(&format!("{PREFIX}wn"))?;
    client.wait("the new workspace", |t| {
        t.lines().last().is_some_and(|b| b.contains("+2"))
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
    server.ok(&[
        "opts",
        "60",
        "9",
        "size",
        "str",
        "third",
        "name",
        "new-workspace",
    ])?;
    server.ok(&["str", "c1", "client", "str", "third", "workspace", "show"])?;
    client.wait("the third", |t| {
        t.lines().last().is_some_and(|b| b.contains("third"))
    })?;
    // Attaching to a workspace by name.
    let mut other = server.attach_to(10, 60, "third")?;
    other.wait("attached to third", |t| {
        t.lines().last().is_some_and(|b| b.contains("third"))
    })?;
    server.ok(&["str", "c1", "client", "str", "last", "step-workspace"])?;
    assert!(client_line(&server, "c1")?.contains(" +2 "));
    assert!(client_line(&server, "c2")?.contains(" +3 "));
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
    let right = server.ok(&["str", "%2", "pane", "capture"])?;
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
        Ok(server.ok(&["ls"])?.contains("%1 40x9"))
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
        Ok(server.ok(&["ls"])?.contains("%1 100x29"))
    })?;
    // And when it detaches, only the big client counts.
    small.keys(&format!("{PREFIX}b"))?;
    eventually("small again", || {
        Ok(server.ok(&["ls"])?.contains("%1 40x9"))
    })?;
    small.detach()?;
    eventually("the small client gone", || {
        Ok(server.ok(&["ls"])?.contains("%1 100x29"))
    })?;
    Ok(())
}

/// A tab closes as its last pane moves out, as it does when its last pane
/// closes, and its client is shown the tab beside it.
#[test]
fn moving_a_tabs_last_pane_out_closes_the_tab() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 80)?;
    client.wait_for("$")?;
    server.ok(&[
        "opts",
        "80",
        "19",
        "size",
        "str",
        "+1",
        "workspace",
        "in",
        "new-tab",
    ])?;
    server.ok(&["str", "c1", "client", "str", "@1", "tab", "show"])?;
    server.ok(&["str", "%1", "pane", "str", "@2", "tab", "move"])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        !ls.contains("@1 ") && ls.contains("client c1 80x20 +1 @2 "),
        "{ls}"
    );
    // Moving to a new tab and a new workspace creates them; the tab and
    // the workspace each leaves with nothing in them close.
    server.ok(&["str", "%2", "pane", "str", "new-tab", "move"])?;
    server.ok(&["str", "%1", "pane", "str", "new-workspace", "move"])?;
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("+2") && !ls.contains("@2 "), "{ls}");
    Ok(())
}

/// Client capture includes the composed UI and has no pane version.
#[test]
fn client_capture_includes_ui_without_a_pane_version() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 90)?;
    client.wait_for("$")?;
    server.ok(&["all", "str", "session-notice", "notify"])?;
    client.wait_for("session-notice")?;
    assert!(
        server
            .ok(&["str", "c1", "client", "capture"])?
            .contains("session-notice")
    );
    client.keys("\x02e")?;
    client.wait_for("Enter accept")?;
    let capture = server.ok(&["str", "c1", "client", "capture"])?;
    assert!(capture.contains("Enter accept"), "{capture}");
    assert_eq!(capture.lines().count(), 20);
    // As JSON: the client, its size, the cursor or null, and the rows.
    let json = server.ok(&["str", "c1", "client", "capture", "json"])?;
    assert!(
        json.starts_with(
            "{\"client\":\"c1\",\"screen_rows\":20,\"screen_cols\":90,\"screen_cursor\":"
        ),
        "{json}"
    );
    assert!(!json.contains("\"version\":"), "{json}");
    let rows = json.split_once("\"lines\":[").map_or("", |(_, rows)| rows);
    assert_eq!(rows.matches("\",\"").count() + 1, 20, "twenty rows: {json}");
    // A client that is not there is an error.
    assert_eq!(server.fux(&["str", "c9", "client", "capture"])?.status, 1);
    Ok(())
}

/// A tab with a size of its own, shown to a client, takes the client's
/// screen; hidden again (another tab of its workspace, or another
/// workspace, shown), it goes back to its own size.
#[test]
fn a_hidden_tab_goes_back_to_its_own_size() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(25, 84)?;
    client.wait_for("$")?;
    let run = [
        "agent-opts",
        "str",
        "hid",
        "name",
        "100",
        "40",
        "size",
        "str",
        "cat",
        "program",
        "run",
    ];
    let pane = server.ok(&run)?;
    let pane = pane.split(':').next().unwrap_or_default();
    let sized = |size: &str| Ok(server.ok(&["ls"])?.contains(&format!("hid {size}")));
    eventually("its own size", || sized("100x40"))?;
    server.ok(&["str", "c1", "client", pane, "tab", "show"])?;
    eventually("the client's size", || sized("84x24"))?;
    let other = server.ok(&[
        "agent-opts",
        "84",
        "24",
        "size",
        "str",
        "sleep",
        "program",
        "str",
        "60",
        "arg",
        "run",
    ])?;
    let other = other.split(':').next().unwrap_or_default();
    server.ok(&["str", "c1", "client", other, "tab", "show"])?;
    eventually("its own size, another tab shown", || sized("100x40"))?;
    server.ok(&["str", "c1", "client", pane, "tab", "show"])?;
    eventually("the client's size again", || sized("84x24"))?;
    server.ok(&["str", "c1", "client", "str", "main", "workspace", "show"])?;
    eventually("its own size again", || sized("100x40"))?;
    Ok(())
}
