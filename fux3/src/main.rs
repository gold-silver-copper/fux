//! `fux3`: starts panes, names the session directory, waits for a change,
//! lists live panes, and starts the viewer. Everything else is a file.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("fux3 runs on Linux and macOS only");

use fux3::files::{self, Head};
use fux3::holder::{self, DEFAULT_CELL, DEFAULT_HISTORY, MAX_HISTORY, Spec};
use fux3::session::{self, Session};
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
      --env NAME=VALUE         a variable for the program, kept in its recipe
                               (repeatable)
      --no-recipe              write no recipe (a pane nobody starts again)
      --workspace-id ID        refuse unless the workspace's directory is the
                               one ID names: a workspace removed and made
                               again is another (the viewer's starts use it)
    With no PROGRAM, WORKSPACE/.recipes/NAME runs if there is one: the
    recipe each start writes, an sh script that starts the program again,
    with the size and labels of that start unless given here.
  fux3 ls                      list live panes, as WORKSPACE/NAME
  fux3 wait [WORKSPACE/]NAME [--ms N]
                               wait until the screen changes (default 10000 ms)
  fux3 put [WORKSPACE/]NAME [TEXT]
                               write TEXT (else stdin, at most 512 bytes) to the
                               pane's in, whole or refused, never waiting
  fux3 view [WORKSPACE] [--do LINE]
                               show a workspace's panes, as fux does; with --do,
                               make one layout command's edit of the layout
                               file, with or without a viewer running

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
        // `start`'s own arguments again, after its terminal's size; the
        // variables' values come on the pipe, as arguments are public.
        let mut input = std::io::stdin().lock();
        let mut go = [0; 3];
        if input.read_exact(&mut go).is_err() || &go != b"go\n" {
            return ExitCode::FAILURE;
        }
        let spec = spec(args, &mut input);
        drop(input);
        return holder::main(spec);
    }
    let outcome = match command.to_str().unwrap_or_default() {
        "dir" => dir(args),
        "start" => start(args),
        "ls" => ls(args),
        "wait" => wait(args),
        "view" => view(args),
        "put" => put(args),
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
    say_path(&session.root)
}

/// Prints a path as the system names it, bytes and all.
fn say_path(path: &std::path::Path) -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let mut out = path.as_os_str().as_bytes().to_vec();
    out.push(b'\n');
    let mut stdout = std::io::stdout();
    match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) => Err(io("stdout", e)),
    }
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

/// Starts a pane: its holder reads these arguments itself ([`spec`]), so
/// there is one reading of them; this passes its terminal's size and cell
/// first (`-` with none) and prints the pane's directory.
fn start(mut args: impl Iterator<Item = OsString>) -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    let pane = (args.next()).ok_or_else(|| Failure::new("start needs a NAME"))?;
    let (workspace, name) = session::split_pane(&text(pane.clone())?)?;
    let session = Session::open()?;
    let here = fuxix::terminal::stdin_size();
    let size = here.map(|(size, _)| format!("{}x{}", size.cols, size.rows));
    let cell = here.and_then(|(size, pixels)| {
        let pixels = pixels?;
        let w = pixels.width.get().checked_div(size.cols.get())?;
        let h = pixels.height.get().checked_div(size.rows.get())?;
        fux_vt::CellSize::new(w, h).map(|_| format!("{w}x{h}"))
    });
    let or_none = |text: Option<String>| OsString::from(text.unwrap_or_else(|| "-".to_owned()));
    let mut public = vec![or_none(size), or_none(cell), pane];
    let mut private = Vec::new();
    // As the holder reads them: an option's value is never an option, so
    // nothing after a `--` that is a value is taken for the program.
    const VALUED: [&str; 5] = ["--size", "--cell", "--history", "--label", "--workspace-id"];
    while let Some(arg) = args.next() {
        let (env, program) = (arg == "--env", arg == "--");
        let valued = VALUED.iter().any(|option| arg == *option);
        public.push(arg);
        if program {
            public.extend(args);
            break;
        }
        if env {
            let value = (args.next()).ok_or_else(|| Failure::new("--env needs NAME=VALUE"))?;
            private.extend_from_slice(value.as_bytes());
            private.push(0);
        } else if valued {
            public.extend(args.next());
        }
    }
    let shown = format!("{workspace}/{name}");
    let line = launch(&shown, public, private)?;
    if line != "ok" {
        return Err(Failure::new(line.strip_prefix("err ").unwrap_or(&line)));
    }
    say_path(&session.root.join(&workspace).join(&name))
}

