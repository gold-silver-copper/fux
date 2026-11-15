//! The tree is flat tables and a name is kept once (D-175): what a depth
//! of nesting and a long name cost, counted, never timed.
use super::*;
use crate::session::testing::TestResult;
#[cfg(unix)]
use fux_wire::command::Beside;
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

/// At 20,000 deep, the depth F015 was found past. It took a minute or two
/// while the journal kept with each entry every workspace that held its
/// place (F013, F016), and was run by name (D-176).
#[cfg(unix)]
#[test]
fn twenty_thousand_nested_workspaces_fit_a_small_stack() -> TestResult {
    nested_on_a_small_stack(20_000)
}

/// The pane of the tab `nest` made last, and the outermost workspace that
/// holds it.
fn innermost(s: &Session) -> Result<(PaneId, WsId), String> {
    let (ws, tab) = (s.parts.tabs_in(None).into_iter())
        .max_by_key(|(_, tab)| tab.id())
        .ok_or("no tab")?;
    let pane = *tab.root().panes().first().ok_or("no pane")?;
    Ok((pane, *s.parts.path(ws).first().ok_or("no workspace")?))
}

/// The bytes this thread holds, as its allocator counts them.
fn live() -> usize {
    fux_vt_peak::ThreadPeak.live()
}

/// F013: an event in a workspace in a thousand others holds what one at
/// the top does. A pane's program sets 4,000 titles: the journal counts
/// as many bytes for them at either depth, and the thread's allocator,
/// which counts what is really held, differs by less than the journal's
/// ring grows by at once. With each entry's chain of workspaces it was
/// 16 MiB more at the depth.
#[test]
fn a_journal_entry_holds_the_same_at_any_depth() -> TestResult {
    let (options, _) = defaults()?;
    let titles = |depth: usize| -> Result<(usize, usize), String> {
        let (mut s, _) = testing::attached(24, 80)?;
        nest(&mut s, None, depth, options)?;
        let (pane, _) = innermost(&s)?;
        let before = (s.parts.journal().bytes(), live());
        for n in 0..4_000 {
            let title = format!("\x1b]2;title {n:04}\x07");
            s.output(pane, title.as_bytes(), Instant::now());
        }
        let after = (s.parts.journal().bytes(), live());
        Ok((
            after.0.saturating_sub(before.0),
            after.1.saturating_sub(before.1),
        ))
    };
    let (shallow, deep) = (titles(1)?, titles(1_000)?);
    // What the journal counts: 4,000 entries and their titles, and the
    // rows its table has room for, which come half as many again at once.
    assert!(shallow.0 >= 4_000 * 80, "{shallow:?}");
    assert!(
        deep.0 < shallow.0.saturating_mul(3) / 2,
        "counted: {} at depth 1, {} at depth 1,000",
        shallow.0,
        deep.0
    );
    assert!(
        deep.1.abs_diff(shallow.1) < 1 << 20,
        "held: {} at depth 1, {} at depth 1,000",
        shallow.1,
        deep.1
    );
    Ok(())
}

/// Killing the outermost of N nested workspaces, a pane elsewhere left
/// running, journals each one's leaving what held it while the kill goes
/// on, N rows at once whatever the journal's room. The room those rows
/// took is given back once the events that follow let them go: with the
/// ring full of bells before and after, the thread holds no more after a
/// kill of 16,000 than of 1,000, and the ring as many bells as before. A
/// table that kept room for every row it once held kept half a megabyte
/// of it at 16,000, uncounted.
#[test]
fn what_a_deep_kill_took_of_the_journal_is_given_back_at_any_depth() -> TestResult {
    let (options, room) = defaults()?;
    let left = |depth: usize| -> Result<usize, String> {
        let (mut s, _) = testing::attached(24, 80)?;
        let bells = |s: &mut Session| {
            for _ in 0..s.parts.journal().rows().saturating_add(1) {
                s.output(PaneId::of(1), b"\x07", Instant::now());
            }
        };
        bells(&mut s);
        let (start, kept) = (live(), s.parts.journal().kept());
        nest(&mut s, None, depth, options)?;
        let (_, outermost) = innermost(&s)?;
        let kill = Command::Kill {
            target: Some(AnyRef::Workspace(outermost)),
        };
        ask(&mut s, &kill)?;
        assert_eq!(s.parts.every_workspace().len(), 1);
        bells(&mut s);
        let held = s.parts.journal().bytes();
        assert!(held <= room.get(), "{held} counted at depth {depth}");
        // No room is kept for rows gone, so as many bells fit as before.
        assert_eq!(
            s.parts.journal().kept(),
            kept,
            "bells kept at depth {depth}"
        );
        Ok(live().saturating_sub(start))
    };
    for depth in [1_000, 4_000, 16_000] {
        let left = left(depth)?;
        assert!(left < 64 << 10, "{left} bytes left held at depth {depth}");
    }
    Ok(())
}

