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
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => remove_own(held, &aside, &dir)?,
        Err(e) => return Err(fux3::io(VIEW, e)),
    }
    fux3::session::private_dir(held, VIEW, false)
}

/// `workspace` as it is called now (a workspace renamed with `mv` is the
/// same one): the name and path to hand other programs. Refused once it
/// was removed.
pub fn named(workspace: &Workspace) -> Result<Workspace, Failure> {
    workspace.check()?;
    let mut now = workspace.clone();
    if let Some((_, name)) = fux3::session::place(workspace.held()) {
        (now.dir, now.name) = (workspace.session.root.join(&name), name);
    }
    Ok(now)
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
    (program, recipe): (&[String], bool),
    cancel: &AtomicBool,
) -> Result<PaneRef, Failure> {
    let workspace = &named(workspace)?;
    let mut command = Command::new(fux3);
    command
        .args(["start", &format!("{}/{name}", workspace.name)])
        .args(["--workspace-id", &workspace.id()])
        .args((!recipe).then_some("--no-recipe"))
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
    program: (&[String], bool),
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
            let live = live_panes(workspace);
            layout::resolve(&layout::read(&view)?.shown(&live), &live, target)?;
            let pane = start_new(&fux3_program()?, workspace, "shell", (&[], true), cancel)?;
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

/// Removes `name` in `dir`, held open as `own`: a directory of the
/// viewer's or the core's own that holds only files, and those files,
/// through the handle held, never by looking the name up again. A
/// directory in it is not entered: it stops there, saying which.
fn remove_own(dir: &File, name: &str, own: &File) -> Result<(), Failure> {
    for (file, _) in at::names(own, 1 << 16).map_err(|e| fux3::io(name, e))? {
        let shown = format!("{name}/{}", file.to_string_lossy());
        at::remove_file(own, &file).map_err(|e| fux3::io(shown, e))?;
    }
    at::remove_dir(dir, name).map_err(|e| fux3::io(name, e))
}

/// Closes `workspace`: ends each of its panes, waits for them to go (a
/// bounded time), and then, under the session's lock so that no pane can
/// start in it meanwhile, removes `.view` and `.recipes` (fux3's own, which
/// hold only files) and the directory it holds, emptied by that, and never
/// another that has its name. Nothing else is ever removed: a workspace
/// that holds anything but its panes and those two is refused before a
/// pane is ended, saying what is there; so is one whose pane will not end.
pub fn close_workspace(workspace: &Workspace, cancel: &AtomicBool) -> Result<(), Failure> {
    let begun = Instant::now();
    workspace.check()?;
    let own = [VIEW, fux3::recipe::DIR];
    // What the workspace holds that is neither fux3's own nor in `panes`.
    let others = |panes: &[String]| -> Result<Vec<String>, Failure> {
        let all = at::names(workspace.held(), 1 << 16).map_err(|e| fux3::io("listing", e))?;
        let names = all
            .into_iter()
            .map(|(name, _)| name.to_string_lossy().into_owned());
        Ok(names
            .filter(|n| !own.contains(&n.as_str()) && !panes.contains(n))
            .collect())
    };
    let stays = |what: String| Failure::new(format!("workspace {} stays{what}", workspace.name));
    let live = workspace.live()?;
    let other = others(&live)?;
    if !other.is_empty() {
        let names = other.join(", ");
        return Err(stays(format!(
            ", and no pane was ended: it holds {names}, which is no pane of it"
        )));
    }
    for name in live {
        // One that ended meanwhile has no reader: it is gone already.
        let _ = (workspace.pane(&name)).and_then(|dir| crate::panes::ctl_line(&dir, "kill"));
    }
    loop {
        let lock = workspace.session.lock()?;
        workspace.clear_dead(&lock)?;
        // Panes ending are still here, under names nothing lists.
        let left = others(&[])?;
        if left.is_empty() {
            for name in own {
                // One missing is no fault.
                if let Ok(dir) = at::open_dir(workspace.held(), name) {
                    remove_own(workspace.held(), name, &dir)?;
                }
            }
            let removed = || stays(": it was removed".to_owned());
            let (session, name) = fux3::session::place(workspace.held()).ok_or_else(removed)?;
            return at::remove_dir(&session, &name)
                .map_err(|e| fux3::io(format!("removing workspace {name}"), e));
        }
        // What nobody holds is no pane ending: it is what a pane left that
        // it did not make (a file of the user's, another pane inside it).
        // Looked at under the session's lock, so no start meets this lock.
        let held = |name: &&String| {
            let dir = at::open_dir(workspace.held(), name.as_str());
            dir.is_ok_and(|dir| !fux3::session::try_lock(&dir).unwrap_or(true))
        };
        let mut leftover = left.iter().find(|name| !held(name)).cloned();
        if leftover.is_some() {
            // Its holder may have gone just now: cleared again, is it still there?
            workspace.clear_dead(&lock)?;
            let still = others(&[])?;
            leftover = leftover.filter(|name| still.contains(name));
        }
        drop(lock);
        if let Some(name) = leftover {
            let why = "is left, holding what its pane did not make";
            return Err(stays(format!(": its panes were ended, and {name} {why}")));
        }
        if begun.elapsed() >= COMMAND_TIME || cancel.load(Ordering::Relaxed) {
            return Err(stays(format!(": {} did not end", left.join(", "))));
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
        let layout = layout::read(&view_dir(&second)?)?.to_string();
        assert!(
            layout.contains("theirs") && !layout.contains("stale"),
            "{layout}"
        );
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
        let layout = layout::read(&view_dir(&session.workspace("w", false)?)?)?.to_string();
        assert!(!layout.contains("late"), "{layout}");
        std::fs::remove_dir_all(&root).map_err(|e| fux3::io("removing", e))
    }

    /// Closing a workspace removes nothing but fux3's own: with anything
    /// else in it (a user's file or directory, a pane starting or ending
    /// under a name nothing lists, a live pane inside another directory) it
    /// is refused, saying what is there, before any pane is ended.
    #[test]
    fn closing_a_workspace_removes_only_what_fux3_made() -> Result<(), Failure> {
        let scratch = fuxix::file::scratch().map_err(|e| fux3::io("scratch", e))?;
        let root = scratch.join(format!("f3v-close-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        fuxix::file::private_dir(&root).map_err(|e| fux3::io("root", e))?;
        let workspace = Session::at(root.clone())?.workspace("w", true)?;
        view_dir(&workspace)?;
        // A directory with a pane's marker, locked as its holder would.
        let pane = |path: &str| -> Result<File, Failure> {
            let dir = root.join("w").join(path);
            std::fs::create_dir_all(&dir).map_err(|e| fux3::io(path, e))?;
            std::fs::write(dir.join("id"), "0123456789abcdef\n").map_err(|e| fux3::io(path, e))?;
            let held = File::open(&dir).map_err(|e| fux3::io(path, e))?;
            held.lock().map_err(|e| fux3::io(path, e))?;
            Ok(held)
        };
        // A live pane with a `ctl` someone reads: no kill may reach it.
        let live = pane("p")?;
        let ctl = root.join("w/p/ctl");
        fuxix::file::private_fifo(&ctl).map_err(|e| fux3::io("ctl", e))?;
        let reader = {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = File::options();
            options.read(true).custom_flags(fuxix::file::NONBLOCK);
            options.open(&ctl).map_err(|e| fux3::io("ctl", e))?
        };
        for (top, other) in [
            (".new-0123456789abcdef", ".new-0123456789abcdef"),
            (".gone-x", ".gone-x/inner"),
            ("notes", "notes/deep"),
        ] {
            let holder = pane(other)?;
            std::fs::write(root.join("w").join(other).join("todo"), "mine")
                .map_err(|e| fux3::io(other, e))?;
            let refused = close_workspace(&workspace, &AtomicBool::new(true)).err();
            let said = refused.map(|f| f.0).unwrap_or_default();
            assert!(
                said.contains(top) && said.contains("no pane was ended"),
                "{said:?}"
            );
            let mut sent = [0u8; 8];
            // Nothing was written to it: no bytes, or none to read.
            let told = fuxix::io::read(&reader, &mut sent).is_ok_and(|n| n > 0);
            assert!(!told, "{other}: p was told to end");
            let kept = root.join("w").join(other).join("todo");
            assert!(kept.exists(), "{other} was taken apart");
            assert!(
                root.join("w/.view/layout").exists(),
                "{other}: the layout went"
            );
            drop(holder);
            std::fs::remove_dir_all(root.join("w").join(top)).map_err(|e| fux3::io(top, e))?;
        }
        drop((live, reader));
        std::fs::remove_dir_all(root.join("w/p")).map_err(|e| fux3::io("p", e))?;
        close_workspace(&workspace, &AtomicBool::new(true))?;
        assert!(!root.join("w").exists());
        std::fs::remove_dir_all(&root).map_err(|e| fux3::io("removing", e))
    }
}
