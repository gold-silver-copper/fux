//! A shared shell: one PTY running a shell behind a Unix socket. Any number
//! of clients attach at once; all see the output and may type. A late
//! joiner first gets the recent output replayed from a ring buffer.
//!
//!     fuxix-shareshell serve SOCKET [--size ROWSxCOLS] [-- PROGRAM ARGS...]
//!     fuxix-shareshell attach SOCKET        (detach with Ctrl-])
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Dispositions, Pid, Signal, Status};
use fuxix::terminal::Size;
use fuxix::{Errno, io, pty, signal, socket, terminal};
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// How much recent output a late joiner is replayed.
const RING: usize = 64 * 1024;
/// How much output may wait for a slow client before it is dropped.
const BACKLOG: usize = 1 << 20;
/// The byte that detaches a client: Ctrl-], as telnet has it.
const DETACH: u8 = 0x1d;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((cmd, rest)) if cmd == "serve" => serve(rest),
        Some((cmd, [path])) if cmd == "attach" => attach(Path::new(path)),
        _ => Err(usage()),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("fuxix-shareshell: {message}");
            ExitCode::from(2)
        }
    }
}

fn usage() -> String {
    "usage: fuxix-shareshell serve SOCKET [--size ROWSxCOLS] [-- PROGRAM ARGS...]\n       \
     fuxix-shareshell attach SOCKET"
        .into()
}

/// One attached client: its socket, and the output it has yet to take.
struct Client {
    stream: UnixStream,
    pending: VecDeque<u8>,
}

/// Recent output, at most `RING` bytes.
#[derive(Default)]
struct Ring(VecDeque<u8>);

impl Ring {
    fn push(&mut self, bytes: &[u8]) {
        self.0.extend(bytes);
        let over = self.0.len().saturating_sub(RING);
        self.0.drain(..over);
    }
}

fn parse_size(text: &str) -> Option<Size> {
    let (rows, cols) = text.split_once('x')?;
    Size::new(rows.parse().ok()?, cols.parse().ok()?)
}

fn serve(args: &[String]) -> Result<ExitCode, String> {
    let (ours, program) = match args.iter().position(|a| a == "--") {
        Some(at) => (args.get(..at).unwrap_or_default(), args.get(at + 1..).unwrap_or_default()),
        None => (args, &[][..]),
    };
    let (path, size) = match ours {
        [path] => (PathBuf::from(path), None),
        [path, flag, size] if flag == "--size" => {
            (PathBuf::from(path), Some(parse_size(size).ok_or("bad --size, want ROWSxCOLS")?))
        }
        _ => return Err(usage()),
    };
    let size = size
        .or_else(|| terminal::window_size(std::io::stdin()).map(|(s, _)| s))
        .or_else(|| Size::new(24, 80))
        .ok_or("no size")?;
    let program: Vec<String> = if program.is_empty() {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]
    } else {
        program.to_vec()
    };

    let listener = listen(&path)?;
    // From here on the socket file is ours to remove, however we leave.
    let _remove = RemoveOnDrop(path.clone());
    let signals = signal::Pipe::new(&[Signal::Chld, Signal::Term, Signal::Int, Signal::Hup])
        .map_err(|e| format!("signal pipe: {e}"))?;
    let (master, slave) = pty::open(size).map_err(|e| format!("opening a PTY: {e}"))?;
    let (_child, pid) = slave
        .spawn(&program, Dispositions::Default, |c| {
            c.env("SHARESHELL", &path);
        })
        .map_err(|e| format!("starting {}: {e}", program.join(" ")))?;
    eprintln!(
        "fuxix-shareshell: {} (pid {pid}) on {}, {}x{}",
        program.join(" "),
        path.display(),
        size.rows,
        size.cols
    );

    let mut server = Server {
        master,
        master_open: true,
        to_pty: Vec::new(),
        clients: Vec::new(),
        ring: Ring::default(),
    };
    let status = server.run(&listener, &signals, pid)?;
    let note = match status {
        Status::Exited(code) => format!("\r\n[shareshell: {} exited {code}]\r\n", program.join(" ")),
        Status::Signalled { signal, .. } => {
            let name = Signal::from_raw(signal).map_or("?", Signal::name);
            format!("\r\n[shareshell: {} killed by SIG{name}]\r\n", program.join(" "))
        }
    };
    server.broadcast(note.as_bytes());
    server.flush_blocking();
    Ok(match status {
        Status::Exited(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Status::Signalled { .. } => ExitCode::from(1),
    })
}

