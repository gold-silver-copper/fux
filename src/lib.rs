//! fux: a terminal multiplexer. One server holds workspaces, tabs and split
//! panes; `fux attach` shows them in a terminal; every other `fux` command
//! changes them.
pub use fux_vt::bytes;
pub use fux_vt::keys;
pub use fux_vt::keys::{decode, encode};
pub mod capture;
pub mod client;
pub mod command;
pub mod config;
pub mod copy;
pub mod events;
pub mod help;
pub mod id;
pub mod input;
pub mod json;
pub mod layout;
pub mod marks;
pub mod outer;
pub mod overlay;
pub mod pane;
pub mod process;
pub mod protocol;
pub mod render;
pub mod server;
pub mod session;
pub mod socket;
pub mod view;
pub mod wait;
pub mod words;
pub mod workspace;

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// `wait` after `from`. A time too far off for an `Instant` is taken as
/// `from`, so that a deadline fires at once rather than never.
pub(crate) fn after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
}

/// A socket that becomes readable when any of `signals` arrives: each writes
/// a byte to the other end, of which it holds a copy.
pub(crate) fn signal_pipe(signals: &[std::ffi::c_int]) -> std::io::Result<UnixStream> {
    let (pipe, write) = UnixStream::pair()?;
    pipe.set_nonblocking(true)?;
    for &signal in signals {
        signal_hook::low_level::pipe::register(signal, write.try_clone()?)?;
    }
    Ok(pipe)
}

/// Reads whatever a signal pipe holds.
pub(crate) fn drain(mut pipe: &UnixStream) {
    let mut buffer = [0u8; 256];
    while matches!(pipe.read(&mut buffer), Ok(n) if n > 0) {}
}

/// `fux help`: how to run fux, and its commands as their table has them.
fn usage() -> String {
    format!(
        "\
Driving programs from a script or an agent: `fux help agents`.
One command at length, with an example: `fux help COMMAND`.

usage: fux [-L NAME | -S PATH] [attach] [-t WORKSPACE] [--nested] [--read-only]
       fux [-L NAME | -S PATH] attach --headless [--size COLSxROWS] [--log FILE] [-t WORKSPACE]
       fux [-L NAME | -S PATH] server [--background] [--fresh] [--config FILE]
       fux [-L NAME | -S PATH] status|servers [--json]
       fux check-config [FILE]
       fux [-L NAME | -S PATH] COMMAND [ARGS...]

{}
-c CLIENT names the client whose screen a command acts on; from a key, the
command prompt or a menu, it is the client that ran it. Any command takes
--json: its errors are then JSON on stderr, {{\"error\":CODE,\"message\":…}}.

The server: -S PATH, its socket; -L NAME, the socket NAME.sock in fux's
directory, $XDG_RUNTIME_DIR/fux (else $TMPDIR/fux); else FUX_SOCKET; else
server.sock there. Inside a pane, FUX_PANE names it, so commands there target
it without -t, when they reach that pane's server (FUX_SERVER).",
        command::help()
    )
}

/// A command's help, the client's own (`attach`, `server`) included.
fn help_of(name: &str) -> Option<String> {
    let own = match name {
        "attach" => {
            "usage: fux [attach] [-t WORKSPACE] [--nested] [--read-only]\n       \
             fux attach --headless [--size COLSxROWS] [--log FILE] [-t WORKSPACE]\n\n\
             Shows the server's screen in this terminal, starting a server if none \
             runs. --read-only\nwatches: keys and clicks do nothing but detach. \
             --headless attaches a client with no\nterminal: it prints its id, logs \
             what it is sent to FILE, and detaches on SIGTERM;\nsend-keys -c and \
             capture-client act on it.\n\n  fux attach --headless --size 100x30 &"
        }
        "server" => {
            "usage: fux [-L NAME | -S PATH] server [--background] [--fresh] [--config FILE]\n\n\
             Runs a server with no client: what `fux` starts on its own when none runs.\n\
             In the foreground; --background returns once it answers, and prints its\n\
             socket. -S PATH (or --socket PATH) and -L NAME choose the socket. A server\n\
             brings back what one on its socket held when it crashed or the machine\n\
             stopped (programs `run` started wait for `respawn`); --fresh starts empty.\n\
             A private one:\n\n  \
             fux -L test server --background && fux -L test run -n app -- ./app"
        }
        "status" => {
            "usage: fux [-L NAME | -S PATH] status [--json]\n\n\
             Whether a server answers on the socket, its version and this one's: \
             status 0 if one\nanswers that this fux can drive, 1 if none does or it \
             is of another protocol.\n\n  fux -L test status --json"
        }
        "servers" => {
            "usage: fux servers [--json]\n\n\
             The servers that answer in fux's directory, $XDG_RUNTIME_DIR/fux: each \
             one's name\n(for -L) and socket.\n\n  fux servers"
        }
        "check-config" => {
            "usage: fux check-config [FILE]\n\n\
             Reads a config file (the server's, without FILE) as a server would, \
             and prints every\nline it refuses, with why; status 1 if any. Needs no \
             server.\n\n  fux check-config ~/.config/fux/fux.conf"
        }
        _ => return help::command(name),
    };
    Some(own.to_owned())
}

/// The command line of the `fux` binary.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(status) => ExitCode::from(status),
        Err(error) => {
            let text = match wants_json(&args) {
                true => json_error(error.code(), &error.to_string()),
                false => format!("fux: {error}"),
            };
            // Nowhere is left to say that stderr failed.
            let _ = writeln!(std::io::stderr(), "{text}");
            ExitCode::from(1)
        }
    }
}

