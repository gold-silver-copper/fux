//! The clients: `fux attach`, a dumb pipe between a terminal and the server,
//! and the one-shot command client every other `fux` command uses.
use crate::protocol::{Decoder, Frame, PROTOCOL, Role};
use rustix::event::{PollFd, PollFlags};
use rustix::termios::{OptionalActions, Termios};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Modes the attach client sets on the outer terminal: the alternate
/// screen, normal cursor and keypad keys, bracketed paste, focus events, no
/// autowrap; never mouse reporting.
const ENTER: &str = "\x1b[?1049h\x1b[?1l\x1b>\x1b[?2004h\x1b[?1004h\x1b[?7l\x1b[H\x1b[2J";
/// And turns them off again.
const LEAVE: &str = "\x1b[?2026l\x1b[?1004l\x1b[?2004l\x1b[?7h\x1b[0m\x1b[0 q\x1b[?25h\x1b[?1049l";

/// The terminal state to restore, shared with the panic hook.
static SAVED: Mutex<Option<Termios>> = Mutex::new(None);

fn restore() {
    let saved = SAVED.lock().ok().and_then(|mut s| s.take());
    if let Some(termios) = saved {
        let mut out = std::io::stdout();
        let _ = out.write_all(LEAVE.as_bytes());
        let _ = out.flush();
        let _ = rustix::termios::tcsetattr(std::io::stdin(), OptionalActions::Now, &termios);
    }
}

fn connect(socket: &Path, role: Role) -> Result<(UnixStream, Decoder), String> {
    crate::socket::check_client_socket(socket)?;
    let mut stream = UnixStream::connect(socket)
        .map_err(|e| format!("connecting to {}: {e}", socket.display()))?;
    send(
        &mut stream,
        &Frame::Hello {
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION").into(),
            role,
        },
    )?;
    let mut decoder = Decoder::default();
    let mut buffer = vec![0u8; 64 * 1024];
    match read_frame(
        &mut stream,
        &mut decoder,
        &mut buffer,
        Some(Duration::from_secs(5)),
    )? {
        Some(Frame::Hello { .. }) => {}
        Some(Frame::Exit(reason)) => return Err(reason),
        _ => return Err("the server did not answer".into()),
    }
    // A mismatched server answers Hello and then says why it refuses.
    Ok((stream, decoder))
}

fn send(stream: &mut UnixStream, frame: &Frame) -> Result<(), String> {
    let bytes = frame.encode()?;
    stream
        .write_all(&bytes)
        .map_err(|e| format!("writing to the server: {e}"))
}

fn read_frame(
    stream: &mut UnixStream,
    decoder: &mut Decoder,
    buffer: &mut [u8],
    timeout: Option<Duration>,
) -> Result<Option<Frame>, String> {
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
                return Err("the server did not answer in time".into());
            }
            Err(e) => return Err(format!("reading from the server: {e}")),
        }
    }
}

/// Runs one command on the server; its output goes to ours. The exit status.
pub fn command(socket: &Path, argv: &[String]) -> Result<u8, String> {
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
            Some(Frame::Exit(reason)) => return Err(reason),
            Some(_) => {}
            None => return Err("the server closed the connection".into()),
        }
    }
}

