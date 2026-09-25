//! Milestone 3: splits, focus (next, previous, last, directional), resize by
//! weight, zoom, and the bar.
mod support;
use support::*;

const PREFIX: &str = "\x02";

/// `%N` and its size, from `fux ls`.
fn size(server: &Server, pane: &str) -> Result<(u16, u16), String> {
    let ls = server.ok(&["ls"])?;
    let line = ls
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("{pane} ")))
        .ok_or_else(|| format!("{pane} is not listed: {ls}"))?;
    let dims = line
        .split_whitespace()
        .find(|w| w.contains('x') && w.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .ok_or("no size")?;
    let (cols, rows) = dims.split_once('x').ok_or("bad size")?;
    Ok((rows.parse().map_err(e)?, cols.parse().map_err(e)?))
}

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

#[test]
fn splitting_side_by_side_and_stacked_draws_separators_and_focuses_the_new_pane() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 81)?;
    client.wait_for("$")?;
    client.keys(&format!("{PREFIX}v"))?;
    client.wait("a vertical separator", |t| {
        t.lines()
            .all(|l| l.chars().nth(40) == Some('│') || l.contains("main"))
    })?;
    assert_eq!(focused(&server)?, "%2");
    assert_eq!(size(&server, "%1")?, (19, 40));
    assert_eq!(size(&server, "%2")?, (19, 40));
    client.keys(&format!("{PREFIX}s"))?;
    client.wait("a horizontal separator", |t| {
        t.lines().any(|l| l.contains("├─") || l.contains("─────"))
    })?;
    assert_eq!(focused(&server)?, "%3");
    assert_eq!(size(&server, "%2")?, (9, 40));
    assert_eq!(size(&server, "%3")?, (9, 40));
    // The new shell is live and typed into.
    client.keys("echo in-three\r")?;
    client.wait_for("in-three")?;
    assert!(client.bar().contains("%3 sh"), "{}", client.bar());
    // The CLI splits too, and a -- CMD is typed into the new shell.
    server.ok(&["split", "-v", "-t", "%1", "--", "echo", "from cli"])?;
    client.wait_for("from cli")?;
    Ok(())
}

#[test]
fn focus_moves_next_previous_last_and_by_direction() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 81)?;
    client.wait_for("$")?;
    // 1 | (2 / 3)
    client.keys(&format!("{PREFIX}v"))?;
    eventually("%2", || Ok(focused(&server)? == "%2"))?;
    client.keys(&format!("{PREFIX}s"))?;
    eventually("%3", || Ok(focused(&server)? == "%3"))?;
    client.keys(&format!("{PREFIX}o"))?;
    eventually("next wraps to %1", || Ok(focused(&server)? == "%1"))?;
    // The previous pane has no key of its own: the command.
    server.ok(&["select-pane", "-c", "c1", "--previous"])?;
    eventually("previous: %3", || Ok(focused(&server)? == "%3"))?;
    client.keys(&format!("{PREFIX}q"))?;
    eventually("last: %1", || Ok(focused(&server)? == "%1"))?;
    client.keys(&format!("{PREFIX}l"))?;
    eventually(
        "right of %1: the upper right, closest to its centre",
        || Ok(focused(&server)? == "%2"),
    )?;
    client.keys(&format!("{PREFIX}j"))?;
    eventually("down: %3", || Ok(focused(&server)? == "%3"))?;
    client.keys(&format!("{PREFIX}h"))?;
    eventually("left: %1", || Ok(focused(&server)? == "%1"))?;
    // Nothing further left: a notice, and focus stays.
    client.keys(&format!("{PREFIX}h"))?;
    client.wait("the notice", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("no pane left of %1"))
    })?;
    assert_eq!(focused(&server)?, "%1");
    // From the CLI, a client's focus needs -c.
    server.ok(&["select-pane", "-c", "c1", "-t", "%3"])?;
    assert_eq!(focused(&server)?, "%3");
    Ok(())
}