/// Whether a command line asks for JSON: its errors are then JSON too.
fn wants_json(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--json")
}

/// An error as JSON: a stable code and the message.
fn json_error(code: &str, message: &str) -> String {
    json::Json::Object(vec![
        ("error", json::Json::str(code)),
        ("message", json::Json::str(message)),
    ])
    .render()
}

/// Why the `fux` binary failed: exit status 1.
#[derive(Debug)]
enum Error {
    /// `fux attach` inside a pane, without `--nested`.
    Nested,
    Setsid(fuxix::Errno),
    Socket(socket::Error),
    Client(client::Error),
    Server(server::Error),
    /// Help or the version could not be written.
    Output(std::io::Error),
    /// A file `--paste-file` names, or stdin, could not be pasted.
    Paste(String),
}

impl Error {
    /// What kind of failure it is, as `--json` names it.
    fn code(&self) -> &'static str {
        match self {
            Error::Nested => "nested",
            Error::Setsid(_) | Error::Server(_) => "server",
            Error::Socket(socket::Error::NoServer(_)) => "no_server",
            Error::Socket(_) => "socket",
            Error::Client(client::Error::Mismatch(_)) => "mismatch",
            Error::Client(
                client::Error::Connect { .. } | client::Error::Socket(socket::Error::NoServer(_)),
            ) => "no_server",
            Error::Client(_) => "client",
            Error::Output(_) => "output",
            Error::Paste(_) => "paste",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Nested => f.write_str(
                "this is already a fux pane; attaching here would show fux inside itself (--nested does it anyway)",
            ),
            Error::Setsid(errno) => write!(f, "setsid: {errno}"),
            Error::Socket(error) => error.fmt(f),
            Error::Client(error) => error.fmt(f),
            Error::Server(error) => error.fmt(f),
            Error::Output(error) => write!(f, "writing the output: {error}"),
            Error::Paste(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Nested | Error::Paste(_) => None,
            Error::Setsid(errno) => Some(errno),
            Error::Socket(error) => Some(error),
            Error::Client(error) => Some(error),
            Error::Server(error) => Some(error),
            Error::Output(error) => Some(error),
        }
    }
}

