//! `fux3`: starts panes, names the session directory, waits for a change,
//! lists live panes, and starts the viewer. Everything else is a file.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("fux3 runs on Linux and macOS only");

use fux3::files::{self, Head};
use fux3::holder::{self, DEFAULT_CELL, DEFAULT_HISTORY, MAX_HISTORY, Spec};
use fux3::session::{self, Session, Workspace};
use fux3::{Failure, io};
use std::ffi::OsString;
use std::io::{BufRead, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const USAGE: &str = "\
fux3: a terminal multiplexer where a pane is a directory of files

  fux3 dir                     print the session directory
  fux3 start [WORKSPACE/]NAME [OPTIONS] [-- PROGRAM ARGS...]
                               start a pane in a workspace (default: main;
                               default program: $SHELL)
      --size COLSxROWS         its size (default: this terminal's, else 80x24)
      --cell WxH               a cell's size in pixels, for pictures
                               (default: this terminal's, else 10x20)
      --no-pictures            take no sixel pictures
      --history BYTES          the history file's cap (default 4 MiB, 4096 to 256 MiB)
      --label KEY=VALUE        a label, shown in status (repeatable)
  fux3 ls                      list live panes, as WORKSPACE/NAME
  fux3 wait [WORKSPACE/]NAME [--ms N]
                               wait until the screen changes (default 10000 ms)
  fux3 view [WORKSPACE] [--do LINE]
                               show a workspace's panes, as fux does; with --do,
                               apply one layout command with no viewer running

Every command first clears the leftovers of panes whose holder is gone.
Everything else is a file in the pane's directory: see the README.
";

/// How long `start` waits for the holder's word.
const START_WAIT: Duration = Duration::from_secs(10);
/// The longest `wait` waits.
const MAX_WAIT_MS: u64 = 3_600_000;
/// How often `wait` looks.
const WAIT_STEP: Duration = Duration::from_millis(10);

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        return say(USAGE);
    };
    if command == "__hold" {
        return holder::main(args);
    }
    let outcome = match command.to_str().unwrap_or_default() {
        "dir" => dir(args),
        "start" => start(args),
        "ls" => ls(args),
        "wait" => wait(args),
        "view" => view(args),
        "help" | "--help" | "-h" => return say(USAGE),
        other => Err(Failure::new(format!("no command {other:?}; try fux3 help"))),
    };
    match outcome {
        Ok(code) => code,
        Err(failure) => {
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "fux3: {failure}");
            ExitCode::FAILURE
        }
    }
}

