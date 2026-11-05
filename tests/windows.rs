//! fux's server on Windows (WINDOWS.md, PR 7), driven as its clients
//! drive it, over its pipe, by a small client of the wire protocol: fux's
//! own client is Unix's till PR 8. Each test checks one property, against
//! the real `fux` binary on a private socket, and has a deadline.
//!
//! - A server killed outright leaves its panes running, and a new server
//!   takes the run back and drives them: typing, resizing, closing
//!   (spike S3's probe, against real fux).
//! - The signals reach what D-136 says, on a shell pane and a program
//!   pane: INT and QUIT as keys, KILL of a shell's job, `only_shell`,
//!   TERM ending a program's job with fux's own code, HUP refused.
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

/// `fux __serve` on `socket`, its fux folder (`LOCALAPPDATA`) the test's
/// own `home`, which holds CI's ConPTY package if there is one.
fn serve(socket: &Path, log: &Path) -> Result<Served, String> {
    let home = socket
        .parent()
        .and_then(Path::parent)
        .ok_or("the socket's folder")?;
    let conpty = home.join("fux").join("conpty");
    if let Some(package) = std::env::var_os("FUXIX_CONPTY_PACKAGE")
        && !conpty.join(PACKAGE).exists()
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
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
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
        target: Vec::new(),
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
    let deadline = after(Duration::from_secs(10))?;
    while fux_server::socket::connect(socket).is_ok() {
        if Instant::now() > deadline {
            return Err("the server still answers 10 s after kill-server".to_owned());
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
    let shell = new_tab(&socket, Start::Shell(program(&[CMD, "/q"])?))?;
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
    first.child.kill().map_err(text)?;
    first.child.wait().map_err(text)?;
    eprintln!("killed server {}", first.child.id());
    let before = screen_beats(&socket, beat);
    eprintln!("beats before the takeover: {before:?} (none: no server)");
    let second = serve(&socket, &dir.join("second.log"))?;
    let taken = (|| -> Outcome {
        let seen = beats(&screen(&socket, beat)?);
        shows(&socket, beat, Duration::from_secs(20), |s| beats(s) > seen)?;
        eprintln!("the program kept running: beat {seen} and on");
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
        eprintln!("typed and resized after the takeover");
        for pane in [beat, shell] {
            let kill = Command::Kill {
                target: Some(AnyRef::Pane(pane)),
            };
            ask(&socket, Request::Do(kill), ASK)?;
        }
        Ok(())
    })();
    let log = second.log();
    let stopped = stop(&socket);
    eprintln!("the second server's log:\n{log}");
    taken?;
    stopped
}

/// The beats a heartbeat pane shows, its last: 0 before the first.
fn beats(screen: &str) -> u64 {
    (screen.lines().rev())
        .find_map(|line| line.trim().strip_prefix("beat ")?.trim().parse().ok())
        .unwrap_or(0)
}

fn screen_beats(socket: &Path, pane: PaneId) -> Option<u64> {
    screen(socket, pane).ok().map(|s| beats(&s))
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
        let shell = new_tab(&socket, Start::Shell(program(&[CMD, "/q"])?))?;
        shows(&socket, shell, Duration::from_secs(20), |s| s.contains('>'))?;
        let long = Duration::from_secs(20);
        // QUIT: Ctrl-Break, which ping answers with its statistics so far,
        // and goes on.
        typed(&socket, shell, "ping -n 30 127.0.0.1\r")?;
        shows(&socket, shell, long, |s| s.contains("127.0.0.1:"))?;
        assert_eq!(signal(&socket, shell, Signal::Quit)?, None);
        shows(&socket, shell, long, |s| s.contains("Ping statistics"))?;
        eprintln!("QUIT reached ping as Ctrl-Break");
        // INT: Ctrl-C ends ping; the shell answers again.
        assert_eq!(signal(&socket, shell, Signal::Int)?, None);
        typed(&socket, shell, "echo after-int\r")?;
        shows(&socket, shell, long, |s| {
            s.lines().any(|l| l.trim() == "after-int")
        })?;
        eprintln!("INT ended ping, and the shell answers");
        // KILL: what the shell started on its console ends; the shell
        // lives. Then nothing is left beside it.
        typed(&socket, shell, "start /b ping -n 60 127.0.0.1 >nul\r")?;
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(signal(&socket, shell, Signal::Kill)?, None);
        let alone = signal(&socket, shell, Signal::Kill)?;
        assert_eq!(
            alone.as_ref().map(|(code, _)| code.as_str()),
            Some("only_shell"),
            "{alone:?}"
        );
        typed(&socket, shell, "echo after-kill\r")?;
        shows(&socket, shell, long, |s| {
            s.lines().any(|l| l.trim() == "after-kill")
        })?;
        eprintln!("KILL ended the shell's job, then said only_shell");
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
        eprintln!("TERM ended the program pane's job: {ended}");
        Ok(())
    })();
    let log = served.log();
    let stopped = stop(&socket);
    if checked.is_err() {
        eprintln!("the server's log:\n{log}");
    }
    checked?;
    stopped
}