impl From<socket::Error> for Error {
    fn from(error: socket::Error) -> Error {
        Error::Socket(error)
    }
}

impl From<client::Error> for Error {
    fn from(error: client::Error) -> Error {
        Error::Client(error)
    }
}

/// `COLSxROWS`, as `attach --size` takes it: rows and columns.
fn headless_size(text: &str) -> Option<(NonZeroU16, NonZeroU16)> {
    let (cols, rows) = text.split_once('x')?;
    // At most 500 cells each way, as a pane's size.
    let cell = |n: &str| n.parse::<NonZeroU16>().ok().filter(|n| n.get() <= 500);
    Some((cell(rows)?, cell(cols)?))
}

fn nonzero(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

/// A usage error: what is wrong, and the usage of the command it was in,
/// if it names one, rather than every command's; as JSON with `json`.
fn usage_error(message: &str, command: Option<&str>, json: bool) -> Result<u8, Error> {
    let usage = (command.and_then(command::usage_of)).map_or_else(
        || "`fux help` lists commands".to_owned(),
        |usage| format!("usage: fux {usage}"),
    );
    // A message that is itself the usage, or names `fux help`, needs no more.
    let usage = if message.contains("fux help") || message.starts_with("usage:") {
        String::new()
    } else {
        format!("\n{usage}")
    };
    let text = match json {
        true => json_error("usage", &format!("{message}{usage}")),
        false => format!("fux: {message}{usage}"),
    };
    // The status says it, if stderr cannot.
    let _ = writeln!(std::io::stderr(), "{text}");
    Ok(2)
}

/// The server a command line names before its command (`-L NAME`,
/// `-S PATH`), and the rest of it.
fn chosen(args: &[String]) -> Result<(socket::Choice<'_>, &[String]), String> {
    let mut choice = socket::Choice::Default;
    let mut rest = args;
    while let Some((flag, after)) = rest.split_first() {
        let named = match flag.as_str() {
            "-L" => socket::Choice::Name,
            "-S" => |path| socket::Choice::Path(path, "-S"),
            _ => break,
        };
        let Some((value, after)) = after.split_first() else {
            return Err(format!("{flag} needs a value: -L NAME or -S PATH"));
        };
        if choice.named() {
            return Err("name the server once: -L NAME or -S PATH".to_owned());
        }
        choice = named(value.as_str());
        rest = after;
    }
    Ok((choice, rest))
}

/// The most a paste from a file or stdin may hold: what one command's
/// frame, and a pane's input, take at once.
const PASTE_FILE_LIMIT: usize = 900 * 1024;

/// `send-keys --paste-file FILE` and `--paste -` (stdin), read here, where
/// the caller's files are: the command line with each as `--paste TEXT`.
fn pasted(args: &[String]) -> Result<Vec<String>, Error> {
    if args.first().map(String::as_str) != Some("send-keys") {
        return Ok(args.to_vec());
    }
    let read = |from: &str| -> Result<String, Error> {
        let mut bytes = Vec::new();
        let limit = (PASTE_FILE_LIMIT as u64).saturating_add(1);
        let read = match from {
            "-" => std::io::stdin().take(limit).read_to_end(&mut bytes),
            path => std::fs::File::open(path).and_then(|f| f.take(limit).read_to_end(&mut bytes)),
        };
        let name = if from == "-" { "stdin" } else { from };
        read.map_err(|e| Error::Paste(format!("--paste {name}: {e}")))?;
        if bytes.len() > PASTE_FILE_LIMIT {
            return Err(Error::Paste(format!(
                "--paste {name}: more than {} KiB, the most one paste takes",
                PASTE_FILE_LIMIT / 1024
            )));
        }
        String::from_utf8(bytes)
            .map_err(|_| Error::Paste(format!("--paste {name}: not UTF-8 text")))
    };
    let mut out = Vec::with_capacity(args.len());
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--" => {
                out.push(word.clone());
                out.extend(words.by_ref().cloned());
            }
            "--paste-file" | "--paste" => {
                let Some(value) = words.next() else {
                    out.push(word.clone());
                    continue;
                };
                out.push("--paste".to_owned());
                out.push(match (word.as_str(), value.as_str()) {
                    ("--paste", "-") | ("--paste-file", _) => read(value)?,
                    _ => value.clone(),
                });
            }
            // A flag's value is never one of these.
            "-t" | "-c" | "-N" | "--text" | "--wait-text" | "--wait-gone" | "--wait-idle"
            | "--timeout" | "--if-revision" => {
                out.push(word.clone());
                out.extend(words.next().cloned());
            }
            _ => out.push(word.clone()),
        }
    }
    Ok(out)
}

