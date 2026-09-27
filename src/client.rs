//! The clients: `fux attach`, a dumb pipe between a terminal and the server,
//! and the one-shot command client every other `fux` command uses.
use crate::protocol::{Decoder, Frame, PROTOCOL, Role};
use fuxix::poll::{Events as PollFlags, PollFd};
use fuxix::terminal::Termios;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Modes the attach client sets on the outer terminal: the alternate
/// screen, normal cursor and keypad keys, bracketed paste, focus events, no
/// autowrap; never mouse reporting.
const ENTER: &str = "\x1b[?1049h\x1b[?1l\x1b>\x1b[?2004h\x1b[?1004h\x1b[?7l\x1b[H\x1b[2J";
/// And turns them off again.
const LEAVE: &str = "\x1b[?2026l\x1b[?1004l\x1b[?2004l\x1b[?7h\x1b[0m\x1b[0 q\x1b[?25h\x1b[?1049l";

/// Why a client could not reach the server, start one, or go on.
#[derive(Debug)]
pub enum Error {
    Socket(crate::socket::Error),
    Connect {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The server ended the connection, saying why.
    Refused(String),
    /// The server closed the connection before it answered.
    NoAnswer,
    Timeout,
    /// The server closed the connection before the command was done.
    Closed,
    Protocol(crate::protocol::Error),
    Write(std::io::Error),
    Read(std::io::Error),
    // Starting a server.
    Exe(std::io::Error),
    NoDirectory,
    Log(std::io::Error),
    Spawn(std::io::Error),
    Exited {
        status: std::process::ExitStatus,
        log: PathBuf,
    },
    NotStarted {
        log: PathBuf,
    },
    // Attaching this terminal.
    NotATerminal,
    Modes(fuxix::Errno),
    RawMode(fuxix::Errno),
    /// The pipes that carry signals could not be set up.
    Signals(std::io::Error),
    Poll(fuxix::Errno),
    ReadTerminal(fuxix::Errno),
    WriteTerminal(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Socket(error) => error.fmt(f),
            Error::Connect { path, source } => {
                write!(f, "connecting to {}: {source}", path.display())
            }
            Error::Refused(reason) => f.write_str(reason),
            Error::NoAnswer => f.write_str("the server did not answer"),
            Error::Timeout => f.write_str("the server did not answer in time"),
            Error::Closed => f.write_str("the server closed the connection"),
            Error::Protocol(error) => error.fmt(f),
            Error::Write(error) => write!(f, "writing to the server: {error}"),
            Error::Read(error) => write!(f, "reading from the server: {error}"),
            Error::Exe(error) => write!(f, "finding the fux binary: {error}"),
            Error::NoDirectory => f.write_str("the socket has no directory"),
            Error::Log(error) => write!(f, "opening the server log: {error}"),
            Error::Spawn(error) => write!(f, "starting a server: {error}"),
            Error::Exited { status, log } => {
                write!(f, "the server exited ({status}); see {}", log.display())
            }
            Error::NotStarted { log } => write!(
                f,
                "the server did not start within 2 s; see {}",
                log.display()
            ),
            Error::NotATerminal => f.write_str("fux attach needs a terminal on stdin"),
            Error::Modes(errno) => write!(f, "reading terminal modes: {errno}"),
            Error::RawMode(errno) => write!(f, "setting raw mode: {errno}"),
            Error::Signals(error) => error.fmt(f),
            Error::Poll(errno) => write!(f, "poll: {errno}"),
            Error::ReadTerminal(errno) => write!(f, "reading the terminal: {errno}"),
            Error::WriteTerminal(error) => write!(f, "writing the terminal: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Socket(error) => Some(error),
            Error::Protocol(error) => Some(error),
            Error::Connect { source: error, .. }
            | Error::Write(error)
            | Error::Read(error)
            | Error::Exe(error)
            | Error::Log(error)
            | Error::Spawn(error)
            | Error::Signals(error)
            | Error::WriteTerminal(error) => Some(error),
            Error::Modes(errno)
            | Error::RawMode(errno)
            | Error::Poll(errno)
            | Error::ReadTerminal(errno) => Some(errno),
            Error::Refused(_)
            | Error::NoAnswer
            | Error::Timeout
            | Error::Closed
            | Error::NoDirectory
            | Error::Exited { .. }
            | Error::NotStarted { .. }
            | Error::NotATerminal => None,
        }
    }
}

impl From<crate::socket::Error> for Error {
    fn from(error: crate::socket::Error) -> Error {
        Error::Socket(error)
    }
}

impl From<crate::protocol::Error> for Error {
    fn from(error: crate::protocol::Error) -> Error {
        Error::Protocol(error)
    }
}

/// The terminal state to restore, shared with the panic hook.
static SAVED: Mutex<Option<Termios>> = Mutex::new(None);

fn restore() {
    let saved = SAVED.lock().ok().and_then(|mut s| s.take());
    if let Some(termios) = saved {
        let mut out = std::io::stdout();
        let _ = out.write_all(LEAVE.as_bytes());
        let _ = out.flush();
        let _ = fuxix::terminal::set_attributes(std::io::stdin(), &termios);
    }
}

fn connect(socket: &Path, role: Role) -> Result<(UnixStream, Decoder), Error> {
    crate::socket::check_client_socket(socket)?;
    let mut stream = UnixStream::connect(socket).map_err(|source| Error::Connect {
        path: socket.to_owned(),
        source,
    })?;
    let sent = send(
        &mut stream,
        &Frame::Hello {
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION").into(),
            role,
        },
    );
    let mut decoder = Decoder::default();
    let mut buffer = vec![0u8; 64 * 1024];
    // A server that refuses a connection says why and closes it, maybe
    // before the Hello is written: its reason is still there to read.
    let answer = read_frame(
        &mut stream,
        &mut decoder,
        &mut buffer,
        Some(Duration::from_secs(5)),
    );
    greeted(answer, sent)?;
    // A mismatched server answers Hello and then says why it refuses.
    Ok((stream, decoder))
}

/// Whether the server's answer to Hello, and sending it, let the client go
/// on: a refusal, which a server may make before reading the Hello, wins.
fn greeted(answer: Result<Option<Frame>, Error>, sent: Result<(), Error>) -> Result<(), Error> {
    match (answer, sent) {
        (Ok(Some(Frame::Exit(reason))), _) => Err(Error::Refused(reason)),
        (_, Err(error)) => Err(error),
        (Ok(Some(Frame::Hello { .. })), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Ok(())) => Err(Error::NoAnswer),
    }
}

fn send(stream: &mut UnixStream, frame: &Frame) -> Result<(), Error> {
    let bytes = frame.encode()?;
    stream.write_all(&bytes).map_err(Error::Write)
}

fn read_frame(
    stream: &mut UnixStream,
    decoder: &mut Decoder,
    buffer: &mut [u8],
    timeout: Option<Duration>,
) -> Result<Option<Frame>, Error> {
    let _ = stream.set_read_timeout(timeout);
    loop {
        if let Some(frame) = decoder.frame()? {
            return Ok(Some(frame));
        }
        match stream.read(buffer) {
            Ok(0) => return Ok(None),
            Ok(n) => decoder.push(buffer.get(..n).unwrap_or_default()),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(Error::Timeout);
            }
            Err(e) => return Err(Error::Read(e)),
        }
    }
}