#[test]
fn resizing_moves_the_border_and_reaches_the_programs() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 81)?;
    client.wait_for("$")?;
    client.keys(&format!("{PREFIX}v"))?;
    eventually("two panes", || Ok(size(&server, "%2").is_ok()))?;
    // Resizing right on the right pane grows it rightward? It has no right
    // neighbour, so its left border moves right: it shrinks. Resize mode
    // repeats the key without the prefix, until Enter.
    client.keys(&format!("{PREFIX}rlllll\r"))?;
    eventually("the border moved", || Ok(size(&server, "%2")?.1 == 35))?;
    assert_eq!(size(&server, "%1")?.1, 45);
    server.ok(&["resize-pane", "-t", "%1", "-L", "10"])?;
    assert_eq!(size(&server, "%1")?.1, 35);
    // The program sees its new size.
    client.keys("stty size\r")?;
    client.wait("the size", |t| t.lines().any(|l| l.ends_with("19 45")))?;
    // Never below the minimum.
    server.ok(&["resize-pane", "-t", "%2", "-L", "200"])?;
    assert_eq!(size(&server, "%1")?.1, 2);
    let stuck = server.fux(&["resize-pane", "-t", "%1", "-U"])?;
    assert_eq!(stuck.status, 1);
    assert!(
        stuck.stderr.contains("no border to move up"),
        "{}",
        stuck.stderr
    );
    Ok(())
}

#[test]
fn zoom_fills_the_screen_and_restores() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 81)?;
    client.wait_for("$")?;
    client.keys(&format!("{PREFIX}v"))?;
    eventually("two panes", || Ok(size(&server, "%2").is_ok()))?;
    client.keys(&format!("{PREFIX}z"))?;
    eventually("zoomed", || Ok(size(&server, "%2")? == (19, 81)))?;
    client.wait("the bar says so", |t| {
        t.lines().last().is_some_and(|b| b.contains("[zoom]"))
    })?;
    assert!(
        !client.text().contains('│'),
        "no separator while zoomed:\n{}",
        client.text()
    );
    // The unfocused pane keeps its last size while no one shows it.
    assert_eq!(size(&server, "%1")?, (19, 40));
    client.keys(&format!("{PREFIX}z"))?;
    eventually("restored", || Ok(size(&server, "%2")? == (19, 40)))?;
    // Changing focus ends a zoom.
    client.keys(&format!("{PREFIX}z"))?;
    eventually("zoomed again", || Ok(size(&server, "%2")? == (19, 81)))?;
    client.keys(&format!("{PREFIX}o"))?;
    eventually("focus moved and zoom ended", || {
        Ok(focused(&server)? == "%1" && size(&server, "%2")? == (19, 40))
    })?;
    Ok(())
}

#[test]
fn the_bar_shows_workspace_tabs_pane_and_title() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    server.ok(&["rename", "-t", "+1", "work"])?;
    server.ok(&["rename", "-t", "@1", "code"])?;
    client.wait("the names", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("work") && b.contains("code"))
    })?;
    // A program's OSC 2 title replaces the pane's name in the bar.
    client.keys("printf '\\033]2;my-title\\007'\r")?;
    client.wait("the title", |t| {
        t.lines().last().is_some_and(|b| b.contains("%1 my-title"))
    })?;
    assert!(
        server
            .ok(&["ls", "--json"])?
            .contains("\"title\":\"my-title\"")
    );
    Ok(())
}

#[test]
fn moving_a_pane_by_direction_nests_it_beside_its_neighbour() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(21, 81)?;
    client.wait_for("$")?;
    // 1 | 2, then moving 2 down moves it... nowhere below: refused.
    client.keys(&format!("{PREFIX}v"))?;
    eventually("two panes", || Ok(size(&server, "%2").is_ok()))?;
    client.keys(&format!("{PREFIX}mj"))?;
    client.wait("the refusal", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("no pane down of %2"))
    })?;
    // Still in move mode: left puts 2 beside 1 on the left, 2 | 1.
    client.keys("h\r")?;
    eventually("swapped places", || {
        let ls = server.ok(&["ls"])?;
        let order: Vec<&str> = ls
            .lines()
            .filter_map(|l| l.trim_start().split(' ').next())
            .filter(|w| w.starts_with('%'))
            .collect();
        Ok(order == ["%2", "%1"])
    })?;
    // From the CLI: 2 | (1 / 3), then 2 moves right, beside 1 (the nearer
    // of 1 and 3 by centre, then by number): (1 | 2) / 3.
    server.ok(&["split", "-v", "-t", "%1"])?;
    server.ok(&["move-pane", "-t", "%2", "-R"])?;
    assert_eq!(size(&server, "%3")?.1, 81, "3 spans the width");
    assert_eq!(
        size(&server, "%1")?.0,
        size(&server, "%2")?.0,
        "1 and 2 share a row"
    );
    assert_eq!(size(&server, "%1")?.1 + size(&server, "%2")?.1 + 1, 81);
    Ok(())
}