fn run(args: &[String]) -> Result<u8, Error> {
    let json = wants_json(args);
    let (choice, args) = match chosen(args) {
        Ok(chosen) => chosen,
        Err(message) => return usage_error(&message, None, json),
    };
    // The binary's own commands take --json too: their errors, and
    // check-config's findings, are then JSON.
    let own = matches!(
        args.first().map(String::as_str),
        None | Some(
            "attach"
                | "-t"
                | "--nested"
                | "--headless"
                | "--read-only"
                | "server"
                | "help"
                | "--help"
                | "-h"
                | "check-config"
                | "--json"
        )
    );
    let plain: Vec<String>;
    let args = match own {
        true => {
            plain = args.iter().filter(|a| *a != "--json").cloned().collect();
            plain.as_slice()
        }
        false => args,
    };
    let first = args.first().map(String::as_str);
    // `fux COMMAND … --help`: that command's help, not a run of it
    // (`kill-server --help` must not stop anything). Right after the
    // command, or anywhere before `--` in a line that does not parse
    // otherwise (`--text --help` types it); never for a program the server
    // starts (`__launch … --help`).
    let anywhere = (args.iter().skip(1))
        .take_while(|a| *a != "--")
        .any(|a| a == "--help");
    // A name the server resolves (`-t NAME`) is no failure to parse.
    let unparsed = matches!(
        command::parse(args),
        Err(usage) if !matches!(usage, command::Usage::Not(command::Kind::Pane | command::Kind::Tab, _))
    );
    let asks_help = args.get(1).is_some_and(|a| a == "--help") || (anywhere && unparsed);
    let command = first.filter(|f| !f.starts_with("__") && !["help", "--help", "-h"].contains(f));
    if let (Some(name), true) = (command, asks_help) {
        return match help_of(name) {
            Some(text) => {
                writeln!(std::io::stdout(), "{}", text.trim_end()).map_err(Error::Output)?;
                Ok(0)
            }
            None => usage_error(&format!("unknown command {name:?}"), None, json),
        };
    }
    match first {
        Some(fuxix::pty::LAUNCH) => Ok(fuxix::pty::launched(args.get(1..).unwrap_or_default())),
        // `attach` may be left out before its flags, as usage says.
        None | Some("attach" | "-t" | "--nested" | "--headless" | "--read-only") => {
            let mut workspace = None;
            let (mut nested, mut headless, mut read_only) = (false, false, false);
            let (mut size, mut log) = (None, None);
            let mut rest = args.iter().skip(usize::from(first == Some("attach")));
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "-t" => match rest.next() {
                        Some(name) => workspace = Some(name.clone()),
                        None => return usage_error("attach -t needs a workspace", None, json),
                    },
                    "--nested" => nested = true,
                    "--headless" => headless = true,
                    "--read-only" => read_only = true,
                    "--size" => match rest.next().and_then(|s| headless_size(s)) {
                        Some(cells) => size = Some(cells),
                        None => {
                            let message = "attach --size needs COLSxROWS, each 1 to 500, as 100x30";
                            return usage_error(message, None, json);
                        }
                    },
                    "--log" => match rest.next() {
                        Some(path) => log = Some(std::path::PathBuf::from(path)),
                        None => return usage_error("attach --log needs a file", None, json),
                    },
                    other => {
                        return usage_error(&format!("attach: unexpected {other:?}"), None, json);
                    }
                }
            }
            if !headless && (size.is_some() || log.is_some()) {
                return usage_error("attach --size and --log go with --headless", None, json);
            }
            if headless && read_only {
                return usage_error(
                    "a headless client takes no keys: --read-only is for a terminal",
                    None,
                    json,
                );
            }
            // A headless client takes no terminal, so may attach from a pane.
            if !nested && !headless && std::env::var_os("FUX_PANE").is_some_and(|p| !p.is_empty()) {
                return Err(Error::Nested);
            }
            // Before a server is started for it, which would be left behind.
            if !headless && !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                return Err(Error::Client(client::Error::NotATerminal));
            }
            let socket = socket::socket_path(choice)?;
            if UnixStream::connect(socket.path()).is_err() {
                client::start_server(&socket, &[])?;
            }
            if headless {
                let size = size.unwrap_or((nonzero(24), nonzero(80)));
                client::headless(&socket, workspace, size, log.as_deref())?;
            } else {
                client::attach(&socket, workspace, read_only)?;
            }
            Ok(0)
        }
        Some("server") => {
            let (mut socket_flag, mut background, mut passed) = (None, false, Vec::new());
            let mut config = None;
            let mut rest = args.iter().skip(1);
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    flag @ ("--socket" | "--config") => {
                        let Some(value) = rest.next().cloned() else {
                            return usage_error(
                                &format!("server {flag} needs a value"),
                                None,
                                json,
                            );
                        };
                        if flag == "--socket" {
                            socket_flag = Some(value);
                        } else {
                            passed.extend(["--config".to_owned(), value.clone()]);
                            config = Some(value);
                        }
                    }
                    "--background" => background = true,
                    "--fresh" => passed.push("--fresh".to_owned()),
                    client::SETSID => {
                        fuxix::process::setsid().map_err(Error::Setsid)?;
                    }
                    other => {
                        return usage_error(&format!("server: unexpected {other:?}"), None, json);
                    }
                }
            }
            let choice = match (socket_flag.as_deref(), choice.named()) {
                (Some(_), true) => {
                    return usage_error("name the server once: -L, -S or --socket", None, json);
                }
                (Some(path), false) => socket::Choice::Path(path, "--socket"),
                (None, _) => choice,
            };
            let socket = socket::socket_path(choice)?;
            if background {
                // A config that will not do is said here, where someone
                // reads it, not only in the server's log.
                if let Some(path) = &config {
                    let path = std::path::Path::new(path);
                    let problem = match config::Config::from_file(path) {
                        Ok(Some(_)) => None,
                        Ok(None) => Some(format!("{}: no such file", path.display())),
                        Err(error) => Some(error.to_string()),
                    };
                    if let Some(problem) = problem {
                        let _ = writeln!(
                            std::io::stderr(),
                            "fux: config: {problem}; the server runs on the defaults"
                        );
                    }
                }
                if let client::Probe::Server { .. } = client::probe(&socket)? {
                    let path = socket.path().display();
                    let _ = writeln!(std::io::stderr(), "fux: a server already answers at {path}");
                } else {
                    client::start_server(&socket, &passed)?;
                }
                writeln!(std::io::stdout(), "{}", socket.path().display())
                    .map_err(Error::Output)?;
                return Ok(0);
            }
            let fresh = passed.iter().any(|p| p == "--fresh");
            let config = config
                .map(std::path::PathBuf::from)
                .or_else(config::default_path);
            server::serve(&socket, config, fresh).map_err(Error::Server)?;
            Ok(0)
        }
        Some("status") => {
            if args.iter().skip(1).any(|a| a != "--json") {
                return usage_error("usage: fux [-L NAME | -S PATH] status [--json]", None, json);
            }
            let socket = socket::socket_path(choice)?;
            let probe = client::probe(&socket)?;
            let (text, ok) = status(&socket, &probe, json);
            writeln!(std::io::stdout(), "{text}").map_err(Error::Output)?;
            Ok(if ok { 0 } else { 1 })
        }
        Some("servers") => {
            if args.iter().skip(1).any(|a| a != "--json") {
                return usage_error("usage: fux servers [--json]", None, json);
            }
            let text = servers(json)?;
            write!(std::io::stdout(), "{text}").map_err(Error::Output)?;
            Ok(0)
        }
        Some("check-config") => {
            let (path, named) = match args.get(1..).unwrap_or_default() {
                [] => (config::default_path(), false),
                [path] => (Some(std::path::PathBuf::from(path)), true),
                _ => return usage_error("usage: fux check-config [FILE]", None, json),
            };
            let Some(path) = path else {
                let _ = writeln!(
                    std::io::stderr(),
                    "fux: no config file: neither XDG_CONFIG_HOME nor HOME is set"
                );
                return Ok(1);
            };
            check_config(&path, named, json)
        }
        Some("help" | "--help" | "-h") => {
            let text = match args.get(1).map(String::as_str) {
                None => usage(),
                Some("agents") => help::AGENTS.to_owned(),
                Some("json") => help::JSON.to_owned(),
                Some(name) => match help_of(name) {
                    Some(text) => text,
                    None => {
                        let message = format!("no command {name:?}");
                        return usage_error(&message, None, json);
                    }
                },
            };
            writeln!(std::io::stdout(), "{}", text.trim_end()).map_err(Error::Output)?;
            Ok(0)
        }
        Some("--version" | "-V" | "version") => {
            let version = env!("CARGO_PKG_VERSION");
            writeln!(std::io::stdout(), "fux {version}").map_err(Error::Output)?;
            Ok(0)
        }
        Some(_) => {
            let args = &pasted(args)?;
            // Parsed here as well, so a usage error needs no server; a
            // tab, or its pane, named by the tab's name only the server can
            // tell.
            match command::parse(args) {
                Err(command::Usage::Not(command::Kind::Pane | command::Kind::Tab, _)) | Ok(_) => {}
                Err(usage) => {
                    return usage_error(&usage.to_string(), args.first().map(String::as_str), json);
                }
            }
            let socket = socket::socket_path(choice)?;
            match client::command(&socket, args) {
                // A server of another fux, which only a kill reaches.
                Err(client::Error::Mismatch(_))
                    if matches!(
                        command::parse(args),
                        Ok(command::Command::KillServer { force: true })
                    ) =>
                {
                    client::kill_server(&socket)?;
                    Ok(0)
                }
                result => Ok(result?),
            }
        }
    }
}

