//! fux's server on Windows (WINDOWS.md, PR 7), driven as its clients
//! drive it, over its pipe, by a small client of the wire protocol (fux's
//! own client: `tests/console.rs`). Each test checks one property, against
//! the real `fux` binary on a private socket, and has a deadline.
//!
//! - A server killed outright leaves its panes running, and a new server
//!   takes the run back and drives them: typing, resizing, closing
//!   (spike S3's probe, against real fux).
//! - The signals reach what D-136 says, on a shell pane and a program
//!   pane: INT and QUIT as keys, KILL of a shell's job, `only_shell`,
//!   TERM ending a program's job with fux's own code, HUP refused.
//! - A pane's own process killed outright: its server ends the program
//!   through its copy of the program's handle, and tells its real end
//!   (D-134).
//! - With no pinned ConPTY to be had, Windows' built-in one runs a
//!   program to its end, its exit told, and the server says once what the
//!   built-in loses (D-130).
//! - A live server, and a pipe's name held by anything else, are refused,
//!   and never joined (D-138).
//! - A shell's directory is read from its process (D-137).
//!
//! One thread polls at a time in a process (WINDOWS.md, PR 4): the tests
//! take turns.
#![cfg(windows)]
use fux_wire::answer::{PaneAnswer, PaneResults};
use fux_wire::command::{
    AnyRef, CaptureView, Command, HistoryBytes, Input, PaneOptions, PictureBytes, Program, Request,
    Resized, Screen, ScrollLines, Start, To,
};
use fux_wire::id::PaneId;
use fux_wire::protocol::{
    Asked, Body, Context, Decoder, Frame, Greeting, Hello, Reply, Response, Role, ToCommand,
};
use fuxix::poll::{Events, PollFd};
use fuxix::process::Signal;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Std, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

type Outcome = Result<(), String>;

const FUX: &str = env!("CARGO_BIN_EXE_fux");

fn turn() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a test found, in its log.
#[expect(
    clippy::print_stderr,
    reason = "what the tests found goes to their log"
)]
fn say(what: &str) {
    eprintln!("{what}");
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn text_of(error: std::fmt::Error) -> String {
    error.to_string()
}

fn after(duration: Duration) -> Result<Instant, String> {
    Instant::now()
        .checked_add(duration)
        .ok_or_else(|| "a deadline".to_owned())
}

/// A folder of the test's own, under cargo's temporary folder.
fn scratch(name: &str) -> Result<PathBuf, String> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("win-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(text)?;
    Ok(dir)
}

/// A `fux __serve` on `socket`, once it said it listens: the process the
/// test started (which, taking a run back, ends once the run's own fux
/// starts) and its log.
struct Served {
    child: Child,
    log: PathBuf,
}

/// The package of the pinned ConPTY, as fuxix's `fetch` finds one put in
/// its folder by hand: CI's cached copy (`FUXIX_CONPTY_PACKAGE`), so no
/// test downloads it.
const PACKAGE: &str = "microsoft.windows.console.conpty.1.25.260930003.nupkg";

/// fux's own folder in `home`, made private, as a server wants it: what
/// cargo's temporary folder passes on need not be.
fn state(home: &Path) -> Result<PathBuf, String> {
    let dir = home.join(fux_server::socket::folder().map_err(text)?);
    match fuxix::file::private_dir(&dir) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => Err(text(error)),
        Ok(()) | Err(_) => Ok(dir),
    }
}

