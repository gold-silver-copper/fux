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
        // (As koh has it for a terminal at the far end of a link: one pane, its keys, the keyboard's flags.)
        Some("show") => match words.get(1..) {
            Some([pane, keys]) if fux::view_pane(pane.into(), keys.into(), 5).is_ok() => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        },
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
    type Test = fn(&Path, &Path) -> Result<(), String>;
    let tests: [(&str, Test); 2] = [
        ("a_program_that_is_not_fux_starts_a_pane_and_is_its_holder", carries),
        ("a_viewer_of_one_pane_asks_its_terminal_nothing_and_ends_with_the_pane", shows),
    ];
    let mut failed = false;
    for (name, test) in tests {
        let wrong = test(&me, &root).err();
        // Whatever became of it, the pane goes: a holder that still lives finds its folder gone and
        // hangs up (and may write a file while the folder is being removed: then once more).
        let end = Instant::now() + Duration::from_secs(5);
        while std::fs::remove_dir_all(&root).is_err() && root.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        match &wrong {
            None => println!("test {name} ... ok"),
            Some(why) => eprintln!("test {name} ... FAILED\n{why}"),
        }
        failed |= wrong.is_some();
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// A file's text, read without waiting.
fn read(path: &Path) -> String {
    let mut text = String::new();
    let _ = fuxix::open(path, false).and_then(|mut file| file.read_to_string(&mut text));
    text
}

/// This program with some words, where nothing of whoever runs the tests is found.
fn carried(me: &Path, root: &Path, words: &[&str]) -> Command {
    let mut command = Command::new(me);
    command.args(words).stdin(Stdio::null()).env_remove("COLUMNS").env_remove("LINES");
    command.env("XDG_RUNTIME_DIR", root).env("XDG_CONFIG_HOME", root).env("HOME", root).env(CARRIED, "1");
    command
}

fn check(right: bool, wrong: String) -> Result<(), String> {
    if right { Ok(()) } else { Err(wrong) }
}

/// `fux::view_pane`, as koh runs it on a pty for a terminal at the far end of a link.
fn shows(me: &Path, root: &Path) -> Result<(), String> {
    let start = |name: &str, says: &str| {
        let pane = root.join("work").join(name);
        let out = carried(me, root, &["fux", "start", &pane.to_string_lossy(), "--", "sh", "-c", says]).output();
        check(out.is_ok_and(|out| out.status.success()), format!("{name} was not started")).map(|()| pane)
    };
    // Two panes of one workspace, and the first is the one shown. No keys are there.
    let (pane, _other) = (start("one", "printf shown; exec sleep 30")?, start("two", "printf another; exec sleep 30")?);
    let keys = root.join("keys");
    let size = fuxix::Winsize { cols: 40, rows: 10, xpixel: 400, ypixel: 200 };
    let (mut master, slave) = fuxix::pty(size).map_err(|e| format!("no pty: {e}"))?;
    let end = |slave: &std::fs::File| slave.try_clone().map(Stdio::from).map_err(|e| e.to_string());
    // (The pty's other end is the viewer's alone once it runs: when the viewer ends, the pty does.)
    let mut viewer = {
        let mut viewer = carried(me, root, &["show", &pane.to_string_lossy(), &keys.to_string_lossy()]);
        viewer.stdin(end(&slave)?).stdout(end(&slave)?).stderr(Stdio::from(slave));
        viewer.spawn().map_err(|e| format!("the viewer was not started: {e}"))?
    };
    // What it writes to its terminal, until `ready`: all of it, as bytes.
    let mut drawn = Vec::new();
    let mut draws = |ready: &mut dyn FnMut(&str) -> bool| {
        let by = Instant::now() + Duration::from_secs(20);
        while Instant::now() < by {
            let mut piece = [0; 4096];
            match master.read(&mut piece) {
                Ok(n @ 1..) => drawn.extend(piece.iter().take(n)),
                // (No more comes of a pty whose other end is closed: the viewer has ended.)
                Ok(_) => return String::from_utf8_lossy(&drawn).into_owned(),
                Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => {
                    return String::from_utf8_lossy(&drawn).into_owned();
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
            if ready(&String::from_utf8_lossy(&drawn)) {
                break;
            }
        }
        String::from_utf8_lossy(&drawn).into_owned()
    };
    let first = draws(&mut |drawn| drawn.contains("shown"));
    // It draws that pane on the alternate screen, and sets the keyboard's flags aside as one that
    // was told the terminal takes some; it asks nothing: not what the terminal is, its colours, its
    // keyboard or a mode. And of the workspace's other pane it draws nothing.
    check(
        first.contains("shown") && first.contains("\x1b[?1049h\x1b[2J") && first.contains("\x1b[>u"),
        format!("{first:?}"),
    )?;
    let asked = ["\x1b[c", "\x1b[?u", ";?\x1b\\", "$p", "another"].into_iter().find(|asked| first.contains(asked));
    check(asked.is_none(), format!("the viewer wrote {asked:?}: {first:?}"))?;
    // Nothing was written of the terminal or of a layout, and no keys were put where there were none.
    let made = ["work/layout", "colours", "keyboard", "keys"].into_iter().find(|name| root.join(name).exists());
    check(made.is_none(), format!("the viewer made {made:?}"))?;
    // The pane ends, and the viewer with it, by itself: with the last screen drawn, and the terminal
    // left as it is (whoever has the terminal puts it back).
    let mut ctl = fuxix::open(pane.join("ctl"), true).map_err(|e| format!("ctl: {e}"))?;
    ctl.write_all(b"kill\n").map_err(|e| format!("ctl: {e}"))?;
    let last = draws(&mut |_| false);
    let by = Instant::now() + Duration::from_secs(5);
    while matches!(viewer.try_wait(), Ok(None)) && Instant::now() < by {
        std::thread::sleep(Duration::from_millis(5));
    }
    let ended = viewer.try_wait().ok().flatten();
    let _ = viewer.kill().and_then(|()| viewer.wait());
    check(ended.is_some_and(|ended| ended.success()), format!("the viewer did not end with its pane: {ended:?}"))?;
    check(last.contains("killed 1") && !last.contains("\x1b[?1049l"), format!("the end: {last:?}"))
}

fn carries(me: &Path, root: &Path) -> Result<(), String> {
    let carried = |words: &[&str]| -> Result<Output, String> {
        let mut command = carried(me, root, &["fux"]);
        command.args(words).output().map_err(|e| format!("{command:?}: {e}"))
    };
    let said = |out: &Output| format!("{:?}", (out.status, String::from_utf8_lossy(&out.stderr)));

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