/// Stops the server, whatever fux version it is.
pub fn kill_server(socket: &Path) -> Result<(), String> {
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

/// Starts a server in the background, in a new session with its output in
/// `fux.log` beside the socket, and waits for it to answer.
pub fn start_server(socket: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| format!("finding the fux binary: {e}"))?;
    let directory = socket.parent().ok_or("the socket has no directory")?;
    crate::socket::prepare_directory(directory)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("fux.log"))
        .map_err(|e| format!("opening the server log: {e}"))?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("server")
        .arg("--socket")
        .arg(socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    // SAFETY: `setsid` is an async-signal-safe system call and the hook
    // allocates nothing.
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()
                .map(drop)
                .map_err(std::io::Error::from)
        });
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("starting a server: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(2);
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
            return Err(format!(
                "the server exited ({status}); see {}",
                directory.join("fux.log").display()
            ));
        }
        if Instant::now() > deadline {
            return Err(format!(
                "the server did not start within 2 s; see {}",
                directory.join("fux.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn window_size() -> (u16, u16) {
    rustix::termios::tcgetwinsize(std::io::stdout())
        .ok()
        .filter(|w| w.ws_row > 0 && w.ws_col > 0)
        .map_or((24, 80), |w| (w.ws_row, w.ws_col))
}

/// Attaches this terminal to the server until detach or the server's end.
pub fn attach(socket: &Path, workspace: Option<String>) -> Result<(), String> {
    let stdin = std::io::stdin();
    if !rustix::termios::isatty(&stdin) {
        return Err("fux attach needs a terminal on stdin".into());
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

    let original =
        rustix::termios::tcgetattr(&stdin).map_err(|e| format!("reading terminal modes: {e}"))?;
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
    rustix::termios::tcsetattr(&stdin, OptionalActions::Now, &raw).map_err(|e| {
        restore();
        format!("setting raw mode: {e}")
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
fn pump(stream: &mut UnixStream, decoder: &mut Decoder) -> Result<String, String> {
    let (winch, winch_in) = UnixStream::pair().map_err(|e| e.to_string())?;
    let (stops, stops_in) = UnixStream::pair().map_err(|e| e.to_string())?;
    winch.set_nonblocking(true).map_err(|e| e.to_string())?;
    stops.set_nonblocking(true).map_err(|e| e.to_string())?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGWINCH, winch_in)
        .map_err(|e| e.to_string())?;
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ] {
        let stop = stops_in.try_clone().map_err(|e| e.to_string())?;
        signal_hook::low_level::pipe::register(signal, stop).map_err(|e| e.to_string())?;
    }
    let _ = stream.set_read_timeout(None);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut buffer = vec![0u8; 64 * 1024];
    let (mut winch, mut stops) = (winch, stops);
    loop {
        let mut fds = [
            PollFd::new(&stdin, PollFlags::IN),
            PollFd::new(&*stream, PollFlags::IN),
            PollFd::new(&winch, PollFlags::IN),
            PollFd::new(&stops, PollFlags::IN),
        ];
        match rustix::event::poll(&mut fds, None) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(format!("poll: {e}")),
        }
        let ready: Vec<PollFlags> = fds.iter().map(PollFd::revents).collect();
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
            match rustix::io::read(&stdin, &mut buffer) {
                Ok(0) => {
                    let _ = send(stream, &Frame::Detach);
                    return Ok("detached: the terminal closed".into());
                }
                Ok(n) => send(
                    stream,
                    &Frame::Input(buffer.get(..n).unwrap_or_default().to_vec()),
                )?,
                Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {}
                Err(e) => return Err(format!("reading the terminal: {e}")),
            }
        }
        if is(1) {
            match stream.read(&mut buffer) {
                Ok(0) => return Ok("the server closed the connection".into()),
                Ok(n) => decoder.push(buffer.get(..n).unwrap_or_default()),
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {}
                Err(e) => return Err(format!("reading from the server: {e}")),
            }
            while let Some(frame) = decoder.frame()? {
                match frame {
                    Frame::Paint(bytes) => {
                        stdout
                            .write_all(&bytes)
                            .map_err(|e| format!("writing the terminal: {e}"))?;
                    }
                    Frame::Exit(reason) => return Ok(reason),
                    Frame::Hello { .. }
                    | Frame::Attach { .. }
                    | Frame::Input(_)
                    | Frame::Resize { .. }
                    | Frame::Detach
                    | Frame::Command { .. }
                    | Frame::Stdout(_)
                    | Frame::Stderr(_)
                    | Frame::Done { .. } => {}
                }
            }
            let _ = stdout.flush();
        }
        // The signal pipes are drained by being read above.
        let mut sink = [0u8; 64];
        let _ = stops.read(&mut sink);
    }
}