/// What a start's arguments ask for, read in the holder: the starter's
/// terminal's size and cell, the pane, the options, the program. Each
/// `--env`'s `NAME=VALUE` comes from `input`, ended by a NUL.
fn spec(
    mut args: impl Iterator<Item = OsString>,
    input: &mut impl BufRead,
) -> Result<Spec, Failure> {
    let mut here = |what: &str| -> Result<Option<(u16, u16)>, Failure> {
        let text = text(
            args.next()
                .ok_or_else(|| Failure::new("holder arguments"))?,
        )?;
        (text != "-").then(|| pair(&text, what)).transpose()
    };
    let (here, here_cell) = (here("size")?, here("cell")?);
    let pane = text(
        args.next()
            .ok_or_else(|| Failure::new("start needs a NAME"))?,
    )?;
    let (workspace, name) = session::split_pane(&pane)?;
    let (mut size, mut workspace_id, mut no_recipe) = (None, None, false);
    let mut cell = here_cell.unwrap_or(DEFAULT_CELL);
    let (mut pictures, mut history, mut labels) = (true, DEFAULT_HISTORY, Vec::new());
    let (mut env, mut argv) = (Vec::new(), Vec::new());
    while let Some(arg) = args.next() {
        let mut value = |what: &str| {
            args.next()
                .ok_or_else(|| Failure::new(format!("{what} needs a value")))
                .and_then(text)
        };
        match arg.to_str().unwrap_or_default() {
            "--size" => size = Some(pair(&value("--size")?, "--size")?),
            "--workspace-id" => workspace_id = Some(value("--workspace-id")?),
            "--cell" => cell = pair(&value("--cell")?, "--cell")?,
            "--no-pictures" => pictures = false,
            "--no-recipe" => no_recipe = true,
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
            "--env" => {
                use std::os::unix::ffi::OsStringExt;
                let mut bytes = Vec::new();
                let most = u64::try_from(fux3::recipe::MAX_RECIPE).unwrap_or(u64::MAX);
                let read = input.by_ref().take(most).read_until(0, &mut bytes);
                if read.is_err() || bytes.pop() != Some(0) {
                    return Err(Failure::new(
                        "--env needs NAME=VALUE, shorter than a recipe",
                    ));
                }
                let at = (bytes.iter().position(|b| *b == b'='))
                    .ok_or_else(|| Failure::new("--env needs NAME=VALUE"))?;
                let key = String::from_utf8_lossy(bytes.get(..at).unwrap_or_default()).into_owned();
                fux3::recipe::check_env_name(&key)?;
                let value = bytes
                    .get(at.saturating_add(1)..)
                    .unwrap_or_default()
                    .to_vec();
                env.push((key, OsString::from_vec(value)));
                if env.len() > holder::MAX_ENV {
                    return Err(Failure::new(format!("at most {} --env", holder::MAX_ENV)));
                }
            }
            "--" => {
                argv.extend(args.by_ref());
                break;
            }
            other => return Err(Failure::new(format!("unknown option {other:?}"))),
        }
    }
    let session = Session::open()?;
    let cwd = std::env::current_dir().map_err(|e| io("the current directory", e))?;
    // With no program, a recipe of the name runs; else the shell, whose
    // recipe is written as any program's is.
    let saved = (session.root.join(&workspace))
        .join(fux3::recipe::DIR)
        .join(&name);
    // A recipe there is run only if it is a plain file of a recipe's size;
    // anything else there is refused, never taken for "no recipe".
    let recipe = match std::fs::symlink_metadata(&saved) {
        _ if !argv.is_empty() => None,
        Ok(_) => Some(
            files::read_small(&saved, fux3::recipe::MAX_RECIPE)
                .map_err(|f| Failure::new(format!("{f}: not a recipe")))?,
        ),
        Err(_) => None,
    };
    if let Some(recipe) = &recipe {
        if !env.is_empty() {
            return Err(Failure::new(format!(
                "--env with no program: the recipe {} sets its own variables; give the program to change them",
                saved.display()
            )));
        }
        // The size and labels of the start that wrote it, unless given.
        let kept = fux3::recipe::kept(recipe);
        size = size.or(kept.size);
        let given = |key: &String| labels.iter().any(|(k, _)| k == key);
        let unset: Vec<_> = kept.labels.into_iter().filter(|(k, _)| !given(k)).collect();
        labels.extend(unset);
        argv = vec!["/bin/sh".into(), saved.clone().into()];
    } else if argv.is_empty() {
        let shell = std::env::var_os("SHELL").filter(|s| !s.is_empty());
        argv.push(shell.unwrap_or_else(|| "/bin/sh".into()));
    }
    let (cols, rows) = size.or(here).unwrap_or((80, 24));
    holder::checked_size(cols, rows)?;
    // Refused before anything is made if it would be too large or could
    // not run; written once the pane runs.
    let recipe = match recipe {
        Some(_) => None,
        None if no_recipe => None,
        None => {
            let kept = fux3::recipe::Kept {
                size: Some((cols, rows)),
                labels: labels.clone(),
            };
            Some(fux3::recipe::render(&cwd, &env, &argv, &kept)?)
        }
    };
    // The program starts in the starter's directory; the holder leaves it.
    std::env::set_current_dir(&session.root).map_err(|e| io(session.root.display(), e))?;
    Ok(Spec {
        session,
        workspace,
        workspace_id,
        name,
        cols,
        rows,
        cell,
        pictures,
        history,
        cwd,
        labels,
        env,
        recipe,
        argv,
    })
}