fn say(text: &str) -> ExitCode {
    let mut stdout = std::io::stdout();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

type Outcome = Result<ExitCode, Failure>;

fn text(arg: OsString) -> Result<String, Failure> {
    arg.into_string()
        .map_err(|a| Failure::new(format!("{a:?} is not UTF-8")))
}

fn none_left(mut args: impl Iterator<Item = OsString>) -> Result<(), Failure> {
    match args.next() {
        None => Ok(()),
        Some(arg) => Err(Failure::new(format!("unexpected {arg:?}"))),
    }
}

/// The session, its dead panes cleared.
fn session() -> Result<(Session, Vec<String>), Failure> {
    let session = Session::open()?;
    let live = session.live()?;
    Ok((session, live))
}

fn dir(args: impl Iterator<Item = OsString>) -> Outcome {
    none_left(args)?;
    let (session, _) = session()?;
    Ok(say(&format!("{}\n", session.root.display())))
}

fn ls(args: impl Iterator<Item = OsString>) -> Outcome {
    none_left(args)?;
    let (_, live) = session()?;
    let mut out = String::new();
    for name in live {
        out.push_str(&name);
        out.push('\n');
    }
    Ok(say(&out))
}

fn pair(text: &str, what: &str) -> Result<(u16, u16), Failure> {
    let bad = || Failure::new(format!("{what} {text:?}: it must be NxM"));
    let (a, b) = text.split_once('x').ok_or_else(bad)?;
    Ok((a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?))
}

fn start(mut args: impl Iterator<Item = OsString>) -> Outcome {
    let pane = text(
        args.next()
            .ok_or_else(|| Failure::new("start needs a NAME"))?,
    )?;
    let (workspace, name) = session::split_pane(&pane)?;
    let here = fuxix::terminal::stdin_size();
    let (mut cols, mut rows) =
        here.map_or((80, 24), |(size, _)| (size.cols.get(), size.rows.get()));
    let mut cell = here
        .and_then(|(size, pixels)| {
            let pixels = pixels?;
            let w = pixels.width.get().checked_div(size.cols.get())?;
            let h = pixels.height.get().checked_div(size.rows.get())?;
            fux_vt::CellSize::new(w, h).map(|_| (w, h))
        })
        .unwrap_or(DEFAULT_CELL);
    let (mut pictures, mut history, mut labels) = (true, DEFAULT_HISTORY, Vec::new());
    let mut argv = Vec::new();
    while let Some(arg) = args.next() {
        let mut value = |what: &str| {
            args.next()
                .ok_or_else(|| Failure::new(format!("{what} needs a value")))
                .and_then(text)
        };
        match arg.to_str().unwrap_or_default() {
            "--size" => (cols, rows) = pair(&value("--size")?, "--size")?,
            "--cell" => cell = pair(&value("--cell")?, "--cell")?,
            "--no-pictures" => pictures = false,
            "--history" => {
                let text = value("--history")?;
                history = text
                    .parse()
                    .ok()
                    .filter(|n| (holder::MIN_HISTORY..=MAX_HISTORY).contains(n))
                    .ok_or_else(|| {
                        let least = holder::MIN_HISTORY;
                        Failure::new(format!("--history {text}: {least} to {MAX_HISTORY} bytes"))
                    })?;
            }
            "--label" => {
                labels.push(holder::parse_label(&value("--label")?)?);
                if labels.len() > holder::MAX_LABELS {
                    return Err(Failure::new(format!(
                        "at most {} labels",
                        holder::MAX_LABELS
                    )));
                }
            }
            "--" => {
                argv.extend(args.by_ref());
                break;
            }
            other => return Err(Failure::new(format!("unknown option {other:?}"))),
        }
    }
    holder::checked_size(cols, rows)?;
    if argv.is_empty() {
        let shell = std::env::var_os("SHELL").filter(|s| !s.is_empty());
        argv.push(shell.unwrap_or_else(|| "/bin/sh".into()));
    }
    let session = Session::open()?;
    let cwd = std::env::current_dir().map_err(|e| io("the current directory", e))?;
    let spec = Spec {
        root: session.root.clone(),
        workspace,
        name,
        cols,
        rows,
        cell,
        pictures,
        history,
        cwd,
        labels,
        argv,
    };
    let line = launch(&spec)?;
    match line.strip_prefix("ok ") {
        Some(dir) => Ok(say(&format!("{dir}\n"))),
        None => Err(Failure::new(line.strip_prefix("err ").unwrap_or(&line))),
    }
}

/// Starts the holder in a session of its own, holding nothing of this
/// process's but two pipes, and gives its one line: `ok DIR` or `err WHY`.
fn launch(spec: &Spec) -> Result<String, Failure> {
    let exe = std::env::current_exe().map_err(|e| io("finding this program", e))?;
    let (go_read, mut go) = std::io::pipe().map_err(|e| io("pipe", e))?;
    let (ready, ready_write) = std::io::pipe().map_err(|e| io("pipe", e))?;
    let mut command = Command::new(&exe);
    command
        .args(spec.to_args())
        .current_dir(&spec.root)
        .stdin(Stdio::from(go_read))
        .stdout(Stdio::from(ready_write))
        .stderr(Stdio::null());
    fuxix::process::lead_session(&mut command, false, fuxix::process::Dispositions::Default);
    let child = command
        .spawn()
        .map_err(|e| io(format!("starting {}", exe.display()), e))?;
    drop(command);
    let unknown = |why: String| {
        Failure::new(format!(
            "pane {}/{} may or may not have started: {why}",
            spec.workspace, spec.name
        ))
    };
    go.write_all(b"go\n")
        .and_then(|()| go.flush())
        .map_err(|e| unknown(format!("telling it to go: {e}")))?;
    // The line comes on a thread of its own, so that a holder that says
    // nothing cannot hold this command past its bound.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .spawn(move || {
            let mut line = String::new();
            let got = std::io::BufReader::new(ready.take(64 << 10)).read_line(&mut line);
            let _ = sender.send(got.map(|_| line));
        })
        .map_err(|e| unknown(format!("a thread: {e}")))?;
    let line = receiver
        .recv_timeout(START_WAIT)
        .map_err(|_| unknown(format!("no word within {START_WAIT:?}")))?
        .map_err(|e| unknown(format!("reading its word: {e}")))?;
    drop(child);
    let line = line.trim_end().to_owned();
    match line.is_empty() {
        true => Err(Failure::new("the holder ended without a word")),
        false => Ok(line),
    }
}

