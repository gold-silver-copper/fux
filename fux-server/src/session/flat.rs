//! The tree is flat tables, written down whole: what a run's record keeps,
//! and what the journal lets go.
use super::*;
use crate::session::testing::TestResult;
use fux_wire::command::{Command, Program, Screen};

/// What a test's panes run, and its journal's room: read where the stack
/// is a test's own, as reading the defaults runs fux's whole script.
fn defaults() -> Result<(command::PaneOptions, command::JournalBytes), String> {
    let preferences = (fux::script::default_state().map_err(|e| e.to_string())?).preferences;
    Ok((preferences.pane_options(), preferences.journal_bytes))
}

/// A directory of the test `name`'s own, new.
#[cfg(unix)]
fn scratch(name: &str) -> Result<PathBuf, String> {
    let scratch = fuxix::file::scratch().map_err(|e| e.to_string())?;
    let dir = scratch.join(format!("fux-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// A session on a socket in `dir` that records its run there, as a
/// server's does, its panes in this process; and where it keeps the run.
#[cfg(unix)]
fn recording(dir: &Path, journal: command::JournalBytes) -> Result<(Session, PathBuf), String> {
    let socket = dir.join("fux.sock");
    let kept = crate::socket::panes(&socket);
    std::fs::create_dir_all(&kept).map_err(|e| e.to_string())?;
    let run = command::Run::now().ok_or("no clock")?;
    let mut session = Session::new(socket, true, run, journal);
    // No fux to run a pane's own process: each pane is in this one.
    session.exe = None;
    Ok((session, kept))
}

/// `command`, asked of `s` from outside it.
fn ask(s: &mut Session, command: &Command) -> Result<(), String> {
    let asked = s.execute(command, &Origin::Cli(Caller::default()), Instant::now());
    s.settle(Instant::now());
    asked.map(drop).map_err(|e| e.to_string())
}

/// A tab of `cat` in a new workspace.
fn workspace_of_cat(s: &mut Session, options: command::PaneOptions) -> Result<(), String> {
    let made = Command::NewTab {
        target: Some(WsRef::New),
        name: None,
        size: Some(Screen::new(24, 80).map_err(|e| e.to_string())?),
        cwd: Some(std::env::current_dir().map_err(|e| e.to_string())?),
        start: command::Start::Program(Program::new("cat".into(), Vec::new())?),
        options,
    };
    ask(s, &made)
}

/// The pane of the tab made last, and its workspace.
fn newest(s: &Session) -> Result<(PaneId, WsId), String> {
    let (ws, tab) = (s.parts.tabs_in(None).into_iter())
        .max_by_key(|(_, tab)| tab.id())
        .ok_or("no tab")?;
    Ok((*tab.root().panes().first().ok_or("no pane")?, ws))
}

/// A workspace made and killed over a ring full of bells with a client
/// shown it lets go about as many bells as it journals events: not half
/// the ring, as when letting a bell go made no room.
#[test]
fn a_workspace_closed_over_a_full_ring_lets_go_only_a_few_events() -> TestResult {
    let (options, _) = defaults()?;
    let (mut s, client) = testing::attached(24, 80)?;
    for _ in 0..s.parts.journal().rows().saturating_add(1) {
        s.output(PaneId::of(1), b"\x07", Instant::now());
    }
    let (kept, end) = (s.parts.journal().kept(), s.parts.journal().end(s.run));
    workspace_of_cat(&mut s, options)?;
    let (pane, ws) = newest(&s)?;
    s.parts.show(client, AnyRef::Pane(pane));
    s.settle(Instant::now());
    let kill = Command::Kill {
        target: Some(AnyRef::Workspace(ws)),
    };
    ask(&mut s, &kill)?;
    let journaled = (s.parts.journal().end(s.run).seq.0).saturating_sub(end.seq.0);
    assert!(journaled > 0 && journaled < 64, "{journaled} events");
    let now = s.parts.journal().kept();
    assert!(
        now.saturating_add(8) >= kept,
        "{kept} entries kept before, {now} after {journaled} events"
    );
    Ok(())
}

/// A record whose tab is zoomed on a pane that is not in that tab is no
/// tree, and is refused: restored as it is, focus there would name a
/// pane of another tab.
#[cfg(unix)]
#[test]
fn a_record_zoomed_on_a_pane_of_another_tab_is_refused() -> TestResult {
    let (options, journal) = defaults()?;
    let dir = scratch("zoomed")?;
    let (mut s, _) = recording(&dir, journal)?;
    workspace_of_cat(&mut s, options)?;
    workspace_of_cat(&mut s, options)?;
    let restore = |tree: &crate::record::Tree| {
        let copies = (tree.panes().into_iter())
            .map(|pane| (pane.id, Pane::recorded(pane)))
            .collect();
        Parts::restore(journal, tree, copies).map(drop)
    };
    let mut tree = s.parts.record();
    assert!(restore(&tree).is_ok(), "the record as it was written");
    let other = (tree.tabs.get(1).and_then(|tab| tab.panes.first())).map(|pane| pane.id);
    let first = tree.tabs.first_mut().ok_or("no tab")?;
    first.zoomed = Some(other.ok_or("no second tab")?);
    assert!(restore(&tree).is_err(), "zoomed on another tab's pane");
    Ok(())
}

/// A server that took a run back and has not heard from a pane serves as
/// one that has (D-172): a client attaches, which takes an id the record
/// holds, and the record is written. And stopped then, it records the
/// run's end (D-174): no start takes the run back. A name it read is
/// the name its workspace has.
#[cfg(unix)]
#[test]
fn a_server_a_pane_has_not_answered_attaches_clients_and_records_its_end() -> TestResult {
    let (options, journal) = defaults()?;
    let dir = scratch("unsettled")?;
    let (mut first, kept) = recording(&dir, journal)?;
    workspace_of_cat(&mut first, options)?;
    let pane = *first.parts.panes().keys().next().ok_or("no pane")?;
    let named = (first.parts.tabs_in(None).first()).map(|(ws, _)| *ws);
    let named = named.ok_or("no workspace")?;
    let name = Command::Rename {
        target: AnyRef::Workspace(named),
        name: "kept".to_owned(),
    };
    ask(&mut first, &name)?;
    let _silent = std::os::unix::net::UnixListener::bind(kept.join(pane.number().to_string()))?;
    drop(first);
    let record = crate::record::read(&kept.join("run"))?.ok_or("no record")?;
    let before = record.ids.clone();
    let (mut again, _) = recording(&dir, journal)?;
    again.resume(record, Instant::now())?;
    assert!(!again.awaited.is_empty(), "a pane has not answered");
    again.attach(24, 80, None)?;
    again.settle(Instant::now());
    let record = crate::record::read(&kept.join("run"))?.ok_or("no record")?;
    assert_ne!(record.ids, before, "the client's id is recorded");
    assert!(!record.ended);
    assert_eq!(
        again.workspace(named).and_then(|ws| ws.name()),
        Some("kept")
    );
    again.end_run();
    let record = crate::record::read(&kept.join("run"))?.ok_or("no record")?;
    assert!(record.ended, "its end is recorded");
    drop(again);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// The record is made again only once the tree was handed out to change:
/// settling with nothing new, a request that only reads and a key typed
/// make none; a split makes one.
#[cfg(unix)]
#[test]
fn the_record_is_made_only_when_the_tree_may_have_changed() -> TestResult {
    let (options, journal) = defaults()?;
    let dir = scratch("made")?;
    let (mut s, _) = recording(&dir, journal)?;
    workspace_of_cat(&mut s, options)?;
    let pane = *s.parts.panes().keys().next().ok_or("no pane")?;
    let client = s.attach(24, 80, None)?;
    s.settle(Instant::now());
    let made = s.encoded;
    for _ in 0..3 {
        s.settle(Instant::now());
    }
    s.input(client, b"x");
    s.settle(Instant::now());
    assert_eq!(s.encoded, made, "nothing changed the tree");
    let split = Command::Split {
        axis: Axis::Horizontal,
        shares: None,
        target: pane,
        start: command::Start::Program(Program::new("cat".into(), Vec::new())?),
        cwd: Some(dir.clone()),
        options,
    };
    ask(&mut s, &split)?;
    assert!(s.encoded > made);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
