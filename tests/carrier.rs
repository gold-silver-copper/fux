//! A program that is not fux and carries it, as koh does: fux's command line is under its word
//! `fux`, and the word `hold`, by which fux starts a pane's holder as the program it runs in, is
//! fux's too. Run by `cargo test` it is the test of that (`main` is its own: no test harness), and
//! it runs whatever words it is given there: they are a filter's, of the tests that have names.
#![allow(clippy::unwrap_used)]

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Output, Stdio};
use std::time::{Duration, Instant};

/// A variable of the test's own: set, this program was run by the test (or by the fux in it), and its
/// words are a program's. (Not set, `cargo test hold` has given it a filter and no word of fux's.)
const CARRIED: &str = "CARRIED";

fn main() -> ExitCode {
    if std::env::var_os(CARRIED).is_none() {
        return test();
    }
    let words: Vec<OsString> = std::env::args_os().skip(1).collect();
    match words.first().and_then(|word| word.to_str()) {
        Some("fux") => fux::command(words.into_iter().skip(1)),
        Some("hold") => fux::command(words),
        _ => ExitCode::from(2),
    }
}

/// How long the test has, all of it. What it runs is fux, which ends, and a `ps`; the pane's
/// program ends by itself in half of this, and its holder with it.
const LIMIT: Duration = Duration::from_secs(60);

fn test() -> ExitCode {
    std::thread::spawn(|| {
        std::thread::sleep(LIMIT);
        eprintln!("carrier: not over in {LIMIT:?}");
        std::process::exit(124);
    });
    let me = std::env::current_exe().unwrap();
    let tmp = std::env::temp_dir();
    let root = tmp.canonicalize().unwrap_or(tmp).join(format!("fux-carrier-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let wrong = carries(&me, &root).err();
    // Whatever became of it, the pane goes: a holder that still lives finds its folder gone and
    // hangs up (and may write a file while the folder is being removed: then once more).
    let end = Instant::now() + Duration::from_secs(5);
    while std::fs::remove_dir_all(&root).is_err() && root.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    let Some(why) = wrong else {
        println!("test a_program_that_is_not_fux_starts_a_pane_and_is_its_holder ... ok");
        return ExitCode::SUCCESS;
    };
    eprintln!("test a_program_that_is_not_fux_starts_a_pane_and_is_its_holder ... FAILED\n{why}");
    ExitCode::FAILURE
}

/// A file's text, read without waiting.
fn read(path: &Path) -> String {
    let mut text = String::new();
    let _ = fuxix::open(path, false).and_then(|mut file| file.read_to_string(&mut text));
    text
}

fn carries(me: &Path, root: &Path) -> Result<(), String> {
    // This program under its word `fux`, where nothing of whoever runs the tests is found.
    let carried = |words: &[&str]| -> Result<Output, String> {
        let mut command = Command::new(me);
        command.arg("fux").args(words).stdin(Stdio::null()).env_remove("COLUMNS").env_remove("LINES");
        command.env("XDG_RUNTIME_DIR", root).env("XDG_CONFIG_HOME", root).env("HOME", root).env(CARRIED, "1");
        command.output().map_err(|e| format!("{command:?}: {e}"))
    };
    let said = |out: &Output| format!("{:?}", (out.status, String::from_utf8_lossy(&out.stderr)));
    let check = |right: bool, wrong: String| if right { Ok(()) } else { Err(wrong) };

    // `start` under that word starts a pane: the folder it prints is the one named, by a path.
    let pane = root.join("work").join("pane");
    let name = pane.to_str().ok_or("the temporary directory is not text")?;
    let out = carried(&["start", name, "--", "sleep", "30"])?;
    check(out.status.success(), format!("start failed: {}", said(&out)))?;
    check(String::from_utf8_lossy(&out.stdout).trim_end() == name, format!("start printed {:?}", out.stdout))?;

    // The pane's holder is this program, with the word `hold`: there is no `fux` to be it.
    let status = read(&pane.join("status"));
    let holder = status.lines().find_map(|line| line.strip_prefix("holder ")?.parse::<u32>().ok());
    let holder = holder.ok_or(format!("no holder in status: {status:?}"))?;
    let mut ps = Command::new("ps");
    ps.args(["-ww", "-o", "args=", "-p", &holder.to_string()]).env_remove("COLUMNS").stdin(Stdio::null());
    let line = String::from_utf8_lossy(&ps.output().map_err(|e| format!("ps: {e}"))?.stdout).into_owned();
    check(line.starts_with(&format!("{} hold ", me.display())), format!("the holder {holder} is: {line:?}"))?;
    check(me.file_name().is_some_and(|name| name != "fux"), format!("{}: the test is to be no fux", me.display()))?;

    // It holds the pane's lock (the `start` that took it has ended), for as long as it lives.
    // (`kill` hangs the program up, which ends a `sleep`, and `wait` returns when the lock is free.)
    let folder = fuxix::open(&pane, false).map_err(|e| format!("{name}: {e}"))?;
    check(folder.try_lock_shared().is_err(), "nobody holds the lock of a pane that runs".into())?;
    let mut ctl = fuxix::open(pane.join("ctl"), true).map_err(|e| format!("{name}/ctl: {e}"))?;
    ctl.write_all(b"kill\n").map_err(|e| format!("{name}/ctl: {e}"))?;
    let out = carried(&["wait", name])?;
    let ended = String::from_utf8_lossy(&out.stdout).into_owned();
    check(out.status.success() && ended.ends_with("state killed 1\n"), format!("wait: {ended:?} {}", said(&out)))?;
    check(folder.try_lock_shared().is_ok(), "the lock is still held after the holder has gone".into())?;

    // The usage names fux, whatever the program is: after the program's own word it reads as it is typed.
    let out = carried(&["nothing"])?;
    let usage = String::from_utf8_lossy(&out.stderr).into_owned();
    check(!out.status.success() && usage.starts_with("fux: usage:\nfux dir "), format!("usage: {}", said(&out)))
}
