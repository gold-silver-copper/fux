//! fux: a terminal multiplexer that is a view over directories. A pane is a
//! directory kept by one small process; a workspace is a folder of panes;
//! the viewer and every other client only read and write files.
#![forbid(unsafe_code)]

/// The folder of a session (in the runtime or cache directory) and of the keys (in the config directory).
const FOLDER: &str = "fux";
/// What `TERM_PROGRAM` says in a pane.
pub const TERM_PROGRAM: &str = "fux";
/// The number a carrier of pane folders compares: raised when a reader of a folder written by the fux before
/// would misread one this fux writes (a file's meaning or shape changed, a file a reader needs gone), and not
/// when a file is added.
pub const FORMAT: u32 = 1;

mod holder;
mod keys;
mod layout;
#[cfg(test)]
mod tests;
mod view;

pub use view::{LEAVING, view, view_pane};

// The emulator and the system calls are crates of their own, under the names this one calls them by.
use fux_vt::{self as term, sixel, width};
use fuxix as sys;

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "\
fux dir                         print the default session's directory
fux start [PANE] [-- PROGRAM…]  start a pane; PANE is NAME (in main), WORKSPACE/NAME or WORKSPACE/, and not one that is there
fux wait PANE-DIRECTORY         wait until the pane's holder is gone; print its program's pid and the state it left
fux view [WORKSPACE [KEYS]]     show a workspace's panes in tiles and tabs (default: main), with the key scripts of KEYS
fux layout [WORKSPACE [EDIT…]]  print the layout of a workspace (default: main), or edit it
fux --help                      this summary and the complete guide (for people, scripts and agents)
WORKSPACE (and what PANE has before its last /) is a name in the default session, or a directory's path if it begins with / or .";
/// The single help page for people, scripts and agents; every command on it is run by `tools/help-agents.sh`.
const HELP: &str = include_str!("help.txt");