/// Runs one command on the server; its output goes to ours. The exit status.
pub fn command(socket: &Path, argv: &[String]) -> Result<u8, Error> {
    let (mut stream, mut decoder) = connect(socket, Role::Command)?;
    let cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pane = std::env::var("FUX_PANE").ok().filter(|p| !p.is_empty());
    send(
        &mut stream,
        &Frame::Command {
            argv: argv.to_vec(),
            cwd,
            pane,
        },
    )?;
    let (mut stdout, mut stderr) = (std::io::stdout(), std::io::stderr());
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match read_frame(&mut stream, &mut decoder, &mut buffer, None)? {
            Some(Frame::Stdout(bytes)) => {
                let _ = stdout.write_all(&bytes);
            }
            Some(Frame::Stderr(bytes)) => {
                let _ = stderr.write_all(&bytes);
            }
            Some(Frame::Done { status }) => {
                let _ = stdout.flush();
                return Ok(status);
            }
            Some(Frame::Exit(reason)) => return Err(Error::Refused(reason)),
            Some(_) => {}
            None => return Err(Error::Closed),
        }
    }
}

/// Stops the server, whatever fux version it is.
pub fn kill_server(socket: &Path) -> Result<(), Error> {
    let (mut stream, mut decoder) = connect(socket, Role::Kill)?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match read_frame(
            &mut stream,
            &mut decoder,
            &mut buffer,
            Some(Duration::from_secs(5)),
        ) {
            Ok(Some(Frame::Done { .. })) | Ok(None) => return Ok(()),
            Ok(Some(_)) => {}
            Err(error) => return Err(error),
        }
    }
}

