//! fux3: a terminal multiplexer that is a view over directories. A pane is a
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
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

const USAGE: &str = "\
fux3 dir                         print the session directory (FUX3C_DIR sets it)
fux3 start [NAME] [-- PROGRAM…]  start a pane; NAME is PANE, WORKSPACE/PANE or WORKSPACE/
fux3 wait PANE-DIRECTORY         wait until the pane's holder is gone; print its status
fux3 view [WORKSPACE]            show a workspace's panes in tiles and tabs (default: main)
fux3 layout [EDIT…]              print the layout of workspace $FUX3C_WS (default: main), or edit it
The files of a pane, and everything else, are in the README.";

/// The session directory: `$FUX3C_DIR`, else `$XDG_RUNTIME_DIR/fux3c`, else `~/.cache/fux3c`.
fn session() -> Result<PathBuf, String> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    var("FUX3C_DIR")
        .or_else(|| var("XDG_RUNTIME_DIR").map(|d| d.join("fux3c")))
        .or_else(|| var("HOME").map(|d| d.join(".cache/fux3c")))
        .ok_or_else(|| "no session directory: set FUX3C_DIR".into())
}

/// A name fux3 will make: letters, digits, `.`, `_` and `-`, not starting with a dot.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn make_dir(path: &Path) -> std::io::Result<()> {
    DirBuilder::new().mode(0o700).create(path)
}

fn start(args: &[OsString]) -> Result<(), String> {
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let (names, program) = (args.get(..split).unwrap_or(&[]), args.get(split + 1..).unwrap_or(&[]));
    let name = match names {
        [] => "main/".to_string(),
        [name] => name.to_string_lossy().into_owned(),
        _ => return Err("start takes one NAME, then -- and the program".into()),
    };
    let (workspace, pane) = name.split_once('/').unwrap_or(("main", &name));
    if !is_name(workspace) || !(pane.is_empty() || is_name(pane)) {
        return Err(format!(
            "{name}: a name is letters, digits, '.', '_' and '-', at most 64, not starting with a dot"
        ));
    }
    let workspace = session()?.join(workspace);
    DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&workspace)
        .map_err(|e| format!("{}: {e}", workspace.display()))?;
    // Making the directory is what claims the name: two starts cannot both make it.
    let dir = if pane.is_empty() {
        (1..10000)
            .map(|n| workspace.join(n.to_string()))
            .find(|dir| make_dir(dir).is_ok())
            .ok_or("no free number for a pane")?
    } else {
        let dir = workspace.join(pane);
        match make_dir(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
                return Err(format!("{}: {e}", dir.display()));
            }
            _ => dir,
        }
    };
    let program: Vec<OsString> = match program {
        [] => vec![std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into())],
        program => program.to_vec(),
    };
    // The lock on the directory is the proof that a holder lives: the kernel
    // drops it when the holder is gone, however it went. We take it here and
    // the holder inherits it. A `fux3 wait` may hold it for an instant.
    let lock = File::open(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let locked = (0..20).any(|i| {
        std::thread::sleep(Duration::from_millis(5 * u64::from(i > 0)));
        lock.try_lock().is_ok()
    });
    if !locked {
        return Err(format!("{}: a holder already keeps this pane", dir.display()));
    }
    // A pane left by an earlier program is started again in place. Its old
    // status goes first: a new one is how we know the holder is up.
    let _ = fs::remove_file(dir.join("status"));
    let log = File::options()
        .append(true)
        .create(true)
        .open(dir.join("log"))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let me = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let mut holder = Command::new(me);
    holder.arg("hold").arg(cwd).args(&program).current_dir(&dir);
    holder.stdin(Stdio::from(lock)).stdout(Stdio::null()).stderr(log);
    sys::new_session(&mut holder, false);
    let mut holder = holder.spawn().map_err(|e| format!("cannot start the holder: {e}"))?;
    loop {
        if dir.join("status").exists() {
            println!("{}", dir.display());
            return Ok(());
        }
        if holder.try_wait().map_err(|e| e.to_string())?.is_some() {
            let log = fs::read_to_string(dir.join("log")).unwrap_or_default();
            return Err(format!(
                "{}: {}",
                dir.display(),
                log.lines().last().unwrap_or("the holder ended without a word").trim_start_matches("fux3: ")
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait(args: &[OsString]) -> Result<(), String> {
    let [dir] = args else {
        return Err("wait takes one pane directory".into());
    };
    let dir = Path::new(dir);
    // The holder locks its directory for as long as it lives; the kernel
    // drops the lock when it is gone, however it went.
    let lock = File::open(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    lock.lock_shared().map_err(|e| format!("{}: {e}", dir.display()))?;
    drop(lock);
    print!("{}", fs::read_to_string(dir.join("status")).unwrap_or_default());
    Ok(())
}

fn run(args: &[OsString]) -> Result<(), String> {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().and_then(|a| a.to_str()) {
        Some("dir") => {
            println!("{}", session()?.display());
            Ok(())
        }
        Some("start") => start(rest),
        Some("wait") => wait(rest),
        Some("view") => {
            let workspace = rest.first().map_or("main".into(), |name| name.to_string_lossy().into_owned());
            if rest.len() > 1 || !is_name(&workspace) {
                return Err("view takes one workspace's name".into());
            }
            let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
            let config = var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|home| home.join(".config")));
            let keys = var("FUX3C_KEYS").or_else(|| config.map(|config| config.join("fux3c/keys")));
            view::view(session()?, workspace, keys.ok_or("no keys directory: set FUX3C_KEYS")?)
        }
        Some("layout") => {
            let workspace = std::env::var("FUX3C_WS").ok().filter(|name| is_name(name)).unwrap_or("main".into());
            let words: Vec<String> = rest.iter().map(|word| word.to_string_lossy().into_owned()).collect();
            let text =
                layout::command(&session()?.join(workspace), &words.iter().map(String::as_str).collect::<Vec<_>>())?;
            if words.is_empty() {
                print!("{text}");
            }
            Ok(())
        }
        Some("hold") => match rest.split_first() {
            Some((cwd, program)) => holder::hold(program, cwd),
            None => Err("hold is started by `fux3 start`".into()),
        },
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(format!("usage:\n{USAGE}")),
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("fux3: {why}");
            ExitCode::FAILURE
        }
    }
}
