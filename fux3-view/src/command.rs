//! Doing things to a workspace that are no single file write: starting a
//! pane (the `fux3` program beside this one does it), knowing which panes
//! there are, running one layout command, as a viewer's grift does and as
//! `fux3-view --do` does, and closing a workspace.
use crate::layout::{self, Edited, Live, Op, PaneRef, Rect, Target};
use crate::panes::read_status;
use fux3::Failure;
use fux3::session::Workspace;
use fuxix::file::at;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The viewer's own directory in a workspace.
pub const VIEW: &str = ".view";
/// The longest one `fux3` command may take.
const COMMAND_TIME: Duration = Duration::from_secs(12);
/// The most output read from one command.
const MAX_OUTPUT: u64 = 64 << 10;
/// How many numbers a new pane's name is tried with.
const MAX_NEW_NAMES: u32 = 128;

/// The workspace's `.view`, held open. A missing one is made whole, with
/// the default layout, under a name of its own, and named by one rename
/// that replaces nothing: no other writer finds a `.view` without its
/// layout. Refused once the workspace was removed.
pub fn view_dir(workspace: &Workspace) -> Result<File, Failure> {
    workspace.check()?;
    let held = workspace.held();
    if let Ok(dir) = fux3::session::private_dir(held, VIEW, false) {
        return Ok(dir);
    }
    // A name no other maker has: this process's, and this moment's.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let nanos = now.map_or(0, |d| d.subsec_nanos());
    let aside = format!("{VIEW}.new-{}-{nanos}", std::process::id());
    at::make_dir(held, &aside).map_err(|e| fux3::io(&aside, e))?;
    let dir = at::open_dir(held, &aside).map_err(|e| fux3::io(&aside, e))?;
    layout::write_first(&dir)?;
    match at::rename_new(held, &aside, held, VIEW) {
        Ok(()) => {}
        // Another got there first: theirs is the one, and ours goes.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            empty(&dir, 0)?;
            at::remove_dir(held, &aside).map_err(|e| fux3::io(&aside, e))?;
        }
        Err(e) => return Err(fux3::io(VIEW, e)),
    }
    fux3::session::private_dir(held, VIEW, false)
}

/// The `fux3` program beside this one.
pub fn fux3_program() -> Result<PathBuf, Failure> {
    let exe = std::env::current_exe().map_err(|e| fux3::io("finding this program", e))?;
    Ok(exe.with_file_name("fux3"))
}

/// The panes there are, as routing needs them: each pane directory whose
/// `status` says which life it is.
pub fn live_panes(workspace: &Workspace) -> Vec<Live> {
    let names = workspace.names().unwrap_or_default();
    (names.iter())
        .filter_map(|name| {
            let mut pane = crate::panes::Pane::new(name, workspace.pane(name).ok()?);
            pane.status = read_status(&pane.dir).ok()?;
            pane.live()
        })
        .collect()
}

/// Starts pane `name` in `workspace` running `program` (the user's shell
/// if none), with the `fux3` beside this program: the pane as this one
/// life of it, in the directory `workspace` holds and no other of its
/// name (`--workspace-id`). Running out of time, or `cancel`, ends the
/// `fux3` that starts it.
pub fn start(
    fux3: &Path,
    workspace: &Workspace,
    name: &str,
    program: &[String],
    cancel: &AtomicBool,
) -> Result<PaneRef, Failure> {
    let mut command = Command::new(fux3);
    command
        .args(["start", &format!("{}/{name}", workspace.name)])
        .args(["--workspace-id", &workspace.id()])
        .env("FUX3_DIR", &workspace.session.root);
    // Named even for the shell, so a reused name never runs a recipe.
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    command.arg("--");
    match program.is_empty() {
        true => command.arg(shell),
        false => command.args(program),
    };
    let mut child = (command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn())
    .map_err(|e| fux3::io(fux3.display(), e))?;
    let begun = Instant::now();
    // Its one line of error fits the pipe: it is read once it has ended.
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| fux3::io(fux3.display(), e))? {
            break status;
        }
        let late = begun.elapsed() >= COMMAND_TIME;
        if late || cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failure::new(if late {
                "fux3 start took too long"
            } else {
                "stopped"
            }));
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    if !status.success() {
        let mut said = String::new();
        if let Some(err) = child.stderr.take() {
            let _ = err.take(MAX_OUTPUT).read_to_string(&mut said);
        }
        return Err(Failure::new(said.trim().trim_start_matches("fux3: ")));
    }
    // `fux3 start` writes the pane's `status` before it says it started.
    let status = (workspace.pane(name).and_then(|dir| read_status(&dir)))
        .map_err(|failure| Failure::new(format!("pane {name} started, but: {failure}")))?;
    let id = fux3::files::status_value(&status, "id").filter(|id| layout::is_id(id));
    let id =
        id.ok_or_else(|| Failure::new(format!("pane {name} started, but its status names no id")))?;
    Ok(PaneRef {
        name: name.to_owned(),
        id: id.to_owned(),
    })
}