/// Line 1 of `name`'s screen, or why not.
fn head(workspace: &Workspace, name: &str) -> Result<Head, Failure> {
    let path = workspace.pane_dir(name).join(files::SCREEN);
    let bytes = files::read_small(&path, files::MAX_SCREEN_FILE)?;
    let text = String::from_utf8_lossy(&bytes);
    Head::parse(text.lines().next().unwrap_or_default())
}

fn wait(mut args: impl Iterator<Item = OsString>) -> Outcome {
    let pane = text(
        args.next()
            .ok_or_else(|| Failure::new("wait needs a NAME"))?,
    )?;
    let (space, name) = session::split_pane(&pane)?;
    let pane = format!("{space}/{name}");
    let mut ms = 10_000u64;
    while let Some(arg) = args.next() {
        match arg.to_str().unwrap_or_default() {
            "--ms" => {
                let value = args.next().map(text).transpose()?.unwrap_or_default();
                ms = value
                    .parse()
                    .ok()
                    .filter(|n| *n <= MAX_WAIT_MS)
                    .ok_or_else(|| Failure::new(format!("--ms {value:?}: 0 to {MAX_WAIT_MS}")))?;
            }
            other => return Err(Failure::new(format!("unknown option {other:?}"))),
        }
    }
    let (session, live) = session()?;
    if !live.contains(&pane) {
        return Err(Failure::new(format!("no live pane {pane}")));
    }
    let workspace = session.workspace(&space, false)?;
    let first = head(&workspace, &name)?;
    let until = Instant::now()
        .checked_add(Duration::from_millis(ms))
        .unwrap_or_else(Instant::now);
    loop {
        match head(&workspace, &name) {
            Ok(now) if now.version != first.version || now.id != first.id => {
                return Ok(say(&format!("{}\n", now.line())));
            }
            Ok(_) => {}
            // Gone: its directory removed by a kill, or its holder dead.
            Err(failure) if !workspace.pane_dir(&name).exists() || !workspace.is_live(&name) => {
                return Err(Failure::new(format!("pane {pane} is gone: {failure}")));
            }
            Err(_) => {}
        }
        if Instant::now() >= until {
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "fux3: no change in {ms} ms; still {}", first.line());
            return Ok(ExitCode::from(1));
        }
        std::thread::sleep(WAIT_STEP);
    }
}

/// Runs `fux3-view`, found beside this program, in this process's place.
fn view(args: impl Iterator<Item = OsString>) -> Outcome {
    use std::os::unix::process::CommandExt;
    let (session, _) = session()?;
    let exe = std::env::current_exe().map_err(|e| io("finding this program", e))?;
    let viewer = exe.with_file_name("fux3-view");
    let error = Command::new(&viewer)
        .args(args)
        .env("FUX3_DIR", &session.root)
        .exec();
    Err(io(format!("running {}", viewer.display()), error))
}