/// `fux __serve` on `socket`, its fux folder (`LOCALAPPDATA`) the test's
/// own `home`, which holds CI's ConPTY package if there is one.
fn serve(socket: &Path, log: &Path) -> Result<Served, String> {
    let home = socket
        .parent()
        .and_then(Path::parent)
        .ok_or("the socket's folder")?;
    let conpty = state(home)?.join("conpty");
    if let Some(package) = std::env::var_os("FUXIX_CONPTY_PACKAGE")
        && !conpty.exists()
    {
        std::fs::create_dir_all(&conpty).map_err(text)?;
        std::fs::copy(package, conpty.join(PACKAGE)).map_err(text)?;
    }
    let file = std::fs::File::create(log).map_err(text)?;
    let mut child = Std::new(FUX)
        .env("LOCALAPPDATA", home)
        .args(["__serve".as_ref(), socket.as_os_str(), "1000".as_ref()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(file)
        .spawn()
        .map_err(text)?;
    let stdout = child.stdout.take().ok_or("its stdout")?;
    let (said, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // What it says first is for its starter to show.
        let mut stdout = BufReader::new(stdout);
        let mut line = String::new();
        while {
            line.clear();
            matches!(stdout.read_line(&mut line), Ok(n) if n > 0) && line.starts_with("notice: ")
        } {}
        let _ = said.send(line);
    });
    let line = heard.recv_timeout(Duration::from_secs(30)).map_err(|_| {
        let log = std::fs::read_to_string(log).unwrap_or_default();
        format!("no server started in 30 s; its log:\n{log}")
    })?;
    if line != "fux server: listening\n" {
        let log = std::fs::read_to_string(log).unwrap_or_default();
        return Err(format!("the server said {line:?}; its log:\n{log}"));
    }
    say(&format!(
        "server {} listens on {}",
        child.id(),
        socket.display()
    ));
    Ok(Served {
        child,
        log: log.to_owned(),
    })
}

/// What the test started goes with it, should it fail early.
impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Served {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// One request on its own connection, answered within `wait`.
fn ask(socket: &Path, request: Request, wait: Duration) -> Result<Reply, String> {
    let deadline = after(wait)?;
    let stream = fux_server::socket::connect(socket).map_err(|e| format!("connect: {e}"))?;
    let mut out = Hello::ours(Role::Command).encode();
    let asked = Asked {
        after: None,
        request,
        context: Context {
            of: None,
            cwd: std::env::current_dir().map_err(text)?,
            pane: None,
            server: None,
            client: None,
        },
    };
    out.extend(asked.encode());
    let mut sent = 0;
    let mut decoder = Decoder::default();
    let (mut greeted, mut parts) = (false, Vec::new());
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        if !greeted {
            match decoder.frame::<Greeting>() {
                Ok(Some(Greeting::Hello(_))) => greeted = true,
                Ok(Some(Greeting::Exit(why))) => return Err(format!("refused: {why}")),
                Ok(None) => {}
                Err(e) => return Err(format!("its hello: {e}")),
            }
        }
        if greeted {
            loop {
                match decoder.frame::<ToCommand>() {
                    Ok(Some(ToCommand::Part(bytes))) => parts.extend_from_slice(&bytes),
                    Ok(Some(ToCommand::PartsEnd)) => {
                        let reply = fux_wire::wire::Reader::new(&parts).whole::<Reply>();
                        return reply.map_err(|e| format!("its reply: {e:?}"));
                    }
                    Ok(Some(ToCommand::Exit(why))) => return Err(format!("it ended: {why}")),
                    Ok(None) => break,
                    Err(e) => return Err(format!("its reply: {e}")),
                }
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("no answer in time".to_owned());
        }
        let mut wanted = Events::IN;
        if sent < out.len() {
            wanted |= Events::OUT;
        }
        let mut fds = [PollFd::new(&stream, wanted)];
        fuxix::poll::poll(&mut fds, Some(left)).map_err(text)?;
        let ready = fds.first().map_or(Events::NONE, PollFd::revents);
        if ready.intersects(Events::OUT) {
            let rest = out.get(sent..).unwrap_or_default();
            let wrote = fuxix::io::write_what_fits(&stream, rest).map_err(text)?;
            sent = sent.saturating_add(wrote);
        }
        if ready.intersects(Events::IN | Events::HUP | Events::ERR) {
            match fuxix::io::read(&stream, &mut buffer) {
                Ok(0) => return Err("the server closed the connection".to_owned()),
                Ok(n) => decoder.push(buffer.get(..n).unwrap_or_default()),
                Err(fuxix::Errno::AGAIN) => {}
                Err(e) => return Err(format!("reading: {e}")),
            }
        }
    }
}

const ASK: Duration = Duration::from_secs(10);

fn options() -> Result<PaneOptions, String> {
    Ok(PaneOptions {
        history_bytes: HistoryBytes::new(1 << 20).ok_or("history")?,
        program_clipboard: false,
        reflow: true,
        alternate_scroll_lines: ScrollLines::new(3).ok_or("scroll")?,
        picture_bytes: PictureBytes::new(1 << 20).ok_or("pictures")?,
    })
}

fn program(argv: &[&str]) -> Result<Program, String> {
    let (first, rest) = argv.split_first().ok_or("no program")?;
    Program::new(
        (*first).to_owned(),
        rest.iter().map(|w| (*w).to_owned()).collect(),
    )
}

/// A new tab, its pane running `start`: the pane.
fn new_tab(socket: &Path, start: Start) -> Result<PaneId, String> {
    let size = Some(Screen::new(24, 80).map_err(|e| format!("{e:?}"))?);
    let command = Command::NewTab {
        target: vec![fux_wire::command::WsRef::Name("tests".to_owned())],
        name: None,
        size,
        cwd: Some(std::env::current_dir().map_err(text)?),
        start,
        options: options()?,
    };
    match ask(socket, Request::Do(command), ASK)?.response {
        Response::Answered(PaneResults::One(PaneAnswer::Acted(version))) => Ok(version.pane),
        other @ (Response::Answered(_) | Response::Returned(_) | Response::Failed { .. }) => {
            Err(format!("new-tab answered {other:?}"))
        }
    }
}

/// The text pane `pane` shows.
fn screen(socket: &Path, pane: PaneId) -> Result<String, String> {
    let command = Command::CapturePane {
        target: pane,
        history: Some(200),
        view: CaptureView::default(),
    };
    match ask(socket, Request::Do(command), ASK)?.response {
        Response::Returned(Body::Capture(captured)) => {
            let mut text = String::new();
            captured.write_text(&mut text).map_err(text_of)?;
            Ok(text)
        }
        other @ (Response::Answered(_) | Response::Returned(_) | Response::Failed { .. }) => {
            Err(format!("capture answered {other:?}"))
        }
    }
}

/// Waits till pane `pane` shows what `shows` accepts, `wait` at most.
fn shows(
    socket: &Path,
    pane: PaneId,
    wait: Duration,
    shows: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let deadline = after(wait)?;
    loop {
        let now = screen(socket, pane)?;
        if shows(&now) {
            return Ok(now);
        }
        if Instant::now() > deadline {
            return Err(format!(
                "{pane} did not show it in {wait:?}; it shows:\n{now}"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn typed(socket: &Path, pane: PaneId, keys: &str) -> Outcome {
    let command = Command::SendKeys {
        to: To::Now(AnyRef::Pane(pane)),
        inputs: vec![Input::Text(keys.to_owned())],
    };
    ask(socket, Request::Do(command), ASK).map(drop)
}

/// The signal's answer: none, or its failure's code and message.
fn signal(socket: &Path, pane: PaneId, signal: Signal) -> Result<Option<(String, String)>, String> {
    let command = Command::Signal {
        target: pane,
        signal,
    };
    Ok(match ask(socket, Request::Do(command), ASK)?.response {
        Response::Failed { code, message } => Some((code.name().to_owned(), message)),
        Response::Answered(answers) => match answers {
            PaneResults::One(PaneAnswer::Acted(_)) => None,
            other @ (PaneResults::One(_) | PaneResults::Many(_)) => {
                Some(("answered".to_owned(), format!("{other:?}")))
            }
        },
        Response::Returned(_) => None,
    })
}

fn stop(socket: &Path) -> Outcome {
    let _ = ask(socket, Request::Do(Command::KillServer), ASK);
    gone(socket)
}

/// Waits till no server answers on `socket`.
fn gone(socket: &Path) -> Outcome {
    let deadline = after(Duration::from_secs(10))?;
    while fux_server::socket::connect(socket).is_ok() {
        if Instant::now() > deadline {
            return Err("the server still answers after 10 s".to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

const CMD: &str = "cmd.exe";

/// A server killed outright (`TerminateProcess`) leaves its panes and
/// their programs running; a new server on the socket takes the run back
/// and drives them: what is typed reaches the shell, a resize reaches it,
/// and closing ends the panes (S3's probe, against real fux).
#[test]
fn a_new_server_takes_the_run_back_and_drives_its_live_panes() -> Outcome {
    let _turn = turn();
    let dir = scratch("takeover")?;
    let socket = dir.join("fux").join("s.sock");
    let mut first = serve(&socket, &dir.join("first.log"))?;
    let shell = new_tab(&socket, Start::Shell(program(&[CMD, "/d"])?))?;
    let beat = [
        "/q",
        "/c",
        "for /l %i in (1,1,100000) do @(echo beat %i & ping -n 2 127.0.0.1 >nul)",
    ];
    let beat = new_tab(
        &socket,
        Start::Program(program(&[&[CMD][..], &beat[..]].concat())?),
    )?;
    shows(&socket, shell, Duration::from_secs(20), |s| s.contains('>'))?;
    shows(&socket, beat, Duration::from_secs(20), |s| {
        s.contains("beat 2")
    })?;
    let seen = beats(&screen(&socket, beat)?);
    first.child.kill().map_err(text)?;
    first.child.wait().map_err(text)?;
    say(&format!("killed server {}", first.child.id()));
    let second = serve(&socket, &dir.join("second.log"))?;
    let taken = (|| -> Outcome {
        shows(&socket, beat, Duration::from_secs(20), |s| beats(s) > seen)?;
        say(&format!("the program kept running: beat {seen} and on"));
        typed(&socket, shell, "echo after-takeover\r")?;
        shows(&socket, shell, Duration::from_secs(20), |s| {
            s.lines().any(|line| line.trim() == "after-takeover")
        })?;
        let size = Screen::new(30, 100).map_err(|e| format!("{e:?}"))?;
        let resize = Command::Resize(Resized::Tabs(AnyRef::Pane(shell), Some(size)));
        ask(&socket, Request::Do(resize), ASK)?;
        typed(&socket, shell, "mode con\r")?;
        shows(&socket, shell, Duration::from_secs(20), |s| {
            s.lines()
                .any(|l| l.contains("Columns:") && l.contains("100"))
        })?;
        say("typed and resized after the takeover");
        let kill = |pane| Command::Kill {
            target: Some(AnyRef::Pane(pane)),
        };
        ask(&socket, Request::Do(kill(beat)), ASK)?;
        closed(&socket, beat)?;
        // The last pane closed, the server may end: it may not answer.
        let _ = ask(&socket, Request::Do(kill(shell)), ASK);
        closed(&socket, shell).or_else(|why| gone(&socket).map_err(|_| why))?;
        if second.log().contains("panicked") {
            return Err("the server panicked as its panes closed".to_owned());
        }
        say("closed both panes");
        Ok(())
    })();
    let log = second.log();
    let stopped = stop(&socket);
    say(&format!("the second server's log:\n{log}"));
    taken?;
    stopped
}

/// The beats a heartbeat pane shows, its last: 0 before the first.
fn beats(screen: &str) -> u64 {
    (screen.lines().rev())
        .find_map(|line| line.trim().strip_prefix("beat ")?.trim().parse().ok())
        .unwrap_or(0)
}

/// D-136 on a shell pane and a program pane: INT and QUIT reach the
/// console as Ctrl-C and Ctrl-Break, KILL ends what the shell runs and
/// then says `only_shell`, HUP is refused with `signal`, and TERM ends a
/// program pane's job, which exits with fux's own code.
#[test]
fn signals_reach_what_d136_says() -> Outcome {
    let _turn = turn();
    let dir = scratch("signals")?;
    let socket = dir.join("fux").join("s.sock");
    let served = serve(&socket, &dir.join("server.log"))?;
    let checked = (|| -> Outcome {
        let shell = new_tab(&socket, Start::Shell(program(&[CMD, "/d"])?))?;
        shows(&socket, shell, Duration::from_secs(20), |s| s.contains('>'))?;
        let long = Duration::from_secs(20);
        // QUIT: Ctrl-Break, which ping answers with its statistics so far,
        // and goes on.
        typed(&socket, shell, "ping -n 30 127.0.0.1\r")?;
        shows(&socket, shell, long, |s| s.contains("127.0.0.1:"))?;
        assert_eq!(signal(&socket, shell, Signal::Quit)?, None);
        shows(&socket, shell, long, |s| s.contains("Ping statistics"))?;
        say("QUIT reached ping as Ctrl-Break");
        // INT: Ctrl-C ends ping; the shell answers again.
        assert_eq!(signal(&socket, shell, Signal::Int)?, None);
        typed(&socket, shell, "echo after-int\r")?;
        shows(&socket, shell, long, |s| {
            s.lines().any(|l| l.trim() == "after-int")
        })?;
        say("INT ended ping, and the shell answers");
        // KILL: what the shell started on its console ends; the shell
        // lives. Then nothing is left beside it.
        typed(&socket, shell, "start /b ping -n 60 127.0.0.1 >nul\r")?;
        // Sent once it finds the job, which starts a moment later.
        let deadline = after(long)?;
        loop {
            let sent = signal(&socket, shell, Signal::Kill)?;
            match sent.as_ref().map(|(code, _)| code.as_str()) {
                None => break,
                Some("only-shell") if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => return Err(format!("KILL with a job running: {sent:?}")),
            }
        }
        // A process killed leaves the console's list a moment later.
        let deadline = after(long)?;
        loop {
            let alone = signal(&socket, shell, Signal::Kill)?;
            match alone.as_ref().map(|(code, _)| code.as_str()) {
                Some("only-shell") => break,
                None if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => return Err(format!("KILL with the shell alone: {alone:?}")),
            }
        }
        typed(&socket, shell, "echo after-kill\r")?;
        shows(&socket, shell, long, |s| {
            s.lines().any(|l| l.trim() == "after-kill")
        })?;
        say("KILL ended the shell's job, then said only_shell");
        // HUP has no counterpart: refused.
        let hup = signal(&socket, shell, Signal::Hup)?;
        assert_eq!(
            hup.as_ref().map(|(code, _)| code.as_str()),
            Some("signal"),
            "{hup:?}"
        );
        // TERM on a program pane: its job ends with fux's own code.
        let ping = Start::Program(program(&["ping.exe", "-n", "60", "127.0.0.1"])?);
        let ping = new_tab(&socket, ping)?;
        shows(&socket, ping, long, |s| s.contains("127.0.0.1:"))?;
        assert_eq!(signal(&socket, ping, Signal::Term)?, None);
        let deadline = after(long)?;
        let ended = loop {
            match signal(&socket, ping, Signal::Int)? {
                Some((_, message)) if message.contains("exited") => break message,
                _ if Instant::now() > deadline => return Err("ping never ended".to_owned()),
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        let killed = fuxix::process::KILLED.to_string();
        assert!(ended.contains(&killed), "{ended}");
        say(&format!("TERM ended the program pane's job: {ended}"));
        Ok(())
    })();
    let log = served.log();
    let stopped = stop(&socket);
    if checked.is_err() {
        say(&format!("the server's log:\n{log}"));
    }
    checked?;
    stopped
}

/// Where the journal is now, as a snapshot says.
fn cursor(socket: &Path) -> Result<fux_wire::events::Cursor, String> {
    let inspect = Command::Inspect {
        scope: fux_wire::inspect::Scope::All,
        projection: fux_wire::inspect::Projection::Recipe,
    };
    match ask(socket, Request::Do(inspect), ASK)?.response {
        Response::Returned(Body::Snapshot(snapshot)) => Ok(snapshot.cursor),
        other @ (Response::Answered(_) | Response::Returned(_) | Response::Failed { .. }) => {
            Err(format!("inspect answered {other:?}"))
        }
    }
}

/// The journal's events since `from`, a line each: its pane, then its JSON.
fn journal(socket: &Path, from: fux_wire::events::Cursor) -> Result<String, String> {
    let request = Request::NextEvents {
        scope: fux_wire::inspect::Scope::All,
        from: Some(from),
        timeout: fux_wire::command::Limit::from_secs(0),
    };
    match ask(socket, request, ASK)?.response {
        Response::Returned(Body::Events(batch)) => Ok((batch.events.iter())
            .map(|item| {
                let pane = item.pane.map_or("-".to_owned(), |pane| pane.to_string());
                format!("{pane} {}", item.data.render())
            })
            .collect::<Vec<_>>()
            .join("\n")),
        other @ (Response::Answered(_) | Response::Returned(_) | Response::Failed { .. }) => {
            Err(format!("next-events answered {other:?}"))
        }
    }
}

/// Waits till the journal since `from` holds what `holds` accepts.
fn journaled(
    socket: &Path,
    from: fux_wire::events::Cursor,
    wait: Duration,
    holds: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let deadline = after(wait)?;
    loop {
        let now = journal(socket, from)?;
        if holds(&now) {
            return Ok(now);
        }
        if Instant::now() > deadline {
            return Err(format!("the journal did not hold it in {wait:?}:\n{now}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Waits till pane `pane` is gone: its server refuses a capture of it. Any
/// other failure, a server gone among them, is told.
fn closed(socket: &Path, pane: PaneId) -> Outcome {
    let deadline = after(Duration::from_secs(20))?;
    loop {
        match screen(socket, pane) {
            Err(why) if why.starts_with("capture answered Failed") => return Ok(()),
            Err(why) => return Err(format!("{pane} as it closed: {why}")),
            Ok(_) if Instant::now() > deadline => {
                return Err(format!("{pane} still shows 20 s after it was closed"));
            }
            Ok(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// A pane's own process killed outright (a crash): the server, holding a
/// copy of its program's handle (D-134), ends the program if the grace
/// runs out and tells its real end, as no one else can now.
#[test]
fn a_crashed_panes_program_is_ended_and_its_end_told() -> Outcome {
    let _turn = turn();
    let dir = scratch("crash")?;
    let socket = dir.join("fux").join("s.sock");
    let served = serve(&socket, &dir.join("server.log"))?;
    let checked = (|| -> Outcome {
        let ping = Start::Program(program(&["ping.exe", "-n", "60", "127.0.0.1"])?);
        let ping = new_tab(&socket, ping)?;
        shows(&socket, ping, Duration::from_secs(20), |s| {
            s.contains("127.0.0.1:")
        })?;
        let marker = dir
            .join("fux")
            .join("s.sock.panes")
            .join(ping.number().to_string());
        let fux_server::socket::Reach::Listening(reached) =
            fux_server::socket::reach(&marker).map_err(text)?
        else {
            return Err("its pane's process does not listen".to_owned());
        };
        let pid = fux_server::socket::listener_pid(&reached).ok_or("its pane's pid")?;
        if pid.as_raw() == served.child.id() {
            return Err("the pipe names the server, not the pane's process".to_owned());
        }
        let from = cursor(&socket)?;
        let killed = Std::new("taskkill.exe")
            .args(["/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .status()
            .map_err(text)?;
        if !killed.success() {
            return Err(format!("taskkill of pane process {pid}: {killed}"));
        }
        say(&format!("killed {ping}'s own process {pid}"));
        // Its console closed with it, which ends ping with Ctrl-C's code
        // (0xC000013A), unless the grace ran out first and the server
        // killed it with fux's own: either is its real end, which only the
        // server's copy of its handle tells (without it the end is lost).
        let codes = [fuxix::process::KILLED, 0xC000_013A_u32.cast_signed()];
        let exited = |line: &str| {
            line.contains("exited") && codes.iter().any(|code| line.contains(&code.to_string()))
        };
        let told = journaled(&socket, from, Duration::from_secs(20), |j| {
            j.lines().any(exited)
        })?;
        let line = told.lines().find(|line| exited(line)).unwrap_or_default();
        say(&format!("its program's real end was told: {line}"));
        Ok(())
    })();
    let log = served.log();
    let stopped = stop(&socket);
    if checked.is_err() {
        say(&format!("the server's log:\n{log}"));
    }
    checked?;
    stopped
}

/// No pinned ConPTY to be had (its folder can't be made): Windows'
/// built-in one runs a program to its end, its exit status told, and the
/// server says once, in its log, what the built-in loses.
#[test]
fn the_builtin_conpty_runs_a_program_to_its_end() -> Outcome {
    let _turn = turn();
    let dir = scratch("builtin")?;
    let socket = dir.join("fux").join("s.sock");
    // A file where the ConPTY's folder would be: nothing is fetched.
    std::fs::write(state(&dir)?.join("conpty"), b"").map_err(text)?;
    let served = serve(&socket, &dir.join("server.log"))?;
    let checked = (|| -> Outcome {
        let from = cursor(&socket)?;
        let echo = Start::Program(program(&[CMD, "/q", "/c", "echo built-in-ran & exit 3"])?);
        let echo = new_tab(&socket, echo)?;
        shows(&socket, echo, Duration::from_secs(20), |s| {
            s.contains("built-in-ran")
        })?;
        journaled(&socket, from, Duration::from_secs(20), |now| {
            now.contains(r#""exited":3"#)
        })?;
        // Counted with a pane made: a pane's process says nothing again.
        let said = served.log();
        let notices = said.matches("built-in ConPTY").count();
        if notices != 1 {
            return Err(format!("the built-in said {notices} times:\n{said}"));
        }
        say("the built-in ran it to its end: exited 3");
        Ok(())
    })();
    let log = served.log();
    let stopped = stop(&socket);
    if checked.is_err() {
        say(&format!("the server's log:\n{log}"));
    }
    checked?;
    stopped
}

/// A second server on a socket with a live one is refused by the lock;
/// on a pipe name another process holds, it waits and refuses, never
/// joining it (D-138).
#[test]
fn a_live_server_and_a_held_pipe_are_refused() -> Outcome {
    let _turn = turn();
    let dir = scratch("refused")?;
    let socket = dir.join("fux").join("s.sock");
    let served = serve(&socket, &dir.join("first.log"))?;
    let second = serve(&socket, &dir.join("second.log")).err();
    let stopped = stop(&socket);
    drop(served);
    let second = second.ok_or("a second server started")?;
    if !second.contains("already using") {
        return Err(format!("the second server: {second}"));
    }
    say("a live server's socket was refused by its lock");
    stopped?;
    let held = dir.join("fux").join("held.sock");
    let name = fux_server::socket::pipe_name(&held).map_err(text)?;
    let squatter = fuxix::socket::Listener::bind(&name).map_err(text)?;
    let refused = serve(&held, &dir.join("held.log")).err();
    drop(squatter);
    let refused = refused.ok_or("a server joined a held pipe")?;
    if !refused.contains("held by another process") {
        return Err(format!("the held pipe: {refused}"));
    }
    say("a pipe another process holds was refused");
    Ok(())
}

/// A folder others can use is refused before anything in it is trusted:
/// the socket's, and a run's left in a private one.
#[test]
fn a_folder_others_can_use_is_refused() -> Outcome {
    let _turn = turn();
    let dir = scratch("open")?;
    let everyone = |folder: &Path| -> Outcome {
        let granted = Std::new("icacls")
            .arg(folder)
            .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
            .stdout(Stdio::null())
            .status()
            .map_err(text)?;
        granted.success().then_some(()).ok_or("icacls".to_owned())
    };
    let open = dir.join("open");
    std::fs::create_dir(&open).map_err(text)?;
    everyone(&open)?;
    let refused = serve(&open.join("s.sock"), &dir.join("open.log")).err();
    let refused = refused.ok_or("a server used a folder open to all")?;
    if !refused.contains("no one else can use") {
        return Err(format!("the open folder: {refused}"));
    }
    let private = dir.join("private");
    fuxix::file::private_dir(&private).map_err(text)?;
    let panes = private.join("s.sock.panes");
    std::fs::create_dir(&panes).map_err(text)?;
    everyone(&panes)?;
    let refused = serve(&private.join("s.sock"), &dir.join("panes.log")).err();
    let refused = refused.ok_or("a server used a run's folder open to all")?;
    if !refused.contains("no one else can use") {
        return Err(format!("the open run's folder: {refused}"));
    }
    say("folders open to all were refused");
    Ok(())
}

/// A client that goes while its question waits ends its connection's
/// process, which watches it, though it isn't read: told to its server,
/// which lets it go.
#[test]
fn a_client_gone_while_it_waits_ends_its_connection() -> Outcome {
    let _turn = turn();
    let dir = scratch("gone")?;
    let socket = dir.join("fux").join("s.sock");
    let served = serve(&socket, &dir.join("server.log"))?;
    let checked = (|| -> Outcome {
        let from = cursor(&socket)?;
        let stream = fux_server::socket::connect(&socket).map_err(text)?;
        let mut out = Hello::ours(Role::Command).encode();
        let request = Request::NextEvents {
            scope: fux_wire::inspect::Scope::All,
            from: Some(from),
            timeout: fux_wire::command::Limit::from_secs(600),
        };
        let context = Context {
            of: None,
            cwd: std::env::current_dir().map_err(text)?,
            pane: None,
            server: None,
            client: None,
        };
        out.extend(
            Asked {
                after: None,
                request,
                context,
            }
            .encode(),
        );
        let deadline = after(Duration::from_secs(20))?;
        let mut sent = 0;
        while sent < out.len() {
            let mut fds = [PollFd::new(&stream, Events::OUT)];
            let left = deadline.saturating_duration_since(Instant::now());
            fuxix::poll::poll(&mut fds, Some(left)).map_err(text)?;
            let rest = out.get(sent..).unwrap_or_default();
            sent += fuxix::io::write_what_fits(&stream, rest).map_err(text)?;
        }
        connections(Duration::from_secs(20), |count| count == 1)?;
        // Time for its question to reach its server; gone sooner, this
        // tests less, and still passes.
        std::thread::sleep(Duration::from_millis(500));
        drop(stream);
        connections(Duration::from_secs(20), |count| count == 0)?;
        say("the connection's process ended with its client");
        Ok(())
    })();
    let log = served.log();
    let stopped = stop(&socket);
    if checked.is_err() {
        say(&format!("the server's log:\n{log}"));
    }
    checked?;
    stopped
}

/// Waits till the count of connection processes running is one `wanted`
/// accepts.
fn connections(wait: Duration, wanted: impl Fn(usize) -> bool) -> Outcome {
    const COUNT: &str = "@(Get-CimInstance Win32_Process | Where-Object { \
        $_.Name -ne 'powershell.exe' -and $_.CommandLine -like '*__conn*' }).Count";
    let deadline = after(wait)?;
    loop {
        let counted = Std::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", COUNT])
            .output()
            .map_err(text)?;
        let said = String::from_utf8_lossy(&counted.stdout);
        let count = said
            .trim()
            .parse()
            .map_err(|_| format!("counted {said:?}"))?;
        if wanted(count) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("{count} connection processes after {wait:?}"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The directory a shell is in now, read from its process, as `split` and
/// `dump` read it (D-137): the folder `cd` took it to.
#[test]
fn a_shells_directory_is_read_from_its_process() -> Outcome {
    let _turn = turn();
    let dir = scratch("cwd")?;
    let socket = dir.join("fux").join("s.sock");
    let there = dir.join("cwd-here");
    std::fs::create_dir_all(&there).map_err(text)?;
    let served = serve(&socket, &dir.join("server.log"))?;
    let checked = (|| -> Outcome {
        let shell = new_tab(&socket, Start::Shell(program(&[CMD, "/d"])?))?;
        shows(&socket, shell, Duration::from_secs(20), |s| s.contains('>'))?;
        typed(&socket, shell, &format!("cd /d \"{}\"\r", there.display()))?;
        let deadline = after(Duration::from_secs(20))?;
        let inspect = Command::Inspect {
            scope: fux_wire::inspect::Scope::All,
            projection: fux_wire::inspect::Projection::Recipe,
        };
        loop {
            let reply = ask(&socket, Request::Do(inspect.clone()), ASK)?;
            let Response::Returned(Body::Snapshot(snapshot)) = reply.response else {
                return Err("inspect answered no snapshot".to_owned());
            };
            let pane = snapshot.panes.iter().find(|pane| pane.id == shell);
            let cwd = format!("{:?}", pane.map(|pane| &pane.cwd));
            if cwd.contains("cwd-here") {
                say(&format!("its directory: {cwd}"));
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("its directory stayed {cwd}"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    let log = served.log();
    let stopped = stop(&socket);
    if checked.is_err() {
        say(&format!("the server's log:\n{log}"));
    }
    checked?;
    stopped
}