/// A listening socket at `path` that only this user may connect to: its
/// mode is set between bind and listen, so no other user ever can.
fn listen(path: &Path) -> Result<UnixListener, String> {
    let fd = socket::stream().map_err(|e| format!("socket: {e}"))?;
    socket::bind(&fd, path).map_err(|e| format!("binding {}: {e}", path.display()))?;
    let cleanup = |e: String| {
        let _ = std::fs::remove_file(path);
        e
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| cleanup(format!("chmod {}: {e}", path.display())))?;
    socket::listen(&fd, 16).map_err(|e| cleanup(format!("listen: {e}")))?;
    let listener = UnixListener::from(fd);
    listener
        .set_nonblocking(true)
        .map_err(|e| cleanup(format!("nonblocking: {e}")))?;
    Ok(listener)
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct Server {
    master: pty::Master,
    /// False once the master reports the shell's side closed.
    master_open: bool,
    /// Typed input the PTY has not taken yet.
    to_pty: Vec<u8>,
    clients: Vec<Client>,
    ring: Ring,
}

impl Server {
    /// Serves until the program ends; how it ended.
    fn run(&mut self, listener: &UnixListener, signals: &signal::Pipe, pid: Pid) -> Result<Status, String> {
        let mut poll = Poll::default();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let (signalled, listen, pty, clients) = {
                let mut list = poll.list();
                let signalled = list.push(PollFd::new(signals.reader(), Events::IN));
                let listen = list.push(PollFd::new(listener, Events::IN));
                // Once the shell's side has closed the master reports HUP
                // unasked, so it is left out rather than waited on for nothing.
                let pty = self.master_open.then(|| {
                    let mut want = Events::IN;
                    if !self.to_pty.is_empty() {
                        want |= Events::OUT;
                    }
                    list.push(PollFd::new(&self.master, want))
                });
                let clients: Vec<_> = self
                    .clients
                    .iter()
                    .map(|client| {
                        let mut want = Events::IN;
                        if !client.pending.is_empty() {
                            want |= Events::OUT;
                        }
                        list.push(PollFd::new(&client.stream, want))
                    })
                    .collect();
                match list.poll(None) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(e) => return Err(format!("poll: {e}")),
                }
                (
                    list.events(signalled),
                    list.events(listen),
                    pty.map_or(Events::NONE, |pty| list.events(pty)),
                    clients.into_iter().map(|c| list.events(c)).collect::<Vec<_>>(),
                )
            };

            if !signalled.is_empty() {
                while let Some(signal) = signals.next() {
                    if signal != Signal::Chld {
                        // Asked to stop: hang the shell up as a closed terminal
                        // would, and serve on until it has gone.
                        let _ = fuxix::process::kill_group(pid, Signal::Hup);
                    } else if let Some(status) =
                        // A stop of the shell is reported too; then nothing is reaped.
                        fuxix::process::reap(pid).map_err(|e| format!("reap: {e}"))?
                    {
                        self.drain_master(&mut buf);
                        return Ok(status);
                    }
                }
            }
            if pty.intersects(Events::IN | Events::HUP | Events::ERR) {
                self.drain_master(&mut buf);
            }
            if pty.intersects(Events::OUT) {
                self.write_pty();
            }
            for (at, events) in clients.iter().enumerate().rev() {
                if !self.service(at, *events, &mut buf) {
                    self.clients.remove(at);
                    eprintln!("fuxix-shareshell: a client left ({} attached)", self.clients.len());
                }
            }
            if listen.intersects(Events::IN) {
                self.accept(listener);
            }
        }
    }

    /// Reads all the master has: to the ring and to every client.
    fn drain_master(&mut self, buf: &mut [u8]) {
        while self.master_open {
            match io::read(&self.master, buf) {
                Ok(0) | Err(Errno::IO) => self.master_open = false,
                Ok(n) => {
                    let bytes = buf.get(..n).unwrap_or_default();
                    self.ring.push(bytes);
                    self.broadcast(bytes);
                }
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => break,
                Err(e) => {
                    eprintln!("fuxix-shareshell: reading the PTY: {e}");
                    self.master_open = false;
                }
            }
        }
    }

    fn broadcast(&mut self, bytes: &[u8]) {
        for client in &mut self.clients {
            client.pending.extend(bytes);
        }
        self.clients.retain_mut(|c| flush(c) && c.pending.len() <= BACKLOG);
    }

    fn write_pty(&mut self) {
        match io::write_what_fits(&self.master, &self.to_pty) {
            Ok(n) => drop(self.to_pty.drain(..n)),
            Err(_) => self.to_pty.clear(),
        }
    }

    /// Handles a client's events; false if it is gone.
    fn service(&mut self, at: usize, events: Events, buf: &mut [u8]) -> bool {
        let Some(client) = self.clients.get_mut(at) else {
            return false;
        };
        if events.intersects(Events::OUT) && !flush(client) {
            return false;
        }
        if events.intersects(Events::IN | Events::HUP | Events::ERR) {
            loop {
                match client.stream.read(buf) {
                    Ok(0) => return false,
                    Ok(n) => self.to_pty.extend_from_slice(buf.get(..n).unwrap_or_default()),
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => return false,
                }
            }
            self.write_pty();
        }
        true
    }

    fn accept(&mut self, listener: &UnixListener) {
        loop {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            // The socket's mode already keeps other users out; the kernel's
            // word on who connected is checked as well.
            let ours = socket::peer(&stream).is_ok_and(|p| p.uid == fuxix::process::geteuid());
            if !ours || stream.set_nonblocking(true).is_err() {
                continue;
            }
            let mut client = Client { stream, pending: self.ring.0.clone() };
            if flush(&mut client) {
                self.clients.push(client);
                eprintln!("fuxix-shareshell: a client joined ({} attached)", self.clients.len());
            }
        }
    }

    /// Gives every client what it is owed, waiting up to a second in all.
    fn flush_blocking(&mut self) {
        for client in &mut self.clients {
            let _ = client.stream.set_nonblocking(false);
            let _ = client.stream.set_write_timeout(Some(std::time::Duration::from_secs(1)));
            let (a, b) = client.pending.as_slices();
            let _ = client.stream.write_all(a).and_then(|()| client.stream.write_all(b));
        }
    }
}

