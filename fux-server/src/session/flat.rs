//! The tree is flat tables and a name is kept once (D-175): what a depth
//! of nesting and a long name cost, counted, never timed.
use super::*;
use crate::session::testing::TestResult;
#[cfg(unix)]
use fux_wire::command::Beside;
use fux_wire::command::{Command, Program, Screen};

/// What a test's panes run, and its journal's length: read where the stack
/// is a test's own, as reading the defaults runs fux's whole script.
fn defaults() -> Result<(command::PaneOptions, command::JournalEntries), String> {
    let preferences = (fux::script::default_state().map_err(|e| e.to_string())?).preferences;
    Ok((preferences.pane_options(), preferences.journal_entries))
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
fn recording(dir: &Path, journal: command::JournalEntries) -> Result<(Session, PathBuf), String> {
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

/// A tab of `cat` in `depth` new workspaces, each in the one before, the
/// first in `within` (the top with none).
fn nest(
    s: &mut Session,
    within: Option<WsId>,
    depth: usize,
    options: command::PaneOptions,
) -> Result<(), String> {
    let target = (within.map(WsRef::Id).into_iter())
        .chain(std::iter::repeat_n(WsRef::New, depth))
        .collect();
    let nested = Command::NewTab {
        target,
        name: None,
        size: Some(Screen::new(24, 80).map_err(|e| e.to_string())?),
        cwd: Some(std::env::current_dir().map_err(|e| e.to_string())?),
        start: command::Start::Program(Program::new("cat".into(), Vec::new())?),
        options,
    };
    ask(s, &nested)
}

/// F011, F015: `depth` workspaces, each in the one before, are made by
/// one request, recorded, taken back by another session from what the
/// first wrote, and closed by killing the outermost, on a stack of
/// 256 KiB. One walk that went into a workspace's workspaces, in the
/// session, its record's encoding or decoding, or the drop of any of them,
/// would end the thread.
#[cfg(unix)]
fn nested_on_a_small_stack(depth: usize) -> TestResult {
    let (options, journal) = defaults()?;
    let dir = scratch(&format!("deep-{depth}"))?;
    let at = dir.clone();
    let deep = move || -> Result<(), String> {
        let (mut first, kept) = recording(&at, journal)?;
        nest(&mut first, None, depth, options)?;
        let innermost = (first.parts.tabs_in(None).first())
            .map(|(ws, _)| *ws)
            .ok_or("no tab")?;
        assert_eq!(first.parts.path(innermost).len(), depth);
        let outermost = *first.parts.path(innermost).first().ok_or("no workspace")?;
        // Its server gone, as a crash leaves it: the record is what is left.
        drop(first);
        let record = crate::record::read(&kept.join("run"))?.ok_or("no record")?;
        assert_eq!(record.tree.workspaces.len(), depth);
        let (mut again, _) = recording(&at, journal)?;
        again
            .resume(record, Instant::now())
            .map_err(|e| e.to_string())?;
        assert_eq!(again.parts.path(innermost).len(), depth);
        let kill = Command::Kill {
            target: Some(AnyRef::Workspace(outermost)),
        };
        ask(&mut again, &kill)?;
        assert!(again.parts.every_workspace().is_empty());
        Ok(())
    };
    let thread = std::thread::Builder::new().stack_size(256 << 10);
    let done = thread.spawn(deep)?.join();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(done.map_err(|_| "the thread ended: its stack, most likely")??)
}

/// At 2,000 deep: master's walks, which went into each workspace's
/// workspaces, filled 8 MiB at 11,000 (F015), so 256 KiB near 350.
#[cfg(unix)]
#[test]
fn two_thousand_nested_workspaces_fit_a_small_stack() -> TestResult {
    nested_on_a_small_stack(2_000)
}

/// At 20,000 deep, the depth F015 was found past. The tree's own work
/// there is under half a second; the minute or two this takes is the journal's,
/// which still keeps with each entry every workspace that held its place
/// (F013, F016), so making or closing a chain of workspaces costs it the
/// square of the depth. Run by name till the journal keeps no such chain.
#[cfg(unix)]
#[test]
#[ignore = "a minute or two, all of it the journal's parent chains (F013): `cargo test -p fux-server --lib -- --ignored twenty_thousand`"]
fn twenty_thousand_nested_workspaces_fit_a_small_stack() -> TestResult {
    nested_on_a_small_stack(20_000)
}

/// F011: what the tree reads to make workspaces nested twice as deep, show
/// a client the innermost and kill the outermost is about twice as much,
/// not four or eight times: each row read going up or down is counted
/// (`Parts::steps`), and none is read again for each level.
#[test]
fn the_work_of_nesting_and_killing_grows_with_the_depth_alone() -> TestResult {
    let (options, _) = defaults()?;
    let steps = |depth: usize| -> Result<u64, String> {
        let (mut s, client) = testing::attached(24, 80)?;
        s.parts.steps.set(0);
        nest(&mut s, None, depth, options)?;
        let (innermost, tab) = (s.parts.tabs_in(None).last())
            .map(|(ws, tab)| (*ws, tab.id()))
            .ok_or("no tab")?;
        let outermost = *s.parts.path(innermost).first().ok_or("no workspace")?;
        s.parts.show(client, AnyRef::Tab(tab));
        s.settle(Instant::now());
        assert_eq!(s.place(client).map(|place| place.ws), Some(innermost));
        let kill = Command::Kill {
            target: Some(AnyRef::Workspace(outermost)),
        };
        ask(&mut s, &kill)?;
        assert_eq!(s.parts.every_workspace().len(), 1);
        Ok(s.parts.steps.get())
    };
    let (once, twice) = (steps(400)?, steps(800)?);
    assert!(
        once > 400 && twice < once.saturating_mul(3),
        "{once} rows read at depth 400, {twice} at 800"
    );
    Ok(())
}

/// A tree of `depth` workspaces, each in the one before, with a tab of one
/// pane in every one, none of them running anything; a client attached,
/// shown the innermost.
fn chain_of_tabs(depth: u32, client: ClientId) -> Result<Parts, String> {
    let (options, journal) = defaults()?;
    let size = Size::new(24, 80).map_err(|e| e.to_string())?;
    let pane = |id: u32| -> Result<Pane, String> {
        let recorded = crate::record::Pane {
            id: PaneId::of(id),
            name: None,
            shell: false,
            argv: Program::new("cat".into(), Vec::new())?,
            cwd: "/".into(),
            options,
            size,
            respawns: 0,
            ended: None,
            read_error: None,
        };
        Ok(Pane::recorded(&recorded, None))
    };
    let mut parts = Parts::new(journal);
    for level in 1..=depth {
        let parent = level.checked_sub(1).filter(|above| *above > 0);
        let made = vec![(WsId::of(level), None)];
        let first = (TabId::of(level), None, Fit::Clients(size));
        (parts.add_workspaces(parent.map(WsId::of), made, first, pane(level)?))
            .map_err(|_| "no such parent")?;
    }
    parts.attach(client, AnyRef::Tab(TabId::of(depth)));
    Ok(parts)
}

/// F011, with a tab in every workspace of the chain: killing the outermost
/// reads about twice the rows at twice the depth. Each tab's close once
/// went up through every workspace still there to count it, and compared
/// what each client is shown, the whole tree read, before and after it:
/// the square of the depth, whatever the journal costs.
#[test]
fn killing_a_chain_with_a_tab_at_every_level_grows_with_the_depth_alone() -> TestResult {
    let client = ClientId::of(1);
    let steps = |depth: u32| -> Result<u64, String> {
        let mut parts = chain_of_tabs(depth, client)?;
        assert_eq!(
            parts.place(client).map(|((ws, _), _)| ws),
            Some(WsId::of(depth))
        );
        parts.steps.set(0);
        let closed = parts.remove_workspace(Some(WsId::of(1)));
        assert_eq!(
            closed.len(),
            usize::try_from(depth).map_err(|e| e.to_string())?
        );
        assert!(parts.every_workspace().is_empty() && parts.place(client).is_none());
        Ok(parts.steps.get())
    };
    let (once, twice) = (steps(200)?, steps(400)?);
    assert!(
        once > 200 && twice < once.saturating_mul(3),
        "{once} rows read at depth 200, {twice} at 400"
    );
    Ok(())
}

/// The files in `kept` that hold `bytes` bytes or more, each with what
/// tells it from one written anew in its place.
#[cfg(unix)]
fn large(kept: &Path, bytes: u64) -> Result<Vec<(std::ffi::OsString, u64)>, String> {
    use std::os::unix::fs::MetadataExt;
    let mut large = Vec::new();
    for entry in std::fs::read_dir(kept).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let found = entry.metadata().map_err(|e| e.to_string())?;
        if found.len() >= bytes {
            large.push((entry.file_name(), found.ino()));
        }
    }
    Ok(large)
}

/// F014: a name is written once, to a file of its own, and no change of
/// the tree writes it again: with a workspace named in 1 MiB, a split
/// writes the structure alone, in fewer bytes than that one name. A name
/// replaced leaves no file, a server taking the run back reads each name
/// from its file, and removes the name files no structure names, what a
/// crash between a name's writing and the structure's leaves, with the
/// first record it writes.
#[cfg(unix)]
#[test]
fn a_long_name_is_written_once_and_no_split_writes_it_again() -> TestResult {
    const NAME: u64 = 1 << 20;
    let (options, journal) = defaults()?;
    let dir = scratch("names")?;
    let (mut s, kept) = recording(&dir, journal)?;
    nest(&mut s, None, 1, options)?;
    let (ws, pane) = (s.parts.tabs_in(None).first())
        .map(|(ws, tab)| (*ws, tab.root().first_pane()))
        .ok_or("no tab")?;
    let long: String = std::iter::repeat_n('n', usize::try_from(NAME)?).collect();
    let rename = |name: &str| Command::Rename {
        target: AnyRef::Workspace(ws),
        name: name.to_owned(),
    };
    ask(&mut s, &rename(&long))?;
    let named = large(&kept, NAME)?;
    assert_eq!(named.len(), 1, "the name, in one file: {named:?}");
    let split = Command::Split {
        beside: Beside::Split(Axis::Horizontal, None),
        target: pane,
        start: command::Start::Program(Program::new("cat".into(), Vec::new())?),
        cwd: Some(dir.clone()),
        options,
    };
    let before = std::fs::read(kept.join("run"))?;
    ask(&mut s, &split)?;
    let after = std::fs::read(kept.join("run"))?;
    assert_ne!(before, after, "the split is recorded");
    assert!(u64::try_from(after.len())? < NAME / 64, "{}", after.len());
    assert_eq!(large(&kept, NAME)?, named, "the name's file is as it was");
    // What a crash between a name's writing and the structure's leaves:
    // a name no structure names, beside the run as it was.
    let left = crate::record::NameId {
        server: 7,
        number: 77,
    };
    std::fs::write(crate::record::name_file(&kept, left), "never recorded")?;
    drop(s);
    let record = crate::record::read(&kept.join("run"))?.ok_or("no record")?;
    let (mut again, _) = recording(&dir, journal)?;
    again.resume(record, Instant::now())?;
    assert_eq!(
        again.workspace(ws).and_then(|ws| ws.name()).map(str::len),
        Some(long.len())
    );
    assert_eq!(again.parts.panes().len(), 2);
    assert_eq!(large(&kept, NAME)?, named, "taken back, not written again");
    // What the crash left went with the first record written here: its
    // panes, in the first session's process, were found gone, and so
    // recorded. (A server that finds them all there writes none till
    // something changes: `tests/pane_processes.rs`.)
    assert!(!crate::record::name_file(&kept, left).exists());
    // Named anew: the old name's file goes once the structure names the new.
    ask(&mut again, &rename("short"))?;
    assert_eq!(large(&kept, NAME)?, []);
    let names = std::fs::read_dir(&kept)?
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("name."))
        .count();
    assert_eq!(names, 1);
    drop(again);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Two servers on one run (D-171: the first's lock file removed under it)
/// write and remove no name file of each other's. The second takes the
/// run back while a pane has not answered it, so it does not know yet
/// that the run is its own; the first, alive, then gives a name, and the
/// second, asked for one too, writes its record. The first's new name is
/// there still, as it wrote it, and so is the second's, in a file of its
/// own number; and the first's next record is one a third server reads,
/// names and all. Once the second's sweep took the first's file, and its
/// next number was the first's too: the run could not be taken back.
#[cfg(unix)]
#[test]
fn a_second_server_on_a_run_removes_and_overwrites_no_name_of_the_firsts() -> TestResult {
    let (options, journal) = defaults()?;
    let dir = scratch("two")?;
    let (mut first, kept) = recording(&dir, journal)?;
    nest(&mut first, None, 1, options)?;
    let (ws, tab, pane) = (first.parts.tabs_in(None).first())
        .map(|(ws, tab)| (*ws, tab.id(), tab.root().first_pane()))
        .ok_or("no tab")?;
    let rename = |target: AnyRef, name: &str| Command::Rename {
        target,
        name: name.to_owned(),
    };
    ask(&mut first, &rename(AnyRef::Workspace(ws), "one"))?;
    // The pane's own process, as the second finds it: it listens, and has
    // not answered.
    let socket = kept.join(pane.number().to_string());
    let _silent = std::os::unix::net::UnixListener::bind(&socket)?;
    let record = crate::record::read(&kept.join("run"))?.ok_or("no record")?;
    let (mut second, _) = recording(&dir, journal)?;
    second.names_as = first.names_as.wrapping_add(1);
    second.resume(record, Instant::now())?;
    assert!(!second.owns(), "a pane has not answered");
    // The first names its workspace anew, after the second read the record.
    ask(&mut first, &rename(AnyRef::Workspace(ws), "three"))?;
    let named = |s: &Session, of: AnyRef| {
        let (id, _) = s.named.get(&of).ok_or("no such name")?;
        Ok::<_, String>(crate::record::name_file(&kept, *id))
    };
    let firsts = named(&first, AnyRef::Workspace(ws))?;
    // The second writes a record, with a name of its own.
    ask(&mut second, &rename(AnyRef::Tab(tab), "two"))?;
    let seconds = named(&second, AnyRef::Tab(tab))?;
    assert_ne!(firsts, seconds, "a file each");
    assert_eq!(std::fs::read_to_string(&firsts)?, "three");
    assert_eq!(std::fs::read_to_string(&seconds)?, "two");
    // The second leaves, as a pane would tell it to. The first goes on:
    // its next record is the run's, and a server reads it whole.
    drop(second);
    ask(&mut first, &rename(AnyRef::Tab(tab), "four"))?;
    drop(first);
    let record = crate::record::read(&kept.join("run"))?.ok_or("no record")?;
    let (mut third, _) = recording(&dir, journal)?;
    third.resume(record, Instant::now())?;
    assert_eq!(third.workspace(ws).and_then(|ws| ws.name()), Some("three"));
    let tab_name = third.parts.tab(tab).and_then(|(_, tab)| tab.name());
    assert_eq!(tab_name, Some("four"));
    drop(third);
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
    nest(&mut s, None, 1, options)?;
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
        beside: Beside::Split(Axis::Horizontal, None),
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