/// A variable of the environment that is set and not empty, as a path.
fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The default session's directory, the one a name is a name in: `$XDG_RUNTIME_DIR/fux`; else
/// `~/.cache/fux`. By its whole path. (Any other session is named by the paths of what is in it.)
fn session() -> Result<PathBuf, String> {
    let home = || var("HOME").map(|d| d.join(".cache").join(FOLDER));
    let dir = var("XDG_RUNTIME_DIR").map(|d| d.join(FOLDER)).or_else(home);
    let dir = dir.ok_or("no session directory: neither XDG_RUNTIME_DIR nor HOME is set")?;
    std::path::absolute(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// The folder of the workspace an argument names. One that begins with `/` or `.` is a directory's
/// path, taken as it is; any other is a name, of a workspace of the default session, and refused as
/// `wrong` says if it is none.
fn workspace(named: &str, wrong: &str) -> Result<PathBuf, String> {
    match named {
        path if path.starts_with(['/', '.']) => Ok(path.into()),
        name if is_name(name) => Ok(session()?.join(name)),
        _ => Err(wrong.into()),
    }
}

/// Makes a workspace's folder if it is not there (the owner's alone), and gives its own whole path:
/// links followed, `..` as the system reads it. That holds when the way that led there (a link) is gone.
fn made(workspace: &Path) -> Result<PathBuf, String> {
    let made = DirBuilder::new().mode(0o700).recursive(true).create(workspace);
    made.and_then(|()| fs::canonicalize(workspace)).map_err(|e| format!("{}: {e}", workspace.display()))
}

/// The names fux will not make: `-` (which marks a tab in the layout and is "the one before" to
/// `fux layout`), the session's files of what the terminal is like, and a workspace's layout.
const TAKEN: [&str; 4] = ["-", "colours", "keyboard", "layout"];

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

/// Replaces a file in one step, so a reader sees the old one or the new one. The new one is put
/// together beside it, under `staging`'s name for it, and does not stay there when it cannot be
/// given its name. For a name that one writer writes: two at once would put theirs together under
/// the one name (a holder is alone in its pane by its lock; a layout is written under its
/// workspace's).
pub fn replace(name: impl AsRef<Path>, content: &[u8]) -> std::io::Result<()> {
    let new = staging(name.as_ref());
    let replaced = write_new(&new, content).and_then(|()| fs::rename(&new, name));
    replaced.inspect_err(|_| _ = fs::remove_file(&new))
}

/// The name a file is put together under before it is given its own: `.NAME.new` in the same
/// folder (`main/.screen.new` for `main/screen`). A name that starts with a dot is no pane's, no
/// workspace's and no fact's: whoever lists a folder for what is in it passes it over.
pub fn staging(name: &Path) -> PathBuf {
    let mut new = std::ffi::OsString::from(".");
    new.push(name.file_name().unwrap_or_default());
    new.push(".new");
    name.with_file_name(new)
}

/// Writes a file that is made for it: what had the name goes first, and the file is made only if
/// nothing has the name then. So a link of that name is not written through (to make a file anew
/// is never to follow a link), nor is a FIFO there waited at.
pub fn write_new(name: &Path, content: &[u8]) -> std::io::Result<()> {
    let _ = fs::remove_file(name);
    fs::OpenOptions::new().write(true).create_new(true).open(name)?.write_all(content)
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

/// The key at the start of what a terminal sent, as the viewer reads one: how many bytes it is, its
/// name (a key's file's: `C-]`, `dot`, `paste-start`; none for a sequence that is no key), and whether
/// a key goes down with it. (In the kitty protocol's forms a terminal tells of a key let go too, and of
/// Shift or Ctrl by itself: those are no key going down.) None while more bytes are needed to tell; with
/// `whole`, no more are coming. For a program that has a key of its own on a terminal whose other keys
/// are a pane's: koh, which leaves a session by one.
pub fn key(bytes: &[u8], whole: bool) -> Option<(usize, String, bool)> {
    let (length, name, event) = keys::next(bytes, whole, false)?;
    Some((length, name, event.is_none_or(|(.., kind, alone)| kind != 3 && !alone)))
}

/// Which run a pane's folder is of: the number in its `run`, 128 random bits its holder wrote there once, as
/// 32 hex digits. None if the name has anything but that file, to the byte (no file, a link, other digits,
/// one more or fewer): a folder no holder of this fux made.
pub fn run_of(dir: &Path) -> Option<u128> {
    let (text, read) = read(sys::open_regular(dir.join("run")), 34);
    let digits = text.strip_suffix(b"\n").filter(|digits| read.is_ok() && digits.len() == 32)?;
    let digits = digits.iter().all(|digit| matches!(digit, b'0'..=b'9' | b'a'..=b'f')).then_some(digits)?;
    u128::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
}

/// The same as text, of a file opened without waiting (should the name be a FIFO's).
fn text(path: &Path, most: u64) -> String {
    String::from_utf8_lossy(&read(sys::open(path, false), most).0).into_owned()
}

fn start(args: &[OsString]) -> Result<(), String> {
    let mut sections = args.splitn(2, |arg| arg == "--");
    let name = match sections.next().unwrap_or_default() {
        [] => String::new(),
        [name] => name.to_str().ok_or("start takes a PANE that is text")?.to_string(),
        _ => return Err("start takes one PANE, then -- and the program".into()),
    };
    // The pane's name is what follows the last `/` (nothing: the first free number), and what is
    // before it names the workspace: a path with the `/` it ends in (`/p` is the pane `p` of `/`),
    // a name without, and with no `/` at all, `main`.
    let pane = name.rsplit('/').next().unwrap_or_default();
    let given = name.strip_suffix(pane).unwrap_or_default();
    let given = if name.starts_with(['/', '.']) { given } else { given.strip_suffix('/').unwrap_or("main") };
    let wrong = format!(
        "{name}: a name is letters, digits, '.', '_' and '-', at most 64, not starting with a dot, and none of: {}",
        TAKEN.join(" ")
    );
    let workspace = workspace(given, &wrong)?;
    if !(pane.is_empty() || is_name(pane)) {
        return Err(wrong);
    }
    let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let workspace = made(&workspace)?;
    let fail = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    // Making the directory is what claims a name, and a pane is one run: a name that is there is
    // refused, whatever has it (a pane that runs, one that has ended, a folder nobody started), and a
    // number that is there is not free. A folder that was made stays, also when the start fails:
    // its `log` says why.
    let taken = "the name is taken, and a pane is one run: `rm -r` what has it to start the name anew";
    let dir = (1..10000)
        .map(|n| if pane.is_empty() { workspace.join(n.to_string()) } else { workspace.join(pane) })
        .find_map(|dir| match DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => Some(Ok(dir)),
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => Some(Err(fail(&dir, e))),
            Err(_) => (!pane.is_empty()).then(|| Err(format!("{}: {taken}", dir.display()))),
        })
        .ok_or("no free number for a pane")??;
    // The lock on the pane's directory is the proof that a holder lives: the kernel drops it when the holder is
    // gone, however it went. We take it here and the holder inherits it, and the directory with it: entered
    // first, it is the one that is locked and the one the holder works in, even if it is renamed meanwhile.
    let lock = std::env::set_current_dir(&dir).and_then(|()| sys::open(".", false)).map_err(|e| fail(&dir, e))?;
    let started = match lock.try_lock() {
        Ok(()) => launch(&cwd, lock, sections.next().unwrap_or_default()),
        // (Of a folder made a moment ago: whoever looked at it in that moment has its lock.)
        Err(_) => Err("its folder is locked by another".into()),
    };
    started.map_err(|why| format!("{}: {why}", dir.display()))?;
    say(dir.as_os_str().as_bytes())
}

/// Starts the holder of a pane we hold the lock of and are in, and waits until it is up. `cwd` is where we were.
fn launch(cwd: &Path, lock: File, program: &[OsString]) -> Result<(), String> {
    let log = File::create("log").map_err(|e| format!("cannot make the log: {e}"))?;
    let me = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let mut holder = Command::new(me);
    holder.arg("hold").arg(cwd).args(program);
    holder.args(program.is_empty().then(|| var("SHELL").unwrap_or_else(|| "/bin/sh".into())));
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
            let wrong = "view takes a workspace, by its name or its directory, and then perhaps the keys' directory";
            let ([] | [_] | [_, _]) = rest else { return Err(wrong.into()) };
            let named = rest.first().map_or(Some("main"), |name| name.to_str()).ok_or(wrong)?;
            // (The viewer is its session's: for a name the default one, whatever the name's folder is a link to;
            // for a path the folder its workspace is in, made and resolved for that, but not without a terminal.)
            let workspace = workspace(named, wrong)?;
            std::io::stdin().is_terminal().then_some(()).ok_or(view::NO_TERMINAL)?;
            let workspace = if named.starts_with(['/', '.']) { made(&workspace)? } else { workspace };
            let name = workspace.file_name().and_then(|name| name.to_str()).filter(|name| is_name(name));
            let config = var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|home| home.join(".config")));
            let keys = rest.get(1).map(PathBuf::from).or_else(|| config.map(|config| config.join(FOLDER).join("keys")));
            let keys = keys.map(|keys| std::path::absolute(&keys).unwrap_or(keys));
            let (Some(session), Some(name), Some(keys)) = (workspace.parent(), name, keys) else {
                return Err(format!("{}: {wrong}", workspace.display()));
            };
            view::view(session.into(), name.into(), keys)
        }
        Some("layout") => {
            let words = rest.iter().map(|w| w.to_str()).collect::<Option<Vec<_>>>().ok_or("layout edit is not text")?;
            // (The workspace comes before an edit, always: a workspace may be called `pane` or `tab`.)
            let (named, edit) = words.split_first().map_or(("main", &[][..]), |(named, edit)| (*named, edit));
            let layout = layout::command(&workspace(named, &format!("{named}: not a workspace's name"))?, edit)?;
            if edit.is_empty() { say(layout.print().trim_end().as_bytes()) } else { Ok(()) }
        }
        Some("hold") => holder::hold(rest),
        Some("help" | "--help" | "-h") if rest.is_empty() => say(format!("{USAGE}\n\n{}", HELP.trim_end()).as_bytes()),
        _ => Err(format!("usage:\n{USAGE}")),
    }
}

/// The whole of `fux` as a program.
pub fn main() -> ExitCode {
    command(std::env::args_os().skip(1))
}

/// One command line of fux's, for a program that has fux under a word of its own (`koh fux start PATH -- sh`):
/// `args` are the words after that word, the command's own first (`start`), and no program's name before it.
/// `start` runs this program (its own file, not a `fux` that is looked for) with the first word `hold` to hold
/// the pane: a program that carries fux gives its word `hold` to fux too, the word and all that follows it.
/// Called as a `main` is: before the process has a thread, and with nothing else for it to do (a command
/// changes its working directory). The usage and what goes wrong say `fux`, whatever the program is called.
pub fn command(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    sys::own_children();
    let args: Vec<OsString> = args.into_iter().collect();
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