/// Writes what the client can take now; false if it is gone.
fn flush(client: &mut Client) -> bool {
    while !client.pending.is_empty() {
        let (front, _) = client.pending.as_slices();
        match client.stream.write(front) {
            Ok(0) => return false,
            Ok(n) => drop(client.pending.drain(..n)),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
    true
}

fn attach(path: &Path) -> Result<ExitCode, String> {
    let mut stream =
        UnixStream::connect(path).map_err(|e| format!("connecting to {}: {e}", path.display()))?;
    let stdin = std::io::stdin();
    let raw = terminal::attributes(&stdin)
        .is_ok()
        .then(|| terminal::Raw::enter(std::io::stdin()))
        .transpose()
        .map_err(|e| format!("raw mode: {e}"))?;
    if raw.is_some() {
        eprint!("[shareshell: attached to {}; Ctrl-] detaches]\r\n", path.display());
    }
    let stdin_fd = &stdin;
    let mut stdout = std::io::stdout().lock();
    let mut buf = vec![0u8; 16 * 1024];
    let mut stdin_open = true;
    let mut poll = Poll::default();
    loop {
        let (input, output) = {
            let mut list = poll.list();
            let input = list.push(PollFd::new(&stream, Events::IN));
            let output = list.push(PollFd::new(stdin_fd, if stdin_open { Events::IN } else { Events::NONE }));
            match list.poll(None) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(e) => return Err(format!("poll: {e}")),
            }
            (list.events(input), list.events(output))
        };
        if input.intersects(Events::IN | Events::HUP | Events::ERR) {
            match stream.read(&mut buf) {
                Ok(0) => return Ok(ExitCode::SUCCESS),
                Ok(n) => {
                    stdout
                        .write_all(buf.get(..n).unwrap_or_default())
                        .and_then(|()| stdout.flush())
                        .map_err(|e| format!("stdout: {e}"))?;
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("reading the server: {e}")),
            }
        }
        if stdin_open && output.intersects(Events::IN | Events::HUP | Events::ERR) {
            match io::read(stdin_fd, &mut buf) {
                Ok(0) => {
                    // Input ended (a pipe): send no more, read to the end.
                    stdin_open = false;
                    let _ = stream.shutdown(std::net::Shutdown::Write);
                }
                Ok(n) => {
                    let bytes = buf.get(..n).unwrap_or_default();
                    let (keep, detach) = match bytes.iter().position(|&b| b == DETACH) {
                        Some(at) if raw.is_some() => (bytes.get(..at).unwrap_or_default(), true),
                        _ => (bytes, false),
                    };
                    stream.write_all(keep).map_err(|e| format!("writing the server: {e}"))?;
                    if detach {
                        eprint!("\r\n[shareshell: detached]\r\n");
                        return Ok(ExitCode::SUCCESS);
                    }
                }
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(e) => return Err(format!("reading stdin: {e}")),
            }
        }
    }
}
