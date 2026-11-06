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
use fuxix::io::Stream;
use fuxix::process::{Dispositions, Job, Reach, Signal, Status};
use fuxix::socket::Listener as Pipe;
use fuxix::terminal::Size;
use fuxix::{Errno, io, pty};
use std::path::PathBuf;
use std::process::ExitCode;
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

/// `unix:PATH`: the pipe that names. Windows has TCP sockets, but fuxix
/// waits on none, so `tcp:` is refused.
struct Address(PathBuf);

type Conn = Stream;

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
                address = Some(if let Some(rest) = arg.strip_prefix("unix:") {
                    Address(PathBuf::from(rest))
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
    let path = &options.address.0;
    let base = path.to_string_lossy().replace('\\', "/");
    let name = fuxix::socket::name(&format!("fuxix-ptybridge-{base}")).map_err(|e| e.to_string())?;
    let mut listener = Pipe::bind(&name).map_err(|e| format!("listening on {}: {e}", path.display()))?;
    accept_loop(&mut listener, options)
}

/// Accepts connections until a SIGINT or SIGTERM (or, with `--once`, the
/// first connection is done), one at a time: one thread polls in a process
/// here, so a connection can't have a thread of its own.
fn accept_loop(listener: &mut Pipe, options: &Options) -> Result<(), String> {
    let signals = fuxix::signal::Pipe::new(&[Signal::Int, Signal::Term])
        .map_err(|e| format!("catching signals: {e}"))?;
    eprintln!("listening on unix:{}", options.address.0.display());
    let mut poll = Poll::default();
    let mut number = 0u64;
    loop {
        let mut list = poll.list();
        let incoming = list.push(PollFd::new(&*listener, Events::IN));
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
        let conn = match listener.accept().and_then(fuxix::socket::Connection::stream) {
            Ok(conn) => conn,
            Err(Errno::AGAIN) => continue,
            Err(e) => {
                eprintln!("accept: {e}");
                continue;
            }
        };
        number = number.saturating_add(1);
        let session = Session {
            number,
            size: options.size,
            term: options.term.clone(),
            argv: options.argv.clone(),
        };
        session.run(conn);
        if options.once {
            return Ok(());
        }
    }
}

struct Session {
    number: u64,
    size: Size,
    term: String,
    argv: Vec<String>,
}

impl Session {
    fn run(self, conn: Conn) {
        let n = self.number;
        match self.bridge(&conn) {
            Ok(status) => eprintln!("conn {n}: program ended: {status:?}"),
            Err(e) => {
                eprintln!("conn {n}: {e}");
                // The client is told too, if it is still there.
                let _ = io::write(&conn, format!("fuxix-ptybridge: {e}\r\n").as_bytes());
            }
        }
        drop(conn);
    }

    fn bridge(&self, conn: &Conn) -> Result<Status, String> {
        let _ = pty::load(std::path::Path::new("no-pinned-conpty"));
        let (master, slave) = pty::open(self.size).map_err(|e| format!("opening a PTY: {e}"))?;
        let term = self.term.clone();
        let job = slave
            .spawn(
                &self.argv,
                |c| {
                    c.env("TERM", term);
                },
                Dispositions::Default,
            )
            .map_err(|e| format!("starting {}: {e}", self.argv.join(" ")))?;
        eprintln!("conn {}: started pid {}", self.number, job.leader());
        Pump::new(conn, &master, job).run()
    }
}

/// Bytes moved between the socket and the PTY master, until both are done.
struct Pump<'a> {
    conn: &'a Conn,
    master: &'a pty::Master,
    job: Job,
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
    exited: Option<Status>,
}

impl<'a> Pump<'a> {
    fn new(conn: &'a Conn, master: &'a pty::Master, job: Job) -> Self {
        Pump {
            conn,
            master,
            job,
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

    fn run(mut self) -> Result<Status, String> {
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
        // Windows' master can't be asked for modes: no end-of-file key.
        self.hung_up = Some((Instant::now(), None));
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
    fn finished(&mut self) -> Result<Option<Status>, String> {
        let now = Instant::now();
        if self.exited.is_none()
            && let Some(status) = self.job.ended().map_err(|e| format!("wait: {e}"))?
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

    /// A hang-up closes its console; a kill ends its job.
    fn signal(&self, signal: Signal) {
        match signal {
            Signal::Kill => drop(self.job.kill(Reach::Session)),
            _ => drop(pty::hang_up(self.master, &self.job)),
        }
        eprintln!("SIG{} to its job", signal.name());
    }
}
