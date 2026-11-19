//! Doing things to the session that are no single file write: starting a
//! pane (the `fux3` program beside this one does it), knowing which panes
//! there are, and running one layout command, as a viewer's grift does
//! and as `fux3-view --do` does.
use crate::layout::{self, Edited, Live, Op, PaneRef, Rect, Target};
use crate::panes::read_status;
use fux3::Failure;
use fux3::session::Session;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The viewer's own directory in the session.
pub const VIEW: &str = ".view";
/// The longest one `fux3` command may take.
const COMMAND_TIME: Duration = Duration::from_secs(12);
/// The most output read from one command.
const MAX_OUTPUT: u64 = 64 << 10;
/// How many numbers a new pane's name is tried with.
const MAX_NEW_NAMES: u32 = 128;

/// The session's `.view`, made private if missing.
pub fn view_dir(session: &Session) -> Result<PathBuf, Failure> {
    let dir = session.root.join(VIEW);
    match fuxix::file::private_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(fux3::io(dir.display(), e)),
    }
    fux3::session::check_private(&dir)?;
    Ok(dir)
}

/// The `fux3` program beside this one.
pub fn fux3_program() -> Result<PathBuf, Failure> {
    let exe = std::env::current_exe().map_err(|e| fux3::io("finding this program", e))?;
    Ok(exe.with_file_name("fux3"))
}

/// The panes there are, as routing needs them: each pane directory whose
/// `status` says which life it is.
pub fn live_panes(session: &Session) -> Vec<Live> {
    let names = session.names().unwrap_or_default();
    (names.iter())
        .filter_map(|name| {
            let mut pane = crate::panes::Pane::new(name, session.pane_dir(name));
            pane.status = read_status(&pane.dir).ok()?;
            pane.live()
        })
        .collect()
}

/// Starts pane `name` running `program` (the user's shell if none), with
/// the `fux3` beside this program: the pane as this one life of it.
/// Running out of time, or `cancel`, ends the `fux3` that starts it.
pub fn start(
    fux3: &Path,
    session: &Session,
    name: &str,
    program: &[String],
    cancel: &AtomicBool,
) -> Result<PaneRef, Failure> {
    let mut command = Command::new(fux3);
    command.args(["start", name]).env("FUX3_DIR", &session.root);
    if !program.is_empty() {
        command.arg("--").args(program);
    }
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
    let status = read_status(&session.pane_dir(name))?;
    let id = fux3::files::status_value(&status, "id").filter(|id| layout::is_id(id));
    let id =
        id.ok_or_else(|| Failure::new(format!("pane {name} started, but its status names no id")))?;
    Ok(PaneRef {
        name: name.to_owned(),
        id: id.to_owned(),
    })
}

/// Starts a pane named `PREFIX-N`, the first N free.
pub fn start_new(
    fux3: &Path,
    session: &Session,
    prefix: &str,
    program: &[String],
    cancel: &AtomicBool,
) -> Result<PaneRef, Failure> {
    // A name a tile still shows (a pane gone) is not reused either.
    let mut taken = session.names().unwrap_or_default();
    if let Ok(layout) = view_dir(session).and_then(|view| layout::read(&view)) {
        taken.extend(
            layout
                .tabs()
                .flat_map(|t| t.leaves())
                .map(|p| p.name.clone()),
        );
    }
    for number in 1..=MAX_NEW_NAMES {
        let name = format!("{prefix}-{number}");
        if taken.contains(&name) {
            continue;
        }
        match start(fux3, session, &name, program, cancel) {
            Err(failure) if failure.0.contains("exists") => {}
            outcome => return outcome,
        }
    }
    Err(Failure::new(format!("no free name {prefix}-N")))
}

/// Runs one layout command on the session's layout file, as seen in a
/// screen of `area`; `split PANE DIR` starts its shell first.
pub fn run_line(
    session: &Session,
    line: &str,
    area: Rect,
    cancel: &AtomicBool,
) -> Result<Edited, Failure> {
    let command = layout::parse_command(line)?;
    let view = view_dir(session)?;
    let (op, started) = if let Op::SplitNew(target, dir) = &command {
        {
            // The pane split must have a tile before a shell is started for it.
            let shown = layout::read(&view)?.shown(&live_panes(session));
            if !shown
                .tabs()
                .flat_map(|t| t.leaves())
                .any(|p| p.name == target.name)
            {
                return Err(Failure::new(format!("no tile for pane {target}")));
            }
            let pane = start_new(&fux3_program()?, session, "shell", &[], cancel)?;
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
    let edited = layout::edit(&view, &live_panes(session), area, &op);
    match (edited, started) {
        (Ok(edited), started) => Ok(Edited { started, ..edited }),
        (Err(failure), Some(pane)) => Err(Failure::new(format!(
            "{pane} is running but has no tile: {failure}"
        ))),
        (Err(failure), None) => Err(failure),
    }
}