/// A workspace in another, made and killed over a ring full of bells with
/// a client shown it, lets go about as many bells as it journals events
/// (its leaving the outer one, and the client's leaving it, are rows of
/// the journal's too): not half the ring, as when letting a bell go made
/// no room.
#[test]
fn a_workspace_closed_over_a_full_ring_lets_go_only_a_few_events() -> TestResult {
    let (options, _) = defaults()?;
    let (mut s, client) = testing::attached(24, 80)?;
    for _ in 0..s.parts.journal().rows().saturating_add(1) {
        s.output(PaneId::of(1), b"\x07", Instant::now());
    }
    let (kept, end) = (s.parts.journal().kept(), s.parts.journal().end(s.run));
    nest(&mut s, None, 2, options)?;
    let (pane, outermost) = innermost(&s)?;
    s.parts.show(client, AnyRef::Pane(pane));
    s.settle(Instant::now());
    let kill = Command::Kill {
        target: Some(AnyRef::Workspace(outermost)),
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

/// F016: making ten thousand workspaces, each in the one before, setting
/// a title in the innermost and killing the outermost, over and over,
/// leaves the thread holding no more than the journal's room allows, and
/// no more after the last time than after the third, when the ring was
/// full already. Each entry's chain of workspaces made one such round
/// 213 MiB; a chain shared between entries would be held by the one title
/// left of each round, uncounted.
fn nesting_deep_and_killing_holds_no_more_than_the_journals_room(rounds: usize) -> TestResult {
    let (options, room) = defaults()?;
    let (mut s, _) = testing::attached(24, 80)?;
    let start = live();
    let mut third = 0;
    for round in 1..=rounds {
        nest(&mut s, None, 10_000, options)?;
        let (pane, outermost) = innermost(&s)?;
        s.output(pane, b"\x1b]2;deep\x07", Instant::now());
        let kill = Command::Kill {
            target: Some(AnyRef::Workspace(outermost)),
        };
        ask(&mut s, &kill)?;
        assert_eq!(s.parts.every_workspace().len(), 1);
        assert!(s.parts.journal().bytes() <= room.get());
        if round == 3 {
            third = live();
        }
    }
    let end = live();
    // Beyond what the journal counts: the nodes of its map of moves, and
    // what the session keeps of ten thousand ids.
    assert!(
        end.saturating_sub(start) < room.get().saturating_mul(2),
        "{} bytes held more after {rounds} rounds, the journal's room {}",
        end.saturating_sub(start),
        room.get()
    );
    assert!(
        end.saturating_sub(third) < 64 << 10,
        "{third} bytes held after 3 rounds, {end} after {rounds}"
    );
    Ok(())
}

/// Fifty rounds, the ring full from the second: some seconds.
#[test]
fn fifty_rounds_of_nesting_deep_and_killing_hold_no_more_than_the_journals_room() -> TestResult {
    nesting_deep_and_killing_holds_no_more_than_the_journals_room(50)
}

/// A thousand rounds, as F016's plan asks: a minute and a half in a test
/// build, all of it the tree's making and closing of ten million
/// workspaces, so it is run by name.
#[test]
#[ignore = "a minute and a half: `cargo test -p fux-server --lib -- --ignored a_thousand_rounds`"]
fn a_thousand_rounds_of_nesting_deep_and_killing_hold_no_more_than_the_journals_room() -> TestResult
{
    nesting_deep_and_killing_holds_no_more_than_the_journals_room(1_000)
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
/// replaced leaves no file. A server taking the run back reads each name
/// from its file; with the first record it writes, the run its own, it
/// has each name in a file of its own, once, and removes the name files
/// no structure names: the last server's, and what a crash between a
/// name's writing and the structure's left.
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
    // Its panes found gone, it wrote a record: the name it read stays in
    // the file it was read from, and what no structure names is swept.
    let named = large(&kept, NAME)?;
    assert_eq!(named.len(), 1, "the name, in one file: {named:?}");
    let tab = (again.parts.tabs_in(None).first())
        .map(|(_, tab)| tab.id())
        .ok_or("no tab")?;
    let name_tab = Command::Rename {
        target: AnyRef::Tab(tab),
        name: "tab".to_owned(),
    };
    ask(&mut again, &name_tab)?;
    assert_eq!(large(&kept, NAME)?, named, "and no change writes it again");
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
    assert_eq!(names, 2, "the workspace's and the tab's");
    drop(again);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// The record in `kept`, read whole as a server taking the run back
/// reads it: its structure, and every name it has.
#[cfg(unix)]
fn read_whole(kept: &Path) -> Result<Vec<String>, String> {
    let record = crate::record::read(&kept.join("run"))?.ok_or("no record")?;
    let names = crate::record::read_names(kept, &record.tree)?;
    Ok(names.values().map(|name| name.to_string()).collect())
}

/// A server that took a run back and has not heard from a pane serves as
/// one that has (D-172): a client attaches, which takes an id the record
/// holds, and the record is written. And stopped then, it records the
/// run's end (D-174): no start takes the run back.
/// What it writes meanwhile does not grow with its changes: a long name it
/// read stays in the one file it was read from, copied by no record.
#[cfg(unix)]
#[test]
fn a_server_a_pane_has_not_answered_attaches_clients_and_records_its_end() -> TestResult {
    let (options, journal) = defaults()?;
    let dir = scratch("unsettled")?;
    let (mut first, kept) = recording(&dir, journal)?;
    nest(&mut first, None, 1, options)?;
    let pane = *first.parts.panes().keys().next().ok_or("no pane")?;
    const LONG: u64 = 1 << 18;
    let long: String = std::iter::repeat_n('n', usize::try_from(LONG)?).collect();
    let named = (first.parts.tabs_in(None).first()).map(|(ws, _)| *ws);
    let long_name = Command::Rename {
        target: AnyRef::Workspace(named.ok_or("no workspace")?),
        name: long,
    };
    ask(&mut first, &long_name)?;
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
    // Meanwhile a name it read is kept where it is, and no record copies
    // it; a name it replaces leaves no file, the last server's neither.
    let (ws, tab) = (again.parts.tabs_in(None).first())
        .map(|(ws, tab)| (*ws, tab.id()))
        .ok_or("no tab")?;
    let rename = |target: AnyRef, name: &str| Command::Rename {
        target,
        name: name.to_owned(),
    };
    ask(&mut again, &rename(AnyRef::Tab(tab), "t0"))?;
    let copies = large(&kept, LONG)?;
    assert_eq!(copies.len(), 1, "the last server's alone: {copies:?}");
    for change in 1..=5 {
        ask(&mut again, &rename(AnyRef::Tab(tab), &format!("t{change}")))?;
    }
    assert_eq!(large(&kept, LONG)?, copies, "never copied");
    let other: String = std::iter::repeat_n('m', usize::try_from(LONG)?).collect();
    ask(&mut again, &rename(AnyRef::Workspace(ws), &other))?;
    assert_eq!(large(&kept, LONG)?.len(), 1, "the old name's file went");
    assert_eq!(
        read_whole(&kept)?.len(),
        2,
        "the workspace's name and the tab's"
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
