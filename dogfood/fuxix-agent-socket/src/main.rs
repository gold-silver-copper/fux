//! fuxix-agent-socket: an ssh-agent-like daemon holding named secrets in
//! memory, served on a private Unix socket to processes of the same user.
//!
//! ```text
//! fuxix-agent-socket [--socket PATH] serve
//! fuxix-agent-socket [--socket PATH] add NAME      (the secret is stdin's first line)
//! fuxix-agent-socket [--socket PATH] get NAME
//! fuxix-agent-socket [--socket PATH] list
//! fuxix-agent-socket [--socket PATH] remove NAME
//! ```
//!
//! The protocol is one line each way: `ADD name secret`, `GET name`,
//! `LIST`, `REMOVE name`, `QUIT`; answered `OK ...` or `ERR why`.

use fuxix::Errno;
use fuxix::poll::{Entry, Events, Poll, PollFd};
use fuxix::process::Signal;
use fuxix::signal::Pipe;
use std::collections::BTreeMap;
use fuxix::io::Stream;
use fuxix::socket::{Client as Dial, Listener};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The longest request line, newline included.
const MAX_LINE: usize = 8192;
/// Connections served at once; one more is told `ERR busy`.
const MAX_CLIENTS: usize = 64;
/// Secrets held at once.
const MAX_SECRETS: usize = 1024;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut socket = None;
    if args.first().map(String::as_str) == Some("--socket") {
        if args.len() < 2 {
            return usage();
        }
        socket = Some(PathBuf::from(args.remove(1)));
        args.remove(0);
    }
    let socket = match socket.map_or_else(default_socket, Ok) {
        Ok(path) => path,
        Err(why) => return fail(&why),
    };
    let request = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["serve"] => {
            return match serve(&socket) {
                Ok(()) => ExitCode::SUCCESS,
                Err(why) => fail(&why),
            };
        }
        ["add", name] => {
            let mut secret = String::new();
            if let Err(e) = io::stdin().lock().read_line(&mut secret) {
                return fail(&format!("reading the secret from stdin: {e}"));
            }
            let secret = secret.trim_end_matches(['\n', '\r']);
            format!("ADD {name} {secret}")
        }
        ["get", name] => format!("GET {name}"),
        ["remove", name] => format!("REMOVE {name}"),
        ["list"] => "LIST".to_owned(),
        _ => return usage(),
    };
    match ask(&socket, &request) {
        Ok(answer) => match answer.strip_prefix("OK") {
            Some(rest) => {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !rest.is_empty() {
                    println!("{rest}");
                }
                ExitCode::SUCCESS
            }
            None => fail(answer.strip_prefix("ERR ").unwrap_or(&answer)),
        },
        Err(why) => fail(&why),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: fuxix-agent-socket [--socket PATH] serve | add NAME | get NAME | list | remove NAME");
    ExitCode::from(2)
}

fn fail(why: &str) -> ExitCode {
    eprintln!("fuxix-agent-socket: {why}");
    ExitCode::FAILURE
}

/// `$FUXIX_AGENT_SOCK`, else `$XDG_RUNTIME_DIR/fuxix-agent/agent.sock`,
/// else `~/.cache/fuxix-agent/agent.sock`.
fn default_socket() -> Result<PathBuf, String> {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    if let Some(path) = var("FUXIX_AGENT_SOCK") {
        return Ok(PathBuf::from(path));
    }
    let base = var("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(base).join("fuxix-agent").join("agent.sock"))
}

/// The pipe `path` names, this user's alone.
fn pipe(path: &Path) -> Result<String, String> {
    let base = path.to_string_lossy().replace('\\', "/");
    fuxix::socket::name(&format!("fuxix-agent-{base}")).map_err(|e| e.to_string())
}

/// Sends one request line and reads the one-line answer.
fn ask(socket: &Path, request: &str) -> Result<String, String> {
    let mut stream = Dial::connect(&pipe(socket)?)
        .map_err(|e| format!("no agent at {}: {e}", socket.display()))?;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|e| format!("sending: {e}"))?;
    let mut answer = String::new();
    BufReader::new(stream)
        .read_line(&mut answer)
        .map_err(|e| format!("reading the answer: {e}"))?;
    if answer.is_empty() {
        return Err("the agent closed the connection without answering".to_owned());
    }
    Ok(answer.trim_end_matches(['\n', '\r']).to_owned())
}

/// A secret's bytes, overwritten with zeros when it is dropped.
struct Secret(Vec<u8>);

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
        std::hint::black_box(&self.0);
    }
}

