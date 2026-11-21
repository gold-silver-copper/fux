//! fux: a terminal multiplexer that is a view over directories. A pane is a
//! directory kept by one small process; a workspace is a folder of panes;
//! the viewer and every other client only read and write files.
#![forbid(unsafe_code)]

/// The names that tell this program from the fux it is to replace. They are
/// temporary: when that fux is retired, these three lines become `FUX_`,
/// `fux` and `fux` (and the key scripts' variables with them).
macro_rules! name {
    ($suffix:literal) => {
        concat!("FUXN_", $suffix)
    };
}
/// The folder of a session (in the runtime or cache directory) and of the keys (in the config directory).
const FOLDER: &str = "fux-next";
/// What `TERM_PROGRAM` says in a pane.
pub const TERM_PROGRAM: &str = "fux-next";

mod holder;
mod keys;
mod layout;
#[cfg(test)]
mod tests;
mod view;

// The emulator and the system calls are crates of their own, under the names this one calls them by.
use fux_vt::{self as term, sixel, width};
use fuxix as sys;

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

const USAGE: &str = "\
fux dir                         print the session directory
fux start [NAME] [-- PROGRAM…]  start a pane; NAME is PANE, WORKSPACE/PANE or WORKSPACE/
fux wait PANE-DIRECTORY         wait until the pane's holder is gone; print its program's pid and last state
fux view [WORKSPACE]            show a workspace's panes in tiles and tabs (default: in a pane its own, else main)
fux layout [EDIT…]              print the layout of the workspace (in a pane: its own; else main), or edit it
fux help agents                 one page for a program that drives panes: start, type, read, wait, end
Start with `fux help agents`: the files of a pane, and how to start, type, read, wait and end, on one page.";
/// The page for agents: complete by itself, and every command on it is run by `tools/help-agents.sh`.
const AGENTS: &str = include_str!("help-agents.txt");

/// A variable of the environment that is set and not empty, as a path.
fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The pane a command was typed in: `$FUXN_PANE`, when it is a directory's whole path.
fn here() -> Option<PathBuf> {
    var(name!("PANE")).filter(|pane| pane.is_absolute())
}

/// The session directory: `$FUXN_DIR`; else, in a pane, the session that pane
/// is in; else `$XDG_RUNTIME_DIR/fux-next`; else `~/.cache/fux-next`.
fn session() -> Result<PathBuf, String> {
    let from_pane = || Some(here()?.parent()?.parent()?.to_path_buf());
    // A directory named by a relative path is meant from here: panes and scripts get its whole path.
    let whole = |dir: PathBuf| std::path::absolute(&dir).unwrap_or(dir);
    var(name!("DIR"))
        .map(whole)
        .or_else(from_pane)
        .or_else(|| var("XDG_RUNTIME_DIR").map(|d| d.join(FOLDER)))
        .or_else(|| var("HOME").map(|d| d.join(".cache").join(FOLDER)))
        .ok_or_else(|| concat!("no session directory: set ", name!("DIR")).into())
}

/// The workspace meant when none is named: `$FUXN_WS` (a key script's); else,
/// in a pane, the folder that holds the pane; else `main`.
fn workspace() -> String {
    let from_pane = || here()?.parent()?.file_name()?.to_str().map(String::from);
    let named = std::env::var(name!("WS")).ok().or_else(from_pane);
    named.filter(|name| is_name(name)).unwrap_or("main".into())
}

/// A name fux will make: letters, digits, `.`, `_` and `-`, not starting with a dot, and not `-` alone
/// (which marks a tab in the layout and means "the one before" to the viewer), nor the name of
/// the session's files of what the terminal is like (its colours, its keyboard).
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name != "-"
        && name != "colours"
        && name != "keyboard"
        && !name.starts_with('.')
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Text for a file a person reads with `cat`: control characters become `?`.
pub fn plain(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// Replaces a file in one step, so a reader sees the old one or the new one.
pub fn replace(name: impl AsRef<Path>, content: &[u8]) -> std::io::Result<()> {
    let mut new = name.as_ref().as_os_str().to_owned();
    new.push(".new");
    fs::write(&new, content)?;
    fs::rename(&new, name)
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

/// Up to `most` bytes of a file as it is now, opened without waiting (should the name be a
/// FIFO's): what could be read of it, and whether that went well.
fn read(path: &Path, most: u64) -> (Vec<u8>, std::io::Result<usize>) {
    let mut bytes = Vec::new();
    let read = sys::open(path, false).and_then(|file| file.take(most).read_to_end(&mut bytes));
    (bytes, read)
}

/// The same, as text.
fn text(path: &Path, most: u64) -> String {
    String::from_utf8_lossy(&read(path, most).0).into_owned()
}

/// At most 64 KiB of a small file of ours, as text, or why it cannot be read.
fn read_some(path: &Path) -> std::io::Result<String> {
    let (bytes, read) = read(path, 64 << 10);
    read.map(|_| String::from_utf8_lossy(&bytes).into_owned())
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
        holder.env(name!("PANE"), std::path::absolute(dir).unwrap_or(dir.to_path_buf()));
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
        // (Its status comes with its first facts, a moment before its first screen.)
        if read_some(&dir.join("status")).is_ok_and(|status| status.contains(&ours))
            && dir.join("screen.styled").exists()
        {
            return Ok(());
        }
        // A pane may remove its own folder when its program is done (the default
        // shell panes do): gone with its holder, it started, ran and ended.
        if ended && !dir.exists() {
            return Ok(());
        }
        if ended {
            let log = read_some(&dir.join("log")).unwrap_or_default();
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
    // read in that same directory before our own lock goes, even if it was renamed.
    let lock = File::open(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    lock.lock_shared().map_err(|e| format!("{}: {e}", dir.display()))?;
    sys::enter_directory(&lock).map_err(|e| format!("{}: {e}", dir.display()))?;
    // What became of the program is in `state`, and its number in `status`: the holder wrote
    // both, and the last screen after them, before its lock went.
    let state = read_some(Path::new("state")).map_err(|e| format!("{}/state: {e}", dir.display()))?;
    let status = read_some(Path::new("status")).unwrap_or_default();
    drop(lock);
    let pid = status.lines().find(|line| line.starts_with("pid ")).unwrap_or("pid 0");
    let said = [pid.to_string(), format!("state {}", state.trim_end())];
    say(said.join("\n").as_bytes())
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
            let keys = var(name!("KEYS")).or_else(|| config.map(|config| config.join(FOLDER).join("keys")));
            let keys = keys.map(|keys| std::path::absolute(&keys).unwrap_or(keys));
            view::view(session()?, workspace, keys.ok_or(concat!("no keys directory: set ", name!("KEYS")))?)
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
        Some("help") if rest == ["agents"] => say(AGENTS.trim_end().as_bytes()),
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
            // (Line by line: the usage is several.)
            let why: String = why.lines().map(plain).collect::<Vec<_>>().join("\n").chars().take(1000).collect();
            let _ = writeln!(std::io::stderr(), "fux: {why}");
            ExitCode::FAILURE
        }
    }
}
