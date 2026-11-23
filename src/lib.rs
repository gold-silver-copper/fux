//! fux: a terminal multiplexer that is a view over directories. A pane is a
//! directory kept by one small process; a workspace is a folder of panes;
//! the viewer and every other client only read and write files.
#![forbid(unsafe_code)]

/// Environment-variable prefix (`FUX_DIR`, `FUX_KEYS`, `FUX_WS`, `FUX_VIEW`, `FUX_PANE`, ...).
macro_rules! name {
    ($suffix:literal) => {
        concat!("FUX_", $suffix)
    };
}
/// The folder of a session (in the runtime or cache directory) and of the keys (in the config directory).
const FOLDER: &str = "fux";
/// What `TERM_PROGRAM` says in a pane.
pub const TERM_PROGRAM: &str = "fux";

mod holder;
mod keys;
mod layout;
#[cfg(test)]
mod tests;
mod view;

pub use view::view;

// The emulator and the system calls are crates of their own, under the names this one calls them by.
use fux_vt::{self as term, sixel, width};
use fuxix as sys;

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "\
fux dir                         print the session directory
fux start [NAME] [-- PROGRAM…]  start a pane; NAME is PANE, WORKSPACE/PANE or WORKSPACE/
fux wait PANE-DIRECTORY         wait until the pane's holder is gone; print its program's pid and the state it left
fux view [WORKSPACE]            show a workspace's panes in tiles and tabs (default: $FUX_WS; else in a pane its own; else main)
fux layout [EDIT…]              print the layout of the workspace (the same default), or edit it
fux --help                      this summary and the complete guide (for people, scripts and agents)";
/// The single help page for people, scripts and agents; every command on it is run by `tools/help-agents.sh`.
const HELP: &str = include_str!("help.txt");

/// A variable of the environment that is set and not empty, as a path.
fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The pane a command was typed in: `$FUX_PANE`, when it is a directory's whole path.
fn here() -> Option<PathBuf> {
    var(name!("PANE")).filter(|pane| pane.is_absolute())
}