/// The lock beside the pipe's path, held while the agent runs.
struct Bound {
    path: PathBuf,
    _lock: File,
}

/// Takes the path: a private folder if missing, the lock beside it, and the
/// pipe's first instance, which a live agent's, or a squatter's, refuses.
fn bind(path: &Path) -> Result<(Bound, Listener), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        match fuxix::file::private_dir(dir) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => {
                return Err(format!("making {}: {e}", dir.display()));
            }
            _ => {}
        }
    }
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    let lock = fuxix::file::private_file(&mut options)
        .open(&lock_path)
        .map_err(|e| format!("opening {}: {e}", PathBuf::from(&lock_path).display()))?;
    lock.try_lock().map_err(|e| match e {
        fs::TryLockError::WouldBlock => format!("an agent is already running on {}", path.display()),
        fs::TryLockError::Error(e) => format!("locking: {e}"),
    })?;
    let listener = Listener::bind(&pipe(path)?).map_err(|e| format!("binding {}: {e}", path.display()))?;
    Ok((
        Bound {
            path: path.to_owned(),
            _lock: lock,
        },
        listener,
    ))
}

struct Client {
    stream: Stream,
    input: Vec<u8>,
    output: Vec<u8>,
    /// Close once `output` is written.
    closing: bool,
    gone: bool,
}

fn serve(path: &Path) -> Result<(), String> {
    // Made first, so a TERM arriving while binding is not lost.
    let signals = Pipe::new(&[Signal::Term, Signal::Int, Signal::Hup])
        .map_err(|e| format!("catching signals: {e}"))?;
    let (bound, mut listener) = bind(path)?;
    eprintln!("fuxix-agent-socket: listening on {}", bound.path.display());
    let mut secrets: BTreeMap<String, Secret> = BTreeMap::new();
    let mut clients: Vec<Client> = Vec::new();
    let mut poll = Poll::default();
    let stopped = loop {
        let (signalled, incoming, ready) = {
            let mut list = poll.list();
            let signal = list.push(PollFd::new(signals.reader(), Events::IN));
            let listening = list.push(PollFd::new(&listener, Events::IN));
            let entries: Vec<Entry> = clients
                .iter()
                .map(|c| {
                    let wanted = if c.output.is_empty() { Events::IN } else { Events::IN | Events::OUT };
                    list.push(PollFd::new(&c.stream, wanted))
                })
                .collect();
            match list.poll(None) {
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(e) => return Err(format!("poll: {e}")),
            }
            let ready: Vec<Events> = entries.iter().map(|&e| list.events(e)).collect();
            (list.events(signal), list.events(listening), ready)
        };
        if !signalled.is_empty() {
            let mut stop = None;
            while let Some(signal) = signals.next() {
                stop = Some(signal);
            }
            if let Some(signal) = stop {
                break signal;
            }
        }
        for (client, events) in clients.iter_mut().zip(ready) {
            if events.intersects(Events::IN | Events::HUP | Events::ERR) {
                read_requests(client, &mut secrets);
            }
            flush(client);
        }
        clients.retain(|c| !c.gone && !(c.closing && c.output.is_empty()));
        if incoming.intersects(Events::IN) {
            accept(&mut listener, &mut clients);
        }
    };
    eprintln!(
        "fuxix-agent-socket: {} received; forgetting {} secret(s) and removing {}",
        stopped.name(),
        secrets.len(),
        bound.path.display()
    );
    drop(clients);
    drop(listener);
    drop(secrets);
    drop(bound);
    Ok(())
}

/// Accepts every waiting connection whose peer is this user.
fn accept(listener: &mut Listener, clients: &mut Vec<Client>) {
    loop {
        let connection = match listener.accept() {
            Ok(connection) => connection,
            Err(Errno::AGAIN) => return,
            Err(e) => {
                eprintln!("fuxix-agent-socket: accept: {e}");
                return;
            }
        };
        let connected = connection.connected();
        match connected.and_then(|at| fuxix::socket::peer(&connection, at)) {
            Ok(peer) if peer.user.is_current() => {}
            Ok(peer) => {
                eprintln!("fuxix-agent-socket: refused {}", peer.user);
                continue;
            }
            Err(e) => {
                eprintln!("fuxix-agent-socket: refused a peer that could not be identified: {e}");
                continue;
            }
        }
        let stream = match connection.stream() {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("fuxix-agent-socket: a connection: {e}");
                continue;
            }
        };
        if clients.len() >= MAX_CLIENTS {
            let _ = fuxix::io::write_what_fits(&stream, b"ERR busy\n");
            continue;
        }
        clients.push(Client {
            stream,
            input: Vec::new(),
            output: Vec::new(),
            closing: false,
            gone: false,
        });
    }
}

