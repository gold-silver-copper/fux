//! fux: a terminal multiplexer that is a view over directories. A pane is a
//! directory kept by one small process; a workspace is a folder of panes;
//! the viewer and every other client only read and write files.

mod holder;
mod keys;
mod layout;
mod sys;
mod term;
#[cfg(test)]
mod tests;
mod view;
mod width;

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

const USAGE: &str = "\
fux dir                         print the session directory (FUX_DIR sets it)
fux start [NAME] [-- PROGRAM…]  start a pane; NAME is PANE, WORKSPACE/PANE or WORKSPACE/
fux wait PANE-DIRECTORY         wait until the pane's holder is gone; print its status
fux view [WORKSPACE]            show a workspace's panes in tiles and tabs (default: main)
fux layout [EDIT…]              print the layout of the workspace (in a pane: its own; else main), or edit it
The files of a pane, and everything else, are in the README.";

/// A variable of the environment that is set and not empty, as a path.
fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The pane a command was typed in: `$FUX_PANE`, when it is a directory's
/// whole path (the fux before this one put `%N` there).
fn here() -> Option<PathBuf> {
    var("FUX_PANE").filter(|pane| pane.is_absolute())
}

/// The session directory: `$FUX_DIR`; else, in a pane, the session that pane
/// is in; else `$XDG_RUNTIME_DIR/fux/workspaces`; else `~/.cache/fux/workspaces`.
fn session() -> Result<PathBuf, String> {
    let from_pane = || Some(here()?.parent()?.parent()?.to_path_buf());
    var("FUX_DIR")
        .or_else(from_pane)
        .or_else(|| var("XDG_RUNTIME_DIR").map(|d| d.join("fux/workspaces")))
        .or_else(|| var("HOME").map(|d| d.join(".cache/fux/workspaces")))
        .ok_or_else(|| "no session directory: set FUX_DIR".into())
}

/// The workspace meant when none is named: `$FUX_WS` (a key script's); else,
/// in a pane, the folder that holds the pane; else `main`.
fn workspace() -> String {
    let from_pane = || here()?.parent()?.file_name()?.to_str().map(String::from);
    let named = std::env::var("FUX_WS").ok().or_else(from_pane);
    named.filter(|name| is_name(name)).unwrap_or("main".into())
}

/// A name fux will make: letters, digits, `.`, `_` and `-`, not starting with a dot.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn make_dir(path: &Path) -> std::io::Result<()> {
    DirBuilder::new().mode(0o700).create(path)
}

/// A line on standard output. A reader that has left is no reason for a word; any other failure is.
fn say(line: &[u8]) -> Result<(), String> {
    let mut out = std::io::stdout();
    match out.write_all(line).and_then(|()| out.write_all(b"\n")).and_then(|()| out.flush()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(format!("cannot write the answer: {e}")),
        _ => Ok(()),
    }
}

/// At most 64 KiB of a small file of ours, as text.
fn read_some(path: &Path) -> String {
    let mut bytes = Vec::new();
    let _ = File::open(path).map(|file| file.take(64 << 10).read_to_end(&mut bytes));
    String::from_utf8_lossy(&bytes).into_owned()
}

fn start(args: &[OsString]) -> Result<(), String> {
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let (names, program) = (args.get(..split).unwrap_or(&[]), args.get(split + 1..).unwrap_or(&[]));
    let name = match names {
        [] => format!("{}/", workspace()),
        [name] => name.to_string_lossy().into_owned(),
        _ => return Err("start takes one NAME, then -- and the program".into()),
    };
    let default = workspace();
    let (workspace, pane) = name.split_once('/').unwrap_or((&default, &name));
    if !is_name(workspace) || !(pane.is_empty() || is_name(pane)) {
        return Err(format!(
            "{name}: a name is letters, digits, '.', '_' and '-', at most 64, not starting with a dot"
        ));
    }
    if ["layout", "layout.new"].iter().any(|taken| pane.eq_ignore_ascii_case(taken)) {
        return Err(format!("{name}: a workspace keeps its layout in a file of that name"));
    }
    let program: Vec<OsString> = match program {
        [] => vec![std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into())],
        program => program.to_vec(),
    };
    let workspace = session()?.join(workspace);
    let fail = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    DirBuilder::new().mode(0o700).recursive(true).create(&workspace).map_err(|e| fail(&workspace, e))?;
    // The claim is made under a lock on the workspace, so that two starts of
    // one name, or a start and the tidying after a failed one, never mix.
    let claim =
        File::open(&workspace).and_then(|claim| claim.lock().map(|()| claim)).map_err(|e| fail(&workspace, e))?;
    // Making the directory is what claims a new name.
    let named = |dir: PathBuf| match make_dir(&dir) {
        Ok(()) => Ok(Some((dir, true))),
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => Err(fail(&dir, e)),
        Err(_) if pane.is_empty() => Ok(None),
        Err(_) => Ok(Some((dir, false))),
    };
    let (dir, made) = if pane.is_empty() {
        let mut numbers = (1..10000).map(|n| named(workspace.join(n.to_string())));
        numbers.find_map(Result::transpose).ok_or("no free number for a pane")??
    } else {
        named(workspace.join(pane))?.ok_or("no pane")?
    };
    // The lock on the pane's directory is the proof that a holder lives: the
    // kernel drops it when the holder is gone, however it went. We take it
    // here and the holder inherits it. A `fux wait` may hold it for an instant.
    let lock = File::open(&dir);
    let started = match &lock {
        Err(e) => Err(e.to_string()),
        Ok(lock) => {
            let locked = (0..20).any(|i| {
                std::thread::sleep(Duration::from_millis(5 * u64::from(i > 0)));
                lock.try_lock().is_ok()
            });
            // The workspace is let go while the holder starts, and taken again to tidy up if it does not.
            let _ = claim.unlock();
            if locked { launch(&dir, lock, &program) } else { Err("a holder already keeps this pane".into()) }
        }
    };
    match started {
        Ok(()) => {
            // Our copy of the lock goes before anything that could wait.
            drop(lock);
            say(dir.as_os_str().as_bytes())
        }
        Err(why) => {
            // A folder made for a pane that never was is not left behind. We
            // still hold its lock, so nobody has started anything in it.
            if made && claim.lock().is_ok() {
                let _ = fs::remove_dir_all(&dir).or_else(|_| fs::remove_dir(&dir));
            }
            Err(format!("{}: {why}", dir.display()))
        }
    }
}

