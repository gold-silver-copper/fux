//! The tree is flat tables and a name is kept once (D-175): what a depth
//! of nesting and a long name cost, counted, never timed.
use super::*;
use crate::session::testing::TestResult;
use fux_wire::command::{Beside, Command, Program, Screen};

/// What a test's panes run, and its journal's length: read where the stack
/// is a test's own, as reading the defaults runs fux's whole script.
fn defaults() -> Result<(command::PaneOptions, command::JournalEntries), String> {
    let preferences = (fux::script::default_state().map_err(|e| e.to_string())?).preferences;
    Ok((preferences.pane_options(), preferences.journal_entries))
}

/// A directory of the test `name`'s own, new.
fn scratch(name: &str) -> Result<PathBuf, String> {
    let scratch = fuxix::file::scratch().map_err(|e| e.to_string())?;
    let dir = scratch.join(format!("fux-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// A session on a socket in `dir` that records its run there, as a
/// server's does, its panes in this process; and where it keeps the run.
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
/// there is under half a second; the minute this takes is the journal's,
/// which still keeps with each entry every workspace that held its place
/// (F013, F016), so making or closing a chain of workspaces costs it the
/// square of the depth. Run by name till the journal keeps no such chain.
#[cfg(unix)]
#[test]
#[ignore = "about a minute, all of it the journal's parent chains (F013): `cargo test -p fux-server --lib -- --ignored twenty_thousand`"]
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
/// from its file, and removes the name files no structure names: what a
/// crash between a name's writing and the structure's leaves.
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
    std::fs::write(crate::record::name_file(&kept, 77), "never recorded")?;
    drop(s);
    let record = crate::record::read(&kept.join("run"))?.ok_or("no record")?;
    let (mut again, _) = recording(&dir, journal)?;
    again.resume(record, Instant::now())?;
    assert_eq!(
        again.workspace(ws).and_then(|ws| ws.name()).map(str::len),
        Some(long.len())
    );
    assert_eq!(again.parts.panes().len(), 2);
    assert!(!crate::record::name_file(&kept, 77).exists());
    assert_eq!(large(&kept, NAME)?, named, "taken back, not written again");
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