/// `fux status`: what answers on `socket`, as text or JSON, and whether it
/// is a server this fux can drive.
fn status(socket: &socket::SocketPath, probe: &client::Probe, json: bool) -> (String, bool) {
    let path = socket.path().display().to_string();
    let ours = env!("CARGO_PKG_VERSION");
    let left = matches!(probe, client::Probe::Left);
    let (running, theirs) = match probe {
        client::Probe::Absent | client::Probe::Left => (false, None),
        client::Probe::Server { protocol, version } => (true, Some((*protocol, version.as_str()))),
    };
    let compatible = theirs.is_some_and(|(protocol, _)| protocol == protocol::PROTOCOL);
    let text = if json {
        let mut fields = vec![
            ("running", json::Json::Bool(running)),
            ("socket", json::Json::str(&path)),
            ("version", json::Json::str(ours)),
            (
                "protocol",
                json::Json::Number(i64::from(protocol::PROTOCOL)),
            ),
        ];
        if left {
            fields.push(("left", json::Json::Bool(true)));
        }
        if let Some((protocol, version)) = theirs {
            fields.push(("server_version", json::Json::str(version)));
            fields.push(("server_protocol", json::Json::Number(i64::from(protocol))));
            fields.push(("compatible", json::Json::Bool(compatible)));
        }
        json::Json::Object(fields).render()
    } else {
        match theirs {
            None if left => {
                format!("no fux server at {path} (its socket is left from one that stopped)")
            }
            None => format!("no fux server at {path}"),
            Some((_, version)) if compatible => format!("fux {version} at {path}"),
            Some((protocol, version)) => format!(
                "fux {version} at {path} speaks protocol {protocol}, not this fux's ({}, \
                 fux {ours}): `fux kill-server --force` stops it",
                protocol::PROTOCOL
            ),
        }
    };
    (text, compatible)
}