/// Starts the holder of a pane we hold the lock of, and waits until it is up.
fn launch(dir: &Path, lock: &File, program: &[OsString]) -> Result<(), String> {
    let log = File::create(dir.join("log")).map_err(|e| format!("cannot make the log: {e}"))?;
    let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let me = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let mut holder = {
        let lock = lock.try_clone().map_err(|e| format!("cannot copy the lock: {e}"))?;
        let mut holder = Command::new(me);
        holder.arg("hold").arg(cwd).args(program).current_dir(dir);
        // The pane's directory by the name it was started under (the holder's
        // own working directory may read otherwise, through a link).
        holder.env("FUX_PANE", std::path::absolute(dir).unwrap_or(dir.to_path_buf()));
        holder.stdin(Stdio::from(lock)).stdout(Stdio::null()).stderr(log);
        sys::new_session(&mut holder, false);
        holder.spawn().map_err(|e| format!("cannot start the holder: {e}"))?
    };
    // The holder is up when `status` names it. A status left by an earlier
    // program stays until then, and for good if this start fails.
    let ours = format!("holder {}\n", holder.id());
    loop {
        // An error here is the end too: there is nothing left to wait for.
        let ended = !matches!(holder.try_wait(), Ok(None));
        if read_some(&dir.join("status")).contains(&ours) {
            return Ok(());
        }
        if ended {
            let log = read_some(&dir.join("log"));
            let why = log.lines().last().unwrap_or("the holder ended without a word");
            return Err(why.trim_start_matches("fux: ").to_string());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait(args: &[OsString]) -> Result<(), String> {
    let [dir] = args else {
        return Err("wait takes one pane directory".into());
    };
    let dir = Path::new(dir);
    // The holder locks its directory for as long as it lives. The status is
    // read before our own lock goes, so it is that holder's and no later one's.
    let lock = File::open(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    lock.lock_shared().map_err(|e| format!("{}: {e}", dir.display()))?;
    let status = read_some(&dir.join("status"));
    drop(lock);
    say(status.trim_end().as_bytes())
}

fn run(args: &[OsString]) -> Result<(), String> {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().and_then(|a| a.to_str()) {
        Some("dir") => say(session()?.as_os_str().as_bytes()),
        Some("start") => start(rest),
        Some("wait") => wait(rest),
        Some("view") => {
            let workspace = rest.first().map_or(workspace(), |name| name.to_string_lossy().into_owned());
            if rest.len() > 1 || !is_name(&workspace) {
                return Err("view takes one workspace's name".into());
            }
            let config = var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|home| home.join(".config")));
            let keys = var("FUX_KEYS").or_else(|| config.map(|config| config.join("fux/keys")));
            view::view(session()?, workspace, keys.ok_or("no keys directory: set FUX_KEYS")?)
        }
        Some("layout") => {
            let workspace = workspace();
            let words: Vec<String> = rest.iter().map(|word| word.to_string_lossy().into_owned()).collect();
            let text =
                layout::command(&session()?.join(workspace), &words.iter().map(String::as_str).collect::<Vec<_>>())?;
            if words.is_empty() { say(text.trim_end().as_bytes()) } else { Ok(()) }
        }
        Some("hold") => match rest.split_first() {
            Some((cwd, program)) => holder::hold(program, cwd),
            None => Err("hold is started by `fux start`".into()),
        },
        Some("help" | "--help" | "-h") => say(USAGE.as_bytes()),
        _ => Err(format!("usage:\n{USAGE}")),
    }
}

fn main() -> ExitCode {
    sys::own_children();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            // What is quoted may be anything: no control codes, and not without end.
            let why: String = holder::plain(&why).chars().take(1000).collect();
            let _ = writeln!(std::io::stderr(), "fux: {why}");
            ExitCode::FAILURE
        }
    }
}