/// Reads what the client sent and answers each whole line.
fn read_requests(client: &mut Client, secrets: &mut BTreeMap<String, Secret>) {
    let mut buffer = [0u8; 4096];
    loop {
        match fuxix::io::read(&client.stream, &mut buffer) {
            Ok(0) => {
                client.gone = true;
                break;
            }
            Ok(n) => client.input.extend_from_slice(buffer.get(..n).unwrap_or_default()),
            Err(Errno::AGAIN) => break,
            Err(_) => {
                client.gone = true;
                break;
            }
        }
    }
    while !client.closing
        && let Some(end) = client.input.iter().position(|&b| b == b'\n')
    {
        let line: Vec<u8> = client.input.drain(..=end).collect();
        let line = line.strip_suffix(b"\n").unwrap_or(&line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let (answer, close) = match std::str::from_utf8(line) {
            Ok(line) => handle(line, secrets),
            Err(_) => ("ERR not UTF-8".to_owned(), false),
        };
        client.output.extend_from_slice(answer.as_bytes());
        client.output.push(b'\n');
        client.closing |= close;
    }
    if !client.closing && client.input.len() >= MAX_LINE {
        client.output.extend_from_slice(b"ERR line too long\n");
        client.closing = true;
    }
    // A client that hung up after its last request still gets its answers.
    if client.gone && !client.output.is_empty() {
        client.gone = false;
        client.closing = true;
    }
}

/// Writes what fits of the client's answers.
fn flush(client: &mut Client) {
    if client.output.is_empty() {
        return;
    }
    match fuxix::io::write_what_fits(&client.stream, &client.output) {
        Ok(n) => {
            client.output.drain(..n.min(client.output.len()));
        }
        Err(_) => client.gone = true,
    }
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The answer to one request, and whether to close after it.
fn handle(line: &str, secrets: &mut BTreeMap<String, Secret>) -> (String, bool) {
    let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
    let answer = match command {
        "ADD" => {
            let (name, secret) = rest.split_once(' ').unwrap_or((rest, ""));
            if !valid_name(name) {
                "ERR a name is 1 to 64 of A-Z a-z 0-9 . _ -".to_owned()
            } else if secret.is_empty() {
                "ERR no secret given".to_owned()
            } else if !secrets.contains_key(name) && secrets.len() >= MAX_SECRETS {
                format!("ERR holding {MAX_SECRETS} secrets already")
            } else {
                match secrets.insert(name.to_owned(), Secret(secret.as_bytes().to_vec())) {
                    Some(_) => "OK replaced".to_owned(),
                    None => "OK added".to_owned(),
                }
            }
        }
        "GET" => match secrets.get(rest) {
            Some(secret) => format!("OK {}", String::from_utf8_lossy(&secret.0)),
            None => format!("ERR no secret named {rest:?}"),
        },
        "REMOVE" => match secrets.remove(rest) {
            Some(_) => "OK removed".to_owned(),
            None => format!("ERR no secret named {rest:?}"),
        },
        "LIST" if rest.is_empty() => {
            let names: Vec<&str> = secrets.keys().map(String::as_str).collect();
            format!("OK {}", names.join(" ")).trim_end().to_owned()
        }
        "QUIT" => return ("OK bye".to_owned(), true),
        _ => "ERR unknown command; use ADD, GET, LIST, REMOVE or QUIT".to_owned(),
    };
    (answer, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protocol_adds_lists_gets_and_removes() {
        let mut secrets = BTreeMap::new();
        let mut say = |line: &str| handle(line, &mut secrets).0;
        assert_eq!(say("ADD gh tok en"), "OK added");
        assert_eq!(say("ADD aws x"), "OK added");
        assert_eq!(say("ADD gh token"), "OK replaced");
        assert_eq!(say("LIST"), "OK aws gh");
        assert_eq!(say("GET gh"), "OK token");
        assert_eq!(say("REMOVE gh"), "OK removed");
        assert!(say("GET gh").starts_with("ERR"));
        assert!(say("ADD bad/name x").starts_with("ERR"));
        assert!(say("ADD empty").starts_with("ERR"));
    }
}
