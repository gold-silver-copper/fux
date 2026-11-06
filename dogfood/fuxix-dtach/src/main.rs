//! fuxix-dtach: runs a program on a PTY behind a Unix socket, and attaches
//! terminals to it, as dtach does.
//!
//! `-c SOCKET CMD...` starts CMD in the background and attaches to it,
//! `-n SOCKET CMD...` starts it without attaching, and `-a SOCKET` attaches
//! this terminal. An attached terminal is in raw mode; the detach key
//! (Ctrl-\ unless `-e` says another) leaves the program running. The
//! program sees each attached terminal's size, the last one sent winning.
//!
//! The socket carries, from a terminal, packets of a kind byte, a length
//! byte and that many bytes: input for the program, or the terminal's size
//! (rows, columns, big-endian u16s). To a terminal it carries the program's
//! output as it is.

use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Dispositions, Pid, Signal};
use fuxix::terminal::{self, Size};
use fuxix::{Errno, pty, signal, socket};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: fuxix-dtach -c SOCKET [-e KEY] CMD [ARG...]   start CMD and attach
       fuxix-dtach -n SOCKET CMD [ARG...]            start CMD detached
       fuxix-dtach -a SOCKET [-e KEY]                attach to SOCKET
  -e KEY  the detach key: ^X for Ctrl-X, or `none` (default ^\\)";

/// The hidden mode the background process runs in.
const SERVE: &str = "--serve";
/// What the background process prints once it listens.
const READY: &str = "ready";

const INPUT: u8 = 0;
const RESIZE: u8 = 1;

/// The most output kept for a terminal that does not read it, before it is
/// let go: a stuck terminal must not stop the program.
const BACKLOG: usize = 4 << 20;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("fuxix-dtach: {why}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let (mode, rest) = args.split_first().ok_or(USAGE)?;
    if mode == SERVE {
        return serve_main(rest);
    }
    let (socket_path, mut rest) = rest.split_first().ok_or(USAGE)?;
    let socket_path = PathBuf::from(socket_path);
    let mut detach = Some(0x1c);
    if let [flag, key, more @ ..] = rest
        && flag == "-e"
    {
        detach = parse_key(key)?;
        rest = more;
    }
    match mode.as_str() {
        "-a" if rest.is_empty() => attach(&socket_path, detach),
        "-c" | "-n" if !rest.is_empty() => {
            start(&socket_path, rest)?;
            if mode == "-c" {
                attach(&socket_path, detach)
            } else {
                Ok(())
            }
        }
        "-h" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(USAGE.to_owned()),
    }
}

/// `^X` as Ctrl-X's byte, `^?` as DEL, a single character as itself.
fn parse_key(key: &str) -> Result<Option<u8>, String> {
    match key.as_bytes() {
        b"none" => Ok(None),
        b"^?" => Ok(Some(0x7f)),
        [b'^', c] if c.is_ascii_alphabetic() || b"@[\\]^_".contains(c) => {
            Ok(Some(c.to_ascii_uppercase() & 0x1f))
        }
        [c] => Ok(Some(*c)),
        _ => Err(format!("-e {key}: give ^X, a single character, or none")),
    }
}

// ---------------------------------------------------------------- start

/// Starts the background process serving `socket_path`, running `command`,
/// and waits until it listens.
fn start(socket_path: &Path, command: &[String]) -> Result<(), String> {
    if UnixStream::connect(socket_path).is_ok() {
        return Err(format!("{}: a program is attached there already", socket_path.display()));
    }
    // A socket no one listens on is one a crashed server left.
    if let Ok(meta) = std::fs::symlink_metadata(socket_path) {
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_socket() {
            return Err(format!("{}: exists and is no socket", socket_path.display()));
        }
        std::fs::remove_file(socket_path).map_err(|e| format!("{}: {e}", socket_path.display()))?;
    }
    // The program starts at the size of the terminal it is started from.
    let (rows, cols) = terminal::window_size(io::stdin())
        .map_or((24, 80), |(s, _)| (s.rows.get(), s.cols.get()));
    let exe = std::env::current_exe().map_err(|e| format!("finding this program: {e}"))?;
    let mut serve = std::process::Command::new(exe);
    serve
        .arg(SERVE)
        .arg(socket_path)
        .arg(rows.to_string())
        .arg(cols.to_string())
        .args(command)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    // A daemon: a session of its own, holding none of this terminal.
    fuxix::process::lead_session(&mut serve, false, Dispositions::Default);
    let mut child = serve.spawn().map_err(|e| format!("starting the server: {e}"))?;
    let mut said = String::new();
    if let Some(stdout) = child.stdout.take() {
        io::BufReader::new(stdout)
            .read_line(&mut said)
            .map_err(|e| format!("reading the server: {e}"))?;
    }
    match said.trim_end() {
        READY => Ok(()),
        "" => Err("the server exited before it listened".to_owned()),
        why => Err(why.to_owned()),
    }
}

