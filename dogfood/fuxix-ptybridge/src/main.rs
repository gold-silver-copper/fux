//! fuxix-ptybridge: a small socat. Listens on a TCP or Unix socket and, for
//! each connection, runs a program on a PTY of its own, the socket's bytes
//! being the terminal's input and output.
//!
//!   fuxix-ptybridge [--once] [--raw] [--size ROWSxCOLS] [--term TERM]
//!                   (tcp:HOST:PORT | unix:PATH) -- PROGRAM [ARGS...]
//!
//! The connection closing ends the program: its pending input is written,
//! then, if the terminal edits lines then, its end-of-file key, then after a
//! grace period a SIGHUP to its process group, then a SIGKILL. The program ending ends the
//! connection: its last output is sent, then the socket is shut down.
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Dispositions, Pid, Signal};
use fuxix::terminal::{self, Size};
use fuxix::{Errno, io, pty};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, ExitCode};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: fuxix-ptybridge [--once] [--raw] [--size ROWSxCOLS] [--term TERM] \
                     (tcp:HOST:PORT | unix:PATH) -- PROGRAM [ARGS...]";

/// How much is buffered each way before the reading side is paused.
const BUFFER: usize = 64 * 1024;
/// How long the program may run on after the client hung up, before SIGHUP.
const EOF_GRACE: Duration = Duration::from_millis(500);
/// How long after SIGHUP before SIGKILL.
const HUP_GRACE: Duration = Duration::from_secs(2);

struct Options {
    once: bool,
    raw: bool,
    size: Size,
    term: String,
    address: Address,
    argv: Vec<String>,
}

enum Address {
    Tcp(String),
    Unix(PathBuf),
}

enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener, PathBuf),
}

enum Conn {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl AsFd for Listener {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Listener::Tcp(l) => l.as_fd(),
            Listener::Unix(l, _) => l.as_fd(),
        }
    }
}

impl AsFd for Conn {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Conn::Tcp(s) => s.as_fd(),
            Conn::Unix(s) => s.as_fd(),
        }
    }
}

impl Conn {
    fn shutdown(&self, how: Shutdown) {
        // A peer already gone makes this fail, which changes nothing.
        let _ = match self {
            Conn::Tcp(s) => s.shutdown(how),
            Conn::Unix(s) => s.shutdown(how),
        };
    }
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("fuxix-ptybridge: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match serve(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("fuxix-ptybridge: {message}");
            ExitCode::FAILURE
        }
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let (mut once, mut raw, mut size, mut term) = (false, false, None, None);
    let mut address = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--once" => once = true,
            "--raw" => raw = true,
            "--size" => size = Some(parse_size(&args.next().ok_or("--size needs a value")?)?),
            "--term" => term = Some(args.next().ok_or("--term needs a value")?),
            "--" => break,
            _ if address.is_none() => {
                address = Some(if let Some(rest) = arg.strip_prefix("tcp:") {
                    Address::Tcp(rest.to_owned())
                } else if let Some(rest) = arg.strip_prefix("unix:") {
                    Address::Unix(PathBuf::from(rest))
                } else {
                    return Err(format!("not an address or option: {arg}"));
                });
            }
            _ => {
                return Err(format!(
                    "unexpected argument: {arg} (put the program after --)"
                ));
            }
        }
    }
    let argv: Vec<String> = args.collect();
    if argv.is_empty() {
        return Err("no program to run".into());
    }
    Ok(Options {
        once,
        raw,
        size: size.or(Size::new(24, 80)).ok_or("a zero size")?,
        term: term.unwrap_or_else(|| "xterm-256color".into()),
        address: address.ok_or("no address")?,
        argv,
    })
}

fn parse_size(text: &str) -> Result<Size, String> {
    let bad = || format!("bad size {text:?}, want ROWSxCOLS like 24x80");
    let (rows, cols) = text.split_once('x').ok_or_else(bad)?;
    let rows = rows.parse().map_err(|_| bad())?;
    let cols = cols.parse().map_err(|_| bad())?;
    Size::new(rows, cols).ok_or_else(bad)
}