/// The session directory: `$FUX_DIR`; else, in a pane, the session that pane
/// is in; else `$XDG_RUNTIME_DIR/fux`; else `~/.cache/fux`. By its whole path: one named by
/// a relative path is meant from here, and every path of a workspace or a pane is made from this one.
fn session() -> Result<PathBuf, String> {
    let from_pane = || Some(here()?.parent()?.parent()?.to_path_buf());
    let xdg = || var("XDG_RUNTIME_DIR").map(|d| d.join(FOLDER));
    let home = || var("HOME").map(|d| d.join(".cache").join(FOLDER));
    let dir = var(name!("DIR")).or_else(from_pane).or_else(xdg).or_else(home);
    let dir = dir.ok_or(concat!("no session directory: set ", name!("DIR")))?;
    std::path::absolute(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// The workspace a command means: the one `given` it, refused as `wrong` says if it is no name; else `$FUX_WS`
/// (a key script's); else, in a pane, the folder that holds the pane; else `main`. One that is no name is refused.
fn workspace(given: Option<&str>, wrong: &str) -> Result<String, String> {
    let from_pane = || here()?.parent()?.file_name().map(PathBuf::from);
    let meant = || var(name!("WS")).or_else(from_pane).unwrap_or("main".into()).to_string_lossy().into_owned();
    let name = given.map_or_else(meant, String::from);
    let wrong = given.map_or(format!("{name}: not a workspace's name"), |_| wrong.into());
    if is_name(&name) { Ok(name) } else { Err(wrong) }
}

/// The names fux will not make: `-` (which marks a tab in the layout and is "the one before" to
/// `fux layout`), the session's files of what the terminal is like, and a workspace's layout.
const TAKEN: [&str; 5] = ["-", "colours", "keyboard", "layout", "layout.new"];

/// A name fux will make: letters, digits, `.`, `_` and `-`, not starting with a dot, and none of `TAKEN`.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && !TAKEN.contains(&name)
        && !name.starts_with('.')
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Text for a file a person reads with `cat`: control characters become `?`.
pub fn plain(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// What tells one writing of a file from the next: its number, its time and its length.
pub type Stamp = Option<(u64, i64, i64, u64)>;
pub fn stamp(file: fs::Metadata) -> (u64, i64, i64, u64) {
    (file.ino(), file.mtime(), file.mtime_nsec(), file.len())
}

/// Replaces a file in one step, so a reader sees the old one or the new one.
pub fn replace(name: impl AsRef<Path>, content: &[u8]) -> std::io::Result<()> {
    let mut new = name.as_ref().as_os_str().to_owned();
    new.push(".new");
    fs::write(&new, content)?;
    fs::rename(&new, name)
}

/// A line on standard output. A reader that has left is no reason for a word; any other failure is.
fn say(line: &[u8]) -> Result<(), String> {
    let mut out = std::io::stdout();
    match out.write_all(line).and_then(|()| out.write_all(b"\n")).and_then(|()| out.flush()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(format!("cannot write the answer: {e}")),
        _ => Ok(()),
    }
}

/// Up to `most` bytes of an opened file as it is now: what could be read of it, and whether
/// that went well.
fn read(file: std::io::Result<File>, most: u64) -> (Vec<u8>, std::io::Result<usize>) {
    let mut bytes = Vec::new();
    let read = file.and_then(|file| file.take(most).read_to_end(&mut bytes));
    (bytes, read)
}

/// The lines that end in what was `read` of a FIFO, after what was `kept` of one that had not ended. A line
/// of over `most` bytes, as they were read, is None: a byte past the bound says so, and the rest is not kept.
pub fn lines(kept: &mut Vec<u8>, read: &[u8], most: usize) -> Vec<Option<String>> {
    let mut lines = Vec::new();
    for &byte in read {
        if byte == b'\n' {
            let line = std::mem::take(kept);
            lines.push((line.len() <= most).then(|| String::from_utf8_lossy(&line).into_owned()));
        } else if kept.len() <= most {
            kept.push(byte);
        }
    }
    lines
}

/// The same as text, of a file opened without waiting (should the name be a FIFO's).
fn text(path: &Path, most: u64) -> String {
    String::from_utf8_lossy(&read(sys::open(path, false), most).0).into_owned()
}

fn start(args: &[OsString]) -> Result<(), String> {
    let mut sections = args.splitn(2, |arg| arg == "--");
    let name = match sections.next().unwrap_or_default() {
        [] => String::new(),
        [name] => name.to_string_lossy().into_owned(),
        _ => return Err("start takes one NAME, then -- and the program".into()),
    };
    // (The workspace of whoever starts it is asked for only if none is named.)
    let (given, pane) = name.split_once('/').map_or((None, name.as_str()), |(workspace, pane)| (Some(workspace), pane));
    let wrong = format!(
        "{name}: a name is letters, digits, '.', '_' and '-', at most 64, not starting with a dot, and none of: {}",
        TAKEN.join(" ")
    );
    let workspace = workspace(given, &wrong)?;
    if !(pane.is_empty() || is_name(pane)) {
        return Err(wrong);
    }
    let session = session()?;
    let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let workspace = session.join(workspace);
    let fail = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    DirBuilder::new().mode(0o700).recursive(true).create(&workspace).map_err(|e| fail(&workspace, e))?;
    // Making the directory is what claims a new name; a name that is taken is a pane to start
    // again in place, and a number that is taken is not free. A folder that was made stays,
    // also when the start fails: its `log` says why.
    let dir = (1..10000)
        .map(|n| if pane.is_empty() { workspace.join(n.to_string()) } else { workspace.join(pane) })
        .find_map(|dir| match DirBuilder::new().mode(0o700).create(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => Some(Err(fail(&dir, e))),
            made => (made.is_ok() || !pane.is_empty()).then_some(Ok(dir)),
        })
        .ok_or("no free number for a pane")??;
    // The lock on the pane's directory is the proof that a holder lives: the kernel drops it when the holder is
    // gone, however it went. We take it here and the holder inherits it, and the directory with it: entered
    // first, it is the one that is locked and the one the holder works in, even if it is renamed meanwhile.
    let lock = std::env::set_current_dir(&dir).and_then(|()| sys::open(".", false)).map_err(|e| fail(&dir, e))?;
    let started = match lock.try_lock() {
        Ok(()) => launch(&session, &dir, &cwd, lock, sections.next().unwrap_or_default()),
        Err(_) => Err("a holder already keeps this pane".into()),
    };
    started.map_err(|why| format!("{}: {why}", dir.display()))?;
    say(dir.as_os_str().as_bytes())
}

/// Starts the holder of a pane we hold the lock of and are in, and waits until it is up. `cwd` is where we were.
fn launch(session: &Path, dir: &Path, cwd: &Path, lock: File, program: &[OsString]) -> Result<(), String> {
    let log = File::create("log").map_err(|e| format!("cannot make the log: {e}"))?;
    let me = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let mut holder = Command::new(me);
    holder.arg("hold").arg(cwd).args(program);
    holder.args(program.is_empty().then(|| var("SHELL").unwrap_or_else(|| "/bin/sh".into())));
    // The pane's directory by the name it was started under (the holder's own working directory may read
    // otherwise, through a link), and the session, if it was named: whole paths, as the program may go anywhere.
    holder.env(name!("PANE"), dir).envs(var(name!("DIR")).map(|_| (name!("DIR"), session)));
    holder.stdin(Stdio::from(lock)).stdout(Stdio::piped()).stderr(log);
    sys::new_session(&mut holder, false);
    let mut holder = holder.spawn().map_err(|e| format!("cannot start the holder: {e}"))?;
    // The holder says a line when its first screen is written: it is up. With no line it has
    // ended before that, whatever became of its folder since: its log's last line says why.
    if matches!(holder.stdout.take().map(|mut line| line.read(&mut [0])), Some(Ok(1))) {
        return Ok(());
    }
    let _ = holder.wait();
    let log = text(Path::new("log"), 64 << 10);
    let why = log.lines().last().unwrap_or("the holder ended without a word");
    Err(why.trim_start_matches("fux: ").to_string())
}

fn wait(args: &[OsString]) -> Result<(), String> {
    let [dir] = args else {
        return Err("wait takes one pane directory".into());
    };
    // The holder locks its directory for as long as it lives. The directory is entered first:
    // the lock and what is read are of that same directory, even if it is renamed meanwhile.
    let entered = std::env::set_current_dir(dir).and_then(|()| sys::open(".", false));
    let lock = entered.and_then(|lock| lock.lock_shared().map(|()| lock));
    let lock = lock.map_err(|e| format!("{}: {e}", Path::new(dir).display()))?;
    // What became of the program is in `state`, and its number in `status`: the holder wrote
    // both, and the last screen after them, before its lock went.
    let (state, read) = read(sys::open("state", false), 64);
    read.map_err(|e| format!("{}/state: {e}", Path::new(dir).display()))?;
    let status = text(Path::new("status"), 64 << 10);
    drop(lock);
    let pid = status.lines().find(|line| line.starts_with("pid ")).unwrap_or("pid 0");
    say(format!("{pid}\nstate {}", String::from_utf8_lossy(&state).trim_end()).as_bytes())
}

fn run(args: &[OsString]) -> Result<(), String> {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().and_then(|a| a.to_str()) {
        Some("dir") => say(session()?.as_os_str().as_bytes()),
        Some("start") => start(rest),
        Some("wait") => wait(rest),
        Some("view") => {
            let wrong = "view takes one workspace's name";
            let ([] | [_]) = rest else { return Err(wrong.into()) };
            let workspace = workspace(rest.first().map(|name| name.to_string_lossy()).as_deref(), wrong)?;
            let config = var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|home| home.join(".config")));
            let keys = var(name!("KEYS")).or_else(|| config.map(|config| config.join(FOLDER).join("keys")));
            let keys = keys.map(|keys| std::path::absolute(&keys).unwrap_or(keys));
            view::view(session()?, workspace, keys.ok_or(concat!("no keys directory: set ", name!("KEYS")))?)
        }
        Some("layout") => {
            let words = rest.iter().map(|w| w.to_str()).collect::<Option<Vec<_>>>().ok_or("layout edit is not text")?;
            let layout = layout::command(&session()?.join(workspace(None, "")?), &words)?;
            if words.is_empty() { say(layout.print().trim_end().as_bytes()) } else { Ok(()) }
        }
        Some("hold") => holder::hold(rest),
        Some("help" | "--help" | "-h") if rest.is_empty() => say(format!("{USAGE}\n\n{}", HELP.trim_end()).as_bytes()),
        _ => Err(format!("usage:\n{USAGE}")),
    }
}

pub fn main() -> ExitCode {
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