/// Starts the holder of pane `shown` in a session of its own, holding
/// nothing of this process's but two pipes, and gives its one line: `ok`
/// or `err WHY`. `private` goes to it on its pipe, never as an argument.
fn launch(shown: &str, public: Vec<OsString>, private: Vec<u8>) -> Result<String, Failure> {
    let exe = std::env::current_exe().map_err(|e| io("finding this program", e))?;
    let (go_read, mut go) = std::io::pipe().map_err(|e| io("pipe", e))?;
    let (ready, ready_write) = std::io::pipe().map_err(|e| io("pipe", e))?;
    let mut command = Command::new(&exe);
    command
        .arg("__hold")
        .args(public)
        .stdin(Stdio::from(go_read))
        .stdout(Stdio::from(ready_write))
        .stderr(Stdio::null());
    fuxix::process::lead_session(&mut command, false, fuxix::process::Dispositions::Default);
    let child = command
        .spawn()
        .map_err(|e| io(format!("starting {}", exe.display()), e))?;
    drop(command);
    let unknown =
        |why: String| Failure::new(format!("pane {shown} may or may not have started: {why}"));
    // On a thread of its own, so that a holder that neither reads nor
    // says anything cannot hold this command past its bound. A holder
    // that ended early says why in its line, so a failed write is not.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .spawn(move || {
            let _ = go.write_all(b"go\n").and_then(|()| go.write_all(&private));
            drop(go);
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

/// Line 1 of the screen of the pane held open as `pane`, or why not.
fn head(pane: &std::fs::File) -> Result<Head, Failure> {
    let bytes = files::read_at(pane, files::SCREEN, files::MAX_SCREEN_FILE)?;
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
    // The pane is opened once: it is waited on wherever it is moved.
    let held = session.workspace(&space, false)?.pane(&name)?;
    let first = head(&held)?;
    let until = Instant::now()
        .checked_add(Duration::from_millis(ms))
        .unwrap_or_else(Instant::now);
    loop {
        match head(&held) {
            Ok(now) if now.version != first.version || now.id != first.id => {
                return Ok(say(&format!("{}\n", now.line())));
            }
            Ok(_) => {}
            // Gone: taken out of its name by a kill, or its holder dead.
            Err(failure) if !session::try_lock(&held).is_ok_and(|dead| !dead) || !listed(&held) => {
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

/// Whether the pane held open as `pane` is listed under a pane's name.
fn listed(pane: &std::fs::File) -> bool {
    session::place(pane).is_some_and(|(_, name)| session::check_name(&name).is_ok())
}

/// The most bytes `put` writes: one write of at most this many reaches a
/// FIFO whole.
const MAX_PUT: usize = files::PIPE_BUF;

/// Writes TEXT (or stdin) to a pane's `in` without ever waiting: whole, or
/// refused saying why.
fn put(mut args: impl Iterator<Item = OsString>) -> Outcome {
    use fuxix::file::at;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;
    let pane = text(
        args.next()
            .ok_or_else(|| Failure::new("put needs a NAME"))?,
    )?;
    let (space, name) = session::split_pane(&pane)?;
    let bytes = match args.next() {
        Some(arg) => arg.as_bytes().to_vec(),
        None => {
            let mut bytes = Vec::new();
            let limit = u64::try_from(MAX_PUT).unwrap_or(u64::MAX).saturating_add(1);
            std::io::stdin()
                .take(limit)
                .read_to_end(&mut bytes)
                .map_err(|e| io("stdin", e))?;
            bytes
        }
    };
    none_left(args)?;
    if bytes.len() > MAX_PUT {
        return Err(Failure::new(format!(
            "more than {MAX_PUT} bytes: nothing was written; use ctl's send"
        )));
    }
    // No session lock and no clearing here: `put` never waits. The pane
    // is opened once, and everything after is asked of that directory:
    // a new pane that takes the name meanwhile is never typed into.
    let session = Session::open()?;
    let shown = format!("{space}/{name}");
    let none = |_| Failure::new(format!("no pane {shown}"));
    let pane = session
        .workspace(&space, false)
        .map_err(none)?
        .pane(&name)
        .map_err(none)?;
    // A lock that can't be asked about is no proof of a holder.
    if session::try_lock(&pane)? {
        return Err(Failure::new(format!(
            "pane {shown} has no holder: it is dead"
        )));
    }
    let file = at::open(&pane, files::IN, at::WRITE | fuxix::file::NONBLOCK).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Failure::new(format!("no pane {shown}"))
        } else if e.raw_os_error() == Some(ENXIO) {
            Failure::new(format!("pane {shown} has no holder: it is dead"))
        } else {
            io(&shown, e)
        }
    })?;
    if !file.metadata().is_ok_and(|m| m.file_type().is_fifo()) {
        return Err(Failure::new(format!("{shown}/in is not a FIFO")));
    }
    // Only to a program `status` says is running: a write to `in` only
    // queues, and the holder drops what no program will read.
    let status = files::read_at(&pane, files::STATUS, 64 << 10)
        .map_err(|f| Failure::new(format!("pane {shown}: {f}: nothing was written")))?;
    let status = files::parse_status(&String::from_utf8_lossy(&status));
    let state = files::status_value(&status, "state").unwrap_or("unknown");
    if state != "running" {
        return Err(Failure::new(format!(
            "pane {shown}'s program is not running ({state}): nothing was written"
        )));
    }
    match (&file).write(&bytes) {
        Ok(n) if n == bytes.len() => Ok(ExitCode::SUCCESS),
        Ok(n) => Err(Failure::new(format!(
            "only {n} of {} bytes were written",
            bytes.len()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(Failure::new(format!(
            "pane {shown}'s input is full: nothing was written"
        ))),
        Err(e) => Err(io(&shown, e)),
    }
}

/// What opening a FIFO to write without waiting says when nobody reads it
/// (the same number on Linux and macOS).
const ENXIO: i32 = 6;

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