// ---------------------------------------------------------------- serve

/// The background process: `SOCKET ROWS COLS CMD...`. Its stdout says
/// `ready` once it listens, or why it could not.
fn serve_main(args: &[String]) -> Result<(), String> {
    let [socket_path, rows, cols, command @ ..] = args else {
        return Err(USAGE.to_owned());
    };
    let socket_path = Path::new(socket_path);
    let fallback = Size::new(24, 80).ok_or("24x80")?;
    let initial = Size::new(rows.parse().unwrap_or(24), cols.parse().unwrap_or(80)).unwrap_or(fallback);
    let mut stdout = io::stdout();
    match listen(socket_path, initial, command) {
        Ok(server) => {
            let _ = writeln!(stdout, "{READY}").and_then(|()| stdout.flush());
            // The starter's pipe closes, and nothing later opened is fd 1.
            if let Ok(null) = std::fs::File::open("/dev/null") {
                let _ = fuxix::io::redirect_stdout(&null);
            }
            let served = server.serve();
            let _ = std::fs::remove_file(socket_path);
            served
        }
        Err(why) => {
            let _ = writeln!(stdout, "{why}").and_then(|()| stdout.flush());
            Err(why)
        }
    }
}

struct Server {
    listener: UnixListener,
    master: pty::Master,
    pid: Pid,
    /// The program's end (CHLD), and the server's TERM or HUP, which
    /// hang the program up.
    signals: signal::Pipe,
    clients: Vec<Client>,
    /// Input for the program the PTY did not take yet.
    to_program: Vec<u8>,
}

struct Client {
    stream: UnixStream,
    /// Packets read but not yet whole.
    read: Vec<u8>,
    /// Output it has not taken yet.
    unsent: Vec<u8>,
    /// Gone, or too slow: let go at the end of the turn.
    gone: bool,
}

/// Binds the socket, private to this user, and starts the program.
fn listen(socket_path: &Path, initial: Size, command: &[String]) -> Result<Server, String> {
    let at = |what: &str| {
        let shown = socket_path.display().to_string();
        let what = what.to_owned();
        move |e: Errno| format!("{shown}: {what}: {e}")
    };
    let fd = socket::stream().map_err(at("socket"))?;
    socket::bind(&fd, socket_path).map_err(at("bind"))?;
    // Private before anyone may connect: whoever attaches types into it.
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", socket_path.display()))?;
    socket::listen(&fd, 16).map_err(at("listen"))?;
    let listener = UnixListener::from(fd);
    listener.set_nonblocking(true).map_err(|e| format!("socket: {e}"))?;
    let signals = signal::Pipe::new(&[Signal::Chld, Signal::Term, Signal::Hup])
        .map_err(|e| format!("signals: {e}"))?;
    let (master, slave) = pty::open(initial).map_err(|e| format!("a PTY: {e}"))?;
    let (_child, pid) = slave
        .spawn(command, Dispositions::Default, |_| {})
        .map_err(|e| format!("{}: {e}", command.first().map_or("", String::as_str)))?;
    Ok(Server {
        listener,
        master,
        pid,
        signals,
        clients: Vec::new(),
        to_program: Vec::new(),
    })
}