/// Starts a pane named `PREFIX-N` in `workspace`, the first N free.
pub fn start_new(
    fux3: &Path,
    workspace: &Workspace,
    prefix: &str,
    program: &[String],
    cancel: &AtomicBool,
) -> Result<PaneRef, Failure> {
    // A name a tile still shows (a pane gone) is not reused either.
    let mut taken = workspace.names().unwrap_or_default();
    if let Ok(layout) = view_dir(workspace).and_then(|view| layout::read(&view)) {
        taken.extend(
            (layout.tabs.iter())
                .flat_map(|t| t.leaves())
                .map(|p| p.name.clone()),
        );
    }
    for number in 1..=MAX_NEW_NAMES {
        let name = format!("{prefix}-{number}");
        if taken.contains(&name) {
            continue;
        }
        match start(fux3, workspace, &name, program, cancel) {
            Err(failure) if failure.0.contains("exists") => {}
            outcome => return outcome,
        }
    }
    Err(Failure::new(format!("no free name {prefix}-N")))
}

/// Runs one layout command on the workspace's layout file, as seen in a
/// screen of `area`; `split PANE DIR` starts its shell first.
pub fn run_line(
    workspace: &Workspace,
    line: &str,
    area: Rect,
    cancel: &AtomicBool,
) -> Result<Edited, Failure> {
    let command = layout::parse_command(line)?;
    let view = view_dir(workspace)?;
    let (op, started) = if let Op::SplitNew(target, dir) = &command {
        {
            // The pane split must have a tile before a shell is started for it.
            let shown = layout::read(&view)?.shown(&live_panes(workspace));
            if !(shown.tabs.iter())
                .flat_map(|t| t.leaves())
                .any(|p| p.name == target.name)
            {
                return Err(Failure::new(format!("no tile for pane {target}")));
            }
            let pane = start_new(&fux3_program()?, workspace, "shell", &[], cancel)?;
            let new = Target {
                name: pane.name.clone(),
                id: Some(pane.id.clone()),
            };
            let (target, dir) = (target.clone(), *dir);
            (Op::Split(target, new, dir), Some(pane))
        }
    } else {
        (command.clone(), None)
    };
    // One removed while this waited for the lock is said, not its lost file.
    let edited = layout::edit(&view, &live_panes(workspace), area, &op)
        .map_err(|failure| workspace.check().err().unwrap_or(failure));
    match (edited, started) {
        (Ok(edited), started) => Ok(Edited { started, ..edited }),
        (Err(failure), Some(pane)) => Err(Failure::new(format!(
            "{pane} is running but has no tile: {failure}"
        ))),
        (Err(failure), None) => Err(failure),
    }
}

/// Removes everything in `dir`, through its handle: files and links
/// themselves, directories emptied first, `depth` levels down at most.
fn empty(dir: &File, depth: u8) -> Result<(), Failure> {
    for (name, _) in at::names(dir, 1 << 16).map_err(|e| fux3::io("listing", e))? {
        let shown = name.to_string_lossy().into_owned();
        if at::remove_file(dir, &name).is_ok() {
            continue;
        }
        let inner = at::open_dir(dir, &name).map_err(|e| fux3::io(&shown, e))?;
        match depth.checked_sub(1) {
            Some(depth) => empty(&inner, depth)?,
            None => return Err(Failure::new(format!("{shown} is too deep to remove"))),
        }
        at::remove_dir(dir, &name).map_err(|e| fux3::io(&shown, e))?;
    }
    Ok(())
}