fn serve(options: &Options) -> Result<(), String> {
    let listener = match &options.address {
        Address::Tcp(addr) => {
            Listener::Tcp(TcpListener::bind(addr).map_err(|e| format!("listening on {addr}: {e}"))?)
        }
        Address::Unix(path) => Listener::Unix(
            UnixListener::bind(path)
                .map_err(|e| format!("listening on {}: {e}", path.display()))?,
            path.clone(),
        ),
    };
    let result = accept_loop(&listener, options);
    if let Listener::Unix(_, path) = &listener {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// Accepts connections until a SIGINT or SIGTERM (or, with `--once`, the
/// first connection is done), each served on a thread of its own.
fn accept_loop(listener: &Listener, options: &Options) -> Result<(), String> {
    let signals = fuxix::signal::Pipe::new(&[Signal::Int, Signal::Term])
        .map_err(|e| format!("catching signals: {e}"))?;
    match listener {
        Listener::Tcp(l) => eprintln!(
            "listening on tcp:{}",
            l.local_addr().map_err(|e| e.to_string())?
        ),
        Listener::Unix(_, path) => eprintln!("listening on unix:{}", path.display()),
    }
    let mut poll = Poll::default();
    let mut number = 0u64;
    loop {
        let mut list = poll.list();
        let incoming = list.push(PollFd::new(listener, Events::IN));
        let signalled = list.push(PollFd::new(signals.reader(), Events::IN));
        match list.poll(None) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(format!("poll: {e}")),
        }
        if !list.events(signalled).is_empty() {
            eprintln!("signalled, stopping");
            return Ok(());
        }
        if !list.events(incoming).intersects(Events::IN) {
            continue;
        }
        let conn = match listener {
            Listener::Tcp(l) => l.accept().map(|(s, _)| Conn::Tcp(s)),
            Listener::Unix(l, _) => l.accept().map(|(s, _)| Conn::Unix(s)),
        };
        let conn = match conn {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("accept: {e}");
                continue;
            }
        };
        number = number.saturating_add(1);
        let session = Session {
            number,
            raw: options.raw,
            size: options.size,
            term: options.term.clone(),
            argv: options.argv.clone(),
        };
        if options.once {
            session.run(conn);
            return Ok(());
        }
        let spawned = std::thread::Builder::new()
            .name(format!("conn {number}"))
            .spawn(move || session.run(conn));
        if let Err(e) = spawned {
            eprintln!("conn {number}: no thread: {e}");
        }
    }
}

struct Session {
    number: u64,
    raw: bool,
    size: Size,
    term: String,
    argv: Vec<String>,
}

impl Session {
    fn run(self, conn: Conn) {
        let n = self.number;
        match self.bridge(&conn) {
            Ok(status) => eprintln!("conn {n}: program ended: {status}"),
            Err(e) => {
                eprintln!("conn {n}: {e}");
                // The client is told too, if it is still there.
                let _ = io::write(&conn, format!("fuxix-ptybridge: {e}\r\n").as_bytes());
            }
        }
        conn.shutdown(Shutdown::Both);
    }

    fn bridge(&self, conn: &Conn) -> Result<std::process::ExitStatus, String> {
        let (master, slave) = pty::open(self.size).map_err(|e| format!("opening a PTY: {e}"))?;
        if self.raw {
            let mut modes = terminal::attributes(&slave).map_err(|e| format!("tcgetattr: {e}"))?;
            modes.make_raw();
            terminal::set_attributes(&slave, &modes).map_err(|e| format!("tcsetattr: {e}"))?;
        }
        let term = self.term.clone();
        let (child, pid) = slave
            .spawn(&self.argv, Dispositions::Default, |c| {
                c.env("TERM", term);
            })
            .map_err(|e| format!("starting {}: {e}", self.argv.join(" ")))?;
        eprintln!("conn {}: started pid {pid}", self.number);
        io::set_nonblocking(conn, true).map_err(|e| format!("O_NONBLOCK: {e}"))?;
        Pump::new(conn, &master, child, pid).run()
    }
}

/// Bytes moved between the socket and the PTY master, until both are done.
struct Pump<'a> {
    conn: &'a Conn,
    master: &'a pty::Master,
    child: Child,
    pid: Pid,
    to_pty: Vec<u8>,
    to_conn: Vec<u8>,
    /// The client may still send.
    conn_reading: bool,
    /// The client may still be written to.
    conn_writing: bool,
    /// The master may still give output.
    pty_reading: bool,
    /// When the client hung up, and the end-of-file key still to send.
    hung_up: Option<(Instant, Option<u8>)>,
    hupped: Option<Instant>,
    exited: Option<std::process::ExitStatus>,
}

impl<'a> Pump<'a> {
    fn new(conn: &'a Conn, master: &'a pty::Master, child: Child, pid: Pid) -> Self {
        Pump {
            conn,
            master,
            child,
            pid,
            to_pty: Vec::new(),
            to_conn: Vec::new(),
            conn_reading: true,
            conn_writing: true,
            pty_reading: true,
            hung_up: None,
            hupped: None,
            exited: None,
        }
    }