impl Server {
    /// Serves until the program exits.
    fn serve(mut self) -> Result<(), String> {
        let mut poll = Poll::default();
        let mut buffer = vec![0u8; 64 << 10];
        let mut hung_up = false;
        loop {
            let (listener, signalled, clients, pty) = {
                let mut list = poll.list();
                let listener = list.push(PollFd::new(&self.listener, Events::IN));
                let signals = list.push(PollFd::new(self.signals.reader(), Events::IN));
                let clients: Vec<_> = self
                    .clients
                    .iter()
                    .map(|client| {
                        let wants = if client.unsent.is_empty() {
                            Events::IN
                        } else {
                            Events::IN | Events::OUT
                        };
                        list.push(PollFd::new(&client.stream, wants))
                    })
                    .collect();
                // Only while it is open: a hung-up PTY is always ready,
                // and its program's end comes as SIGCHLD.
                let pty = (!hung_up).then(|| {
                    let wants = if self.to_program.is_empty() {
                        Events::IN
                    } else {
                        Events::IN | Events::OUT
                    };
                    list.push(PollFd::new(&self.master, wants))
                });
                match list.poll(None) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(e) => return Err(format!("poll: {e}")),
                }
                (
                    !list.events(listener).is_empty(),
                    !list.events(signals).is_empty(),
                    clients.into_iter().map(|c| list.events(c)).collect::<Vec<_>>(),
                    pty.map(|p| list.events(p)),
                )
            };
            let (mut ended, mut stopped) = (false, false);
            if signalled {
                while let Some(signal) = self.signals.next() {
                    match signal {
                        Signal::Chld => ended = true,
                        _ => stopped = true,
                    }
                }
            }
            if stopped {
                // As a closed terminal does: the program's session is its pid.
                let _ = fuxix::process::kill_group(self.pid, Signal::Hup);
            }
            if ended {
                // Another stop or end of its; `reap` ignores stops.
                if !matches!(fuxix::process::reap(self.pid), Ok(None)) {
                    self.relay_output(&mut buffer);
                    self.flush_clients();
                    return Ok(());
                }
            }
            if let Some(events) = pty {
                if events.intersects(Events::OUT) {
                    self.write_program();
                }
                if events.intersects(Events::IN | Events::HUP | Events::ERR) {
                    hung_up = !self.relay_output(&mut buffer);
                }
            }
            for (index, events) in clients.into_iter().enumerate() {
                if !self.serve_client(index, events, &mut buffer)
                    && let Some(client) = self.clients.get_mut(index)
                {
                    client.gone = true;
                }
            }
            self.clients.retain(|c| !c.gone);
            if listener {
                self.accept();
            }
        }
    }

    /// Reads the program's output and sends it to every client: false once
    /// the PTY has hung up (every holder of its slave closed it).
    fn relay_output(&mut self, buffer: &mut [u8]) -> bool {
        loop {
            match fuxix::io::read(&self.master, buffer) {
                Ok(0) => return false,
                Ok(n) => {
                    let output = buffer.get(..n).unwrap_or_default();
                    for client in &mut self.clients {
                        client.unsent.extend_from_slice(output);
                    }
                    for client in &mut self.clients {
                        client.gone |= !client.send() || client.unsent.len() > BACKLOG;
                    }
                }
                Err(Errno::AGAIN) => return true,
                Err(Errno::INTR) => {}
                // EIO: the slave's last holder closed it (Linux).
                Err(_) => return false,
            }
        }
    }

    fn write_program(&mut self) {
        match fuxix::io::write(&self.master, &self.to_program) {
            Ok(n) => drop(self.to_program.drain(..n.min(self.to_program.len()))),
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(_) => self.to_program.clear(),
        }
    }

    fn accept(&mut self) {
        while let Ok((stream, _)) = self.listener.accept() {
            // Only this user may attach: the socket's mode says so, and
            // the peer's credentials make sure.
            let ours = socket::peer(&stream).is_ok_and(|p| p.uid == fuxix::process::geteuid());
            if ours && stream.set_nonblocking(true).is_ok() {
                self.clients.push(Client {
                    stream,
                    read: Vec::new(),
                    unsent: Vec::new(),
                    gone: false,
                });
            }
        }
    }

    /// Serves the client at `index`: whether to keep it.
    fn serve_client(&mut self, index: usize, events: Events, buffer: &mut [u8]) -> bool {
        let Some(client) = self.clients.get_mut(index) else {
            return false;
        };
        if events.intersects(Events::OUT) && !client.send() {
            return false;
        }
        if !events.intersects(Events::IN | Events::HUP | Events::ERR) {
            return true;
        }
        loop {
            match client.stream.read(buffer) {
                Ok(0) => return false,
                Ok(n) => client.read.extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        let read = std::mem::take(&mut client.read);
        let mut rest = read.as_slice();
        while let [kind, len, more @ ..] = rest
            && let Some((payload, after)) = more.split_at_checked(usize::from(*len))
        {
            match *kind {
                INPUT => self.to_program.extend_from_slice(payload),
                RESIZE => self.resize(payload),
                _ => return false,
            }
            rest = after;
        }
        if let Some(client) = self.clients.get_mut(index) {
            client.read = rest.to_vec();
        }
        if !self.to_program.is_empty() {
            self.write_program();
        }
        true
    }

    /// Sets the PTY's size and has its foreground group redraw: a
    /// terminal that attaches sees the program as it is now.
    fn resize(&mut self, payload: &[u8]) {
        let [r0, r1, c0, c1] = payload else { return };
        let Some(new) = Size::new(u16::from_be_bytes([*r0, *r1]), u16::from_be_bytes([*c0, *c1])) else {
            return;
        };
        let _ = terminal::set_window_size(&self.master, new, None);
        // The kernel tells a change of size alone; an attach at the same
        // size wants a redraw too.
        if let Ok(Some(group)) = terminal::foreground_group(&self.master) {
            let _ = fuxix::process::kill_group(group, Signal::Winch);
        }
    }

    /// Gives each client what it is owed, waiting a little at most: the
    /// program has ended, and its last words should arrive.
    fn flush_clients(&mut self) {
        for client in &mut self.clients {
            let _ = client.stream.set_nonblocking(false);
            let _ = client
                .stream
                .set_write_timeout(Some(std::time::Duration::from_secs(1)));
            let _ = client.stream.write_all(&client.unsent);
        }
    }
}

impl Client {
    /// Writes what it can of `unsent`: false if the client has gone.
    fn send(&mut self) -> bool {
        while !self.unsent.is_empty() {
            match self.stream.write(&self.unsent) {
                Ok(0) => return false,
                Ok(n) => drop(self.unsent.drain(..n.min(self.unsent.len()))),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        true
    }
}

// ---------------------------------------------------------------- attach

fn packets(kind: u8, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len().saturating_add(bytes.len() / 255 * 2 + 2));
    for chunk in bytes.chunks(255) {
        out.push(kind);
        out.push(u8::try_from(chunk.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(chunk);
    }
    out
}

fn size_packet() -> Option<Vec<u8>> {
    let (now, _) = terminal::window_size(io::stdin())?;
    let mut payload = now.rows.get().to_be_bytes().to_vec();
    payload.extend_from_slice(&now.cols.get().to_be_bytes());
    Some(packets(RESIZE, &payload))
}

/// How an attachment ended.
enum Ended {
    Detached,
    ProgramExited,
    Hangup,
}

fn attach(socket_path: &Path, detach: Option<u8>) -> Result<(), String> {
    let fd = socket::stream().map_err(|e| format!("socket: {e}"))?;
    socket::connect(&fd, socket_path)
        .map_err(|e| format!("{}: nothing to attach to: {e}", socket_path.display()))?;
    let mut stream = UnixStream::from(fd);
    let signals = signal::Pipe::new(&[Signal::Winch, Signal::Hup, Signal::Term])
        .map_err(|e| format!("signals: {e}"))?;
    let ended = {
        let _raw = terminal::Raw::enter(io::stdin())
            .map_err(|e| format!("attaching needs a terminal on stdin: {e}"))?;
        relay(&mut stream, &signals, detach)
    };
    let mut stderr = io::stderr();
    match ended? {
        Ended::Detached => {
            let _ = writeln!(stderr, "[detached]");
        }
        Ended::ProgramExited => {
            let _ = writeln!(stderr, "[EOF - fuxix-dtach terminating]");
        }
        Ended::Hangup => {}
    }
    Ok(())
}

fn relay(
    stream: &mut UnixStream,
    signals: &signal::Pipe,
    detach: Option<u8>,
) -> Result<Ended, String> {
    let lost = |e: io::Error| format!("the server: {e}");
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    if let Some(packet) = size_packet() {
        stream.write_all(&packet).map_err(lost)?;
    }
    let mut poll = Poll::default();
    let mut buffer = vec![0u8; 64 << 10];
    loop {
        let (typed, output, signalled) = {
            let mut list = poll.list();
            let typed = list.push(PollFd::new(&stdin, Events::IN));
            let output = list.push(PollFd::new(&*stream, Events::IN));
            let signalled = list.push(PollFd::new(signals.reader(), Events::IN));
            match list.poll(None) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(e) => return Err(format!("poll: {e}")),
            }
            let any = |entry| !list.events(entry).is_empty();
            (any(typed), any(output), any(signalled))
        };
        let mut resized = false;
        while signalled && let Some(signal) = signals.next() {
            match signal {
                Signal::Winch => resized = true,
                _ => return Ok(Ended::Hangup),
            }
        }
        if resized && let Some(packet) = size_packet() {
            stream.write_all(&packet).map_err(lost)?;
        }
        if output {
            match stream.read(&mut buffer) {
                Ok(0) => return Ok(Ended::ProgramExited),
                Ok(n) => {
                    let output = buffer.get(..n).unwrap_or_default();
                    stdout
                        .write_all(output)
                        .and_then(|()| stdout.flush())
                        .map_err(|e| format!("the terminal: {e}"))?;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(lost(e)),
            }
        }
        if typed {
            let n = match fuxix::io::read(&stdin, &mut buffer) {
                Ok(0) => return Ok(Ended::Hangup),
                Ok(n) => n,
                Err(Errno::INTR | Errno::AGAIN) => continue,
                Err(e) => return Err(format!("the terminal: {e}")),
            };
            let input = buffer.get(..n).unwrap_or_default();
            let key = detach.and_then(|key| input.iter().position(|b| *b == key));
            let typed = key.map_or(input, |at| input.get(..at).unwrap_or_default());
            stream.write_all(&packets(INPUT, typed)).map_err(lost)?;
            if key.is_some() {
                return Ok(Ended::Detached);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn detach_keys_parse() {
        assert_eq!(super::parse_key("^\\"), Ok(Some(0x1c)));
        assert_eq!(super::parse_key("^a"), Ok(Some(1)));
        assert_eq!(super::parse_key("none"), Ok(None));
        assert!(super::parse_key("^1").is_err());
    }
}