/// `fux servers`: every server that answers in fux's directory, a line
/// each, or a JSON array.
fn servers(json: bool) -> Result<String, Error> {
    let directory = socket::directory()?;
    let mut names: Vec<String> = match std::fs::read_dir(&directory) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter_map(|name| Some(name.strip_suffix(".sock")?.to_owned()))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    let mut lines = Vec::new();
    let mut objects = Vec::new();
    for name in names {
        let Ok(socket) = socket::socket_path(socket::Choice::Name(&name)) else {
            continue;
        };
        let Ok(client::Probe::Server { protocol, version }) = client::probe(&socket) else {
            continue;
        };
        let path = socket.path().display().to_string();
        lines.push(format!("{name}  {path}  fux {version}\n"));
        objects.push(json::Json::Object(vec![
            ("name", json::Json::str(name.clone())),
            ("socket", json::Json::str(path.clone())),
            ("version", json::Json::str(version.clone())),
            (
                "compatible",
                json::Json::Bool(protocol == protocol::PROTOCOL),
            ),
        ]));
    }
    Ok(match json {
        true => format!("{}\n", json::Json::Array(objects).render()),
        false => lines.concat(),
    })
}

/// `fux check-config`: every line of the file a server would refuse. A
/// file `named` that is not there is an error; the default one missing
/// leaves the defaults, which is fine.
fn check_config(path: &std::path::Path, named: bool, json: bool) -> Result<u8, Error> {
    let shown = path.display().to_string();
    let errors: Vec<String> = match (config::Config::check(path), path.exists(), named) {
        (_, false, true) => vec![format!("{shown}: no such file")],
        (_, false, false) => Vec::new(),
        (Ok(errors), true, _) => errors.iter().map(ToString::to_string).collect(),
        (Err(error), true, _) => vec![error.to_string()],
    };
    let ok = errors.is_empty();
    if json {
        let text = json::Json::Object(vec![
            ("file", json::Json::str(shown.as_str())),
            ("ok", json::Json::Bool(ok)),
            (
                "errors",
                json::Json::Array(errors.iter().map(|e| json::Json::str(e.as_str())).collect()),
            ),
        ])
        .render();
        writeln!(std::io::stdout(), "{text}").map_err(Error::Output)?;
        return Ok(u8::from(!ok));
    }
    if ok {
        let said = match path.exists() {
            true => format!("{shown}: ok"),
            false => format!("{shown}: no such file; a server runs on the defaults"),
        };
        writeln!(std::io::stdout(), "{said}").map_err(Error::Output)?;
        return Ok(0);
    }
    for error in &errors {
        let _ = writeln!(std::io::stderr(), "fux: {error}");
    }
    Ok(1)
}

#[cfg(test)]
mod tests {
    /// The README names every command.
    #[test]
    fn the_readme_names_every_command() {
        let readme = include_str!("../README.md");
        for (_, specs) in crate::command::COMMANDS {
            for name in specs.iter().flat_map(|spec| spec.names()) {
                let named = readme
                    .split(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                    .any(|word| word == name);
                assert!(named, "{name}: not in the README");
            }
        }
    }
}