/// Closes `workspace`: ends each of its panes, waits for them to go (a
/// bounded time), and then, under the session's lock so that no pane can
/// start in it meanwhile, removes the directory it holds, layout and all,
/// and never another that has its name. A pane that will not end is
/// refused out loud and the workspace stays.
pub fn close_workspace(workspace: &Workspace, cancel: &AtomicBool) -> Result<(), Failure> {
    let begun = Instant::now();
    workspace.check()?;
    for name in workspace.live()? {
        // One that ended meanwhile has no reader: it is gone already.
        let _ = (workspace.pane(&name)).and_then(|dir| crate::panes::ctl_line(&dir, "kill"));
    }
    loop {
        let lock = workspace.session.lock()?;
        let live = workspace.clear_dead(&lock)?;
        if live.is_empty() {
            empty(workspace.held(), 4)?;
            let removed = || Failure::new(format!("workspace {} was removed", workspace.name));
            let (session, name) = fux3::session::place(workspace.held()).ok_or_else(removed)?;
            return at::remove_dir(&session, &name)
                .map_err(|e| fux3::io(format!("removing workspace {name}"), e));
        }
        drop(lock);
        if begun.elapsed() >= COMMAND_TIME || cancel.load(Ordering::Relaxed) {
            let names = live.join(", ");
            return Err(Failure::new(format!(
                "workspace {} stays: {names} did not end",
                workspace.name
            )));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fux3::session::Session;

    /// A workspace removed and made again is another: a layout command that
    /// began on the first is refused, saying so, and the second's layout is
    /// as it was.
    #[test]
    fn a_command_for_a_removed_workspace_never_lands_on_its_successor() -> Result<(), Failure> {
        let scratch = fuxix::file::scratch().map_err(|e| fux3::io("scratch", e))?;
        let root = scratch.join(format!("f3v-wsid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        fuxix::file::private_dir(&root).map_err(|e| fux3::io("root", e))?;
        let session = Session::at(root.clone())?;
        let (area, cancel) = (Rect::default(), AtomicBool::new(false));
        let first = session.workspace("w", true)?;
        run_line(&first, "tab new kept", area, &cancel)?;
        std::fs::remove_dir_all(root.join("w")).map_err(|e| fux3::io("removing", e))?;
        let second = session.workspace("w", true)?;
        run_line(&second, "tab new theirs", area, &cancel)?;
        for line in ["tab new stale", "split nosuch right", "reset"] {
            let refused = run_line(&first, line, area, &cancel).err();
            assert_eq!(
                refused.map(|f| f.0).as_deref(),
                Some("workspace w was removed")
            );
        }
        // One that waited for the layout's lock meanwhile says the same.
        let third = session.workspace("w", false)?;
        let lock = layout::lock(&view_dir(&third)?)?;
        let late = std::thread::scope(|scope| {
            let waiting = scope.spawn(|| run_line(&third, "tab new late", area, &cancel));
            std::thread::sleep(Duration::from_millis(200));
            let removed = std::fs::remove_dir_all(root.join("w"));
            let second = removed.map(|()| session.workspace("w", true));
            drop(lock);
            (waiting.join(), second.is_ok_and(|w| w.is_ok()))
        });
        let (Ok(Err(late)), true) = late else {
            return Err(Failure::new("the late command was not refused"));
        };
        assert_eq!(late.0, "workspace w was removed");
        let second = session.workspace("w", false)?;
        run_line(&second, "tab new theirs", area, &cancel)?;
        let layout = layout::read(&view_dir(&second)?)?.to_string();
        assert!(
            layout.contains("theirs") && !layout.contains("stale"),
            "{layout}"
        );
        std::fs::remove_dir_all(&root).map_err(|e| fux3::io("removing", e))
    }
}