    fn run(mut self) -> Result<std::process::ExitStatus, String> {
        let mut poll = Poll::default();
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            if let Some(status) = self.finished()? {
                return Ok(status);
            }
            let (mut want_conn, mut want_pty) = (Events::NONE, Events::NONE);
            if self.conn_reading && self.to_pty.len() < BUFFER {
                want_conn |= Events::IN;
            }
            if self.conn_writing && !self.to_conn.is_empty() {
                want_conn |= Events::OUT;
            }
            if self.pty_reading && self.to_conn.len() < BUFFER {
                want_pty |= Events::IN;
            }
            if !self.to_pty.is_empty() {
                want_pty |= Events::OUT;
            }
            // A side waited on for nothing is left out: a hung-up socket or
            // master is told ready (HUP) unasked, and would spin the loop.
            let mut list = poll.list();
            if !want_conn.is_empty() {
                list.push(PollFd::new(self.conn, want_conn));
            }
            if !want_pty.is_empty() {
                list.push(PollFd::new(self.master, want_pty));
            }
            match list.poll(Some(Duration::from_millis(50))) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(e) => return Err(format!("poll: {e}")),
            }
            // Readiness is not trusted: each side is tried as far as it goes,
            // a nonblocking call saying AGAIN when there is nothing to do.
            self.read_conn(&mut buffer);
            self.write_pty();
            self.read_pty(&mut buffer);
            self.write_conn();
        }
    }

    fn read_conn(&mut self, buffer: &mut [u8]) {
        while self.conn_reading && self.to_pty.len() < BUFFER {
            match io::read(self.conn, buffer) {
                Ok(0) => self.client_hung_up(),
                Ok(n) => self
                    .to_pty
                    .extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => return,
                Err(_) => self.client_hung_up(),
            }
        }
    }

    fn client_hung_up(&mut self) {
        self.conn_reading = false;
        // The master reads the modes the program has set: a line-editing
        // terminal is told the input ended as a typist would tell it.
        let key = terminal::attributes(self.master)
            .ok()
            .filter(|modes| modes.edits_lines())
            .and_then(|modes| modes.end_of_file());
        self.hung_up = Some((Instant::now(), key));
    }

    fn write_pty(&mut self) {
        match io::write_what_fits(self.master, &self.to_pty) {
            Ok(n) => drop(self.to_pty.drain(..n.min(self.to_pty.len()))),
            // The program is gone, and what it did not read is dropped.
            Err(_) => self.to_pty.clear(),
        }
        // What did not fit waits for the terminal's input queue to empty.
        if !self.to_pty.is_empty() {
            return;
        }
        // All the client sent is written: now its end of file, as a key.
        if let Some(key) = self.hung_up.as_mut().and_then(|(_, key)| key.take()) {
            self.to_pty.push(key);
        }
    }

    fn read_pty(&mut self, buffer: &mut [u8]) {
        while self.pty_reading && self.to_conn.len() < BUFFER {
            match io::read(self.master, buffer) {
                Ok(n) if n > 0 => self
                    .to_conn
                    .extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(Errno::INTR) => {}
                // Once the program ended, nothing left to read is all it wrote.
                Err(Errno::AGAIN) if self.exited.is_some() => self.pty_reading = false,
                Err(Errno::AGAIN) => return,
                // 0, or EIO on Linux: no slave is open any more.
                Ok(_) | Err(_) => self.pty_reading = false,
            }
        }
    }

    fn write_conn(&mut self) {
        if self.conn_writing && !self.to_conn.is_empty() {
            match io::write_what_fits(self.conn, &self.to_conn) {
                Ok(n) => drop(self.to_conn.drain(..n.min(self.to_conn.len()))),
                // The client is gone both ways.
                Err(e) => {
                    if !e.is_peer_gone() {
                        eprintln!("writing to the client: {e}");
                    }
                    self.conn_writing = false;
                    if self.conn_reading {
                        self.client_hung_up();
                    }
                }
            }
        }
        if !self.conn_writing {
            self.to_conn.clear();
        }
    }

    /// Reaps the program if it ended, signals it if the client left long
    /// enough ago, and says when the connection is done.
    fn finished(&mut self) -> Result<Option<std::process::ExitStatus>, String> {
        let now = Instant::now();
        if self.exited.is_none()
            && let Some(status) = self.child.try_wait().map_err(|e| format!("wait: {e}"))?
        {
            self.exited = Some(status);
        }
        if let Some(status) = self.exited {
            if !self.pty_reading && (self.to_conn.is_empty() || !self.conn_writing) {
                return Ok(Some(status));
            }
            return Ok(None);
        }
        if let Some((at, _)) = self.hung_up {
            match self.hupped {
                None if now.saturating_duration_since(at) >= EOF_GRACE => {
                    self.signal(Signal::Hup);
                    self.hupped = Some(now);
                }
                Some(at) if now.saturating_duration_since(at) >= HUP_GRACE => {
                    self.signal(Signal::Kill);
                    self.hupped = Some(now);
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// Signals the program's process group: it leads a session, so the
    /// group is its pid.
    fn signal(&self, signal: Signal) {
        if let Err(e) = fuxix::process::kill_group(self.pid, signal)
            && e != Errno::SRCH
        {
            eprintln!("kill -{} -{}: {e}", signal.name(), self.pid);
        }
    }
}