/// The hidden `fux server` flag with which a client starts a server: the
/// server makes itself a session leader before anything else, so it
/// outlives the terminal the client ran in.
pub const SETSID: &str = "--setsid";

/// Starts a server in the background, in a new session with its output in
/// `fux.log` beside the socket, and waits for it to answer.
pub fn start_server(socket: &Path) -> Result<(), Error> {
    let exe = std::env::current_exe().map_err(Error::Exe)?;
    let directory = socket.parent().ok_or(Error::NoDirectory)?;
    crate::socket::prepare_directory(directory)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("fux.log"))
        .map_err(Error::Log)?;
    let mut command = std::process::Command::new(exe);
    // `--setsid`: the server leaves this terminal's session as it starts.
    command
        .arg("server")
        .arg("--socket")
        .arg(socket)
        .arg(SETSID)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    let mut child = command.spawn().map_err(Error::Spawn)?;
    let deadline = crate::after(Instant::now(), Duration::from_secs(2));
    loop {
        if UnixStream::connect(socket).is_ok() {
            // The child is left to run; it is not ours to wait for, and
            // dropping a `Child` neither waits for nor kills it.
            drop(child);
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            // It lost a race to another server, or failed: connect to the
            // winner if there is one.
            if UnixStream::connect(socket).is_ok() {
                return Ok(());
            }
            return Err(Error::Exited {
                status,
                log: directory.join("fux.log"),
            });
        }
        if Instant::now() > deadline {
            return Err(Error::NotStarted {
                log: directory.join("fux.log"),
            });
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn window_size() -> (u16, u16) {
    fuxix::terminal::window_size(std::io::stdout())
        .ok()
        .filter(|(rows, cols)| *rows > 0 && *cols > 0)
        .unwrap_or((24, 80))
}

/// Attaches this terminal to the server until detach or the server's end.
pub fn attach(socket: &Path, workspace: Option<String>) -> Result<(), Error> {
    let stdin = std::io::stdin();
    if !std::io::IsTerminal::is_terminal(&stdin) {
        return Err(Error::NotATerminal);
    }
    let (mut stream, mut decoder) = connect(socket, Role::Attach)?;
    let (rows, cols) = window_size();
    send(
        &mut stream,
        &Frame::Attach {
            rows,
            cols,
            workspace,
        },
    )?;

    let original = fuxix::terminal::attributes(&stdin).map_err(Error::Modes)?;
    let mut raw = original.clone();
    raw.make_raw();
    if let Ok(mut saved) = SAVED.lock() {
        *saved = Some(original);
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    fuxix::terminal::set_attributes(&stdin, &raw).map_err(|e| {
        restore();
        Error::RawMode(e)
    })?;
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(ENTER.as_bytes());
    let _ = stdout.flush();

    let result = pump(&mut stream, &mut decoder);
    restore();
    match result {
        Ok(reason) => {
            println!("[{reason}]");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Moves bytes both ways until the server ends the attachment. The reason.
fn pump(stream: &mut UnixStream, decoder: &mut Decoder) -> Result<String, Error> {
    let (winch, winch_in) = UnixStream::pair().map_err(Error::Signals)?;
    let (stops, stops_in) = UnixStream::pair().map_err(Error::Signals)?;
    winch.set_nonblocking(true).map_err(Error::Signals)?;
    stops.set_nonblocking(true).map_err(Error::Signals)?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGWINCH, winch_in)
        .map_err(Error::Signals)?;
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ] {
        let stop = stops_in.try_clone().map_err(Error::Signals)?;
        signal_hook::low_level::pipe::register(signal, stop).map_err(Error::Signals)?;
    }
    let _ = stream.set_read_timeout(None);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut buffer = vec![0u8; 64 * 1024];
    // Where each read from the terminal is framed; reused by every read.
    let mut input = Vec::new();
    let mut winch = winch;
    loop {
        let mut fds = [
            PollFd::new(&stdin, PollFlags::IN),
            PollFd::new(&*stream, PollFlags::IN),
            PollFd::new(&winch, PollFlags::IN),
            PollFd::new(&stops, PollFlags::IN),
        ];
        match fuxix::poll::poll(&mut fds, None) {
            Ok(_) | Err(fuxix::Errno::INTR) => {}
            Err(e) => return Err(Error::Poll(e)),
        }
        let ready = fds.each_ref().map(PollFd::revents);
        let is = |i: usize| ready.get(i).is_some_and(|f| !f.is_empty());
        if is(3) {
            let _ = send(stream, &Frame::Detach);
            return Ok("detached by a signal".into());
        }
        if is(2) {
            let mut sink = [0u8; 64];
            while matches!(winch.read(&mut sink), Ok(n) if n > 0) {}
            let (rows, cols) = window_size();
            send(stream, &Frame::Resize { rows, cols })?;
        }
        if is(0) {
            match fuxix::io::read(&stdin, &mut buffer) {
                Ok(0) => {
                    let _ = send(stream, &Frame::Detach);
                    return Ok("detached: the terminal closed".into());
                }
                Ok(n) => {
                    input.clear();
                    crate::protocol::encode_input(buffer.get(..n).unwrap_or_default(), &mut input)?;
                    stream.write_all(&input).map_err(Error::Write)?;
                }
                Err(fuxix::Errno::INTR | fuxix::Errno::AGAIN) => {}
                Err(e) => return Err(Error::ReadTerminal(e)),
            }
        }
        if is(1) {
            match stream.read(&mut buffer) {
                Ok(0) => return Ok("the server closed the connection".into()),
                Ok(n) => decoder.push(buffer.get(..n).unwrap_or_default()),
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {}
                Err(e) => return Err(Error::Read(e)),
            }
            while let Some(raw) = decoder.raw()? {
                // A paint goes to the terminal straight from the decoder.
                if let Some(bytes) = raw.paint() {
                    stdout.write_all(bytes).map_err(Error::WriteTerminal)?;
                    continue;
                }
                // Every other frame is decoded, so a bad one is an error,
                // and ignored.
                if let Frame::Exit(reason) = raw.decode()? {
                    return Ok(reason);
                }
            }
            let _ = stdout.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the client makes of the server's answer to Hello, as read from
    /// a socket in `wait`.
    fn answer(server: impl FnOnce(&mut UnixStream), wait: Duration) -> Result<(), Error> {
        let (mut client, mut peer) = UnixStream::pair().map_err(Error::Signals)?;
        server(&mut peer);
        let mut decoder = Decoder::default();
        let mut buffer = [0u8; 256];
        let answer = read_frame(&mut client, &mut decoder, &mut buffer, Some(wait));
        greeted(answer, Ok(()))
    }

    fn write(peer: &mut UnixStream, frame: &Frame) {
        let _ = frame.encode().map(|bytes| peer.write_all(&bytes));
    }

    /// A server that refuses says why; one that says nothing times out;
    /// one that hangs up did not answer.
    #[test]
    fn the_client_tells_a_refusal_from_a_timeout() {
        let wait = Duration::from_millis(50);
        let hello = Frame::Hello {
            protocol: PROTOCOL,
            version: "0".into(),
            role: Role::Command,
        };
        assert!(answer(|peer| write(peer, &hello), wait).is_ok());
        let refused = answer(|peer| write(peer, &Frame::Exit("busy".into())), wait);
        assert!(matches!(&refused, Err(Error::Refused(reason)) if reason == "busy"));
        let silent = answer(|_| {}, wait);
        assert!(matches!(silent, Err(Error::Timeout)));
        assert_eq!(
            silent.err().map(|e| e.to_string()).as_deref(),
            Some("the server did not answer in time")
        );
        let gone = answer(|peer| drop(peer.shutdown(std::net::Shutdown::Write)), wait);
        assert!(matches!(gone, Err(Error::NoAnswer)));
        let garbled = answer(|peer| drop(peer.write_all(&[0, 0, 0, 1, 99])), wait);
        assert!(matches!(
            garbled,
            Err(Error::Protocol(crate::protocol::Error::UnknownKind(99)))
        ));
        // A refusal wins over a failure to send the Hello.
        let both = greeted(Ok(Some(Frame::Exit("no".into()))), Err(Error::Closed));
        assert!(matches!(both, Err(Error::Refused(_))));
    }
}
