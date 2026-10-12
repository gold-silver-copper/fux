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

usage: fux [attach] [-t WORKSPACE] [--nested]
       fux attach --headless [--size COLSxROWS] [--log FILE] [-t WORKSPACE]
       fux server [--socket PATH] [--config FILE]
       fux COMMAND [ARGS...]

{}
-c CLIENT names the client whose screen a command acts on; from a key, the
command prompt or a menu, it is the client that ran it.

The socket is FUX_SOCKET, else $XDG_RUNTIME_DIR/fux/server.sock, else
$TMPDIR/fux/server.sock. Inside a pane, FUX_PANE names it, so commands there
target it without -t.",
        command::help()
    )
}

/// A command's help, the client's own (`attach`, `server`) included.
fn help_of(name: &str) -> Option<String> {
    let own = match name {
        "attach" => {
            "usage: fux [attach] [-t WORKSPACE] [--nested]\n       \
             fux attach --headless [--size COLSxROWS] [--log FILE] [-t WORKSPACE]\n\n\
             Shows the server's screen in this terminal, starting a server if none \
             runs. --headless\nattaches a client with no terminal: it prints its id, \
             logs what it is sent to FILE,\nand detaches on SIGTERM; send-keys -c and \
             capture-client act on it.\n\n  fux attach --headless --size 100x30 &"
        }
        "server" => {
            "usage: fux server [--socket PATH] [--config FILE]\n\n\
             Runs a server in the foreground, with no client: what `fux` starts on \
             its own when\nnone runs. A private one for tests: --socket PATH, and \
             FUX_SOCKET=PATH for the commands.\n\n  fux server --socket \"$PWD/s.sock\" &"
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
            // Nowhere is left to say that stderr failed.
            let _ = writeln!(std::io::stderr(), "fux: {error}");
            ExitCode::from(1)
        }
    }
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
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Nested => None,
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
/// if it names one, rather than every command's.
fn usage_error(message: &str, command: Option<&str>) -> Result<u8, Error> {
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
    // The status says it, if stderr cannot.
    let _ = writeln!(std::io::stderr(), "fux: {message}{usage}");
    Ok(2)
}

fn run(args: &[String]) -> Result<u8, Error> {
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
            None => usage_error(&format!("unknown command {name:?}"), None),
        };
    }
    match first {
        Some(fuxix::pty::LAUNCH) => Ok(fuxix::pty::launched(args.get(1..).unwrap_or_default())),
        // `attach` may be left out before its flags, as usage says.
        None | Some("attach" | "-t" | "--nested" | "--headless") => {
            let mut workspace = None;
            let (mut nested, mut headless, mut size, mut log) = (false, false, None, None);
            let mut rest = args.iter().skip(usize::from(first == Some("attach")));
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "-t" => match rest.next() {
                        Some(name) => workspace = Some(name.clone()),
                        None => return usage_error("attach -t needs a workspace", None),
                    },
                    "--nested" => nested = true,
                    "--headless" => headless = true,
                    "--size" => match rest.next().and_then(|s| headless_size(s)) {
                        Some(cells) => size = Some(cells),
                        None => {
                            return usage_error(
                                "attach --size needs COLSxROWS, each 1 to 500, as 100x30",
                                None,
                            );
                        }
                    },
                    "--log" => match rest.next() {
                        Some(path) => log = Some(std::path::PathBuf::from(path)),
                        None => return usage_error("attach --log needs a file", None),
                    },
                    other => return usage_error(&format!("attach: unexpected {other:?}"), None),
                }
            }
            if !headless && (size.is_some() || log.is_some()) {
                return usage_error("attach --size and --log go with --headless", None);
            }
            // A headless client takes no terminal, so may attach from a pane.
            if !nested && !headless && std::env::var_os("FUX_PANE").is_some_and(|p| !p.is_empty()) {
                return Err(Error::Nested);
            }
            let socket = socket::socket_path(None)?;
            if UnixStream::connect(socket.path()).is_err() {
                client::start_server(&socket)?;
            }
            if headless {
                let size = size.unwrap_or((nonzero(24), nonzero(80)));
                client::headless(&socket, workspace, size, log.as_deref())?;
            } else {
                client::attach(&socket, workspace)?;
            }
            Ok(0)
        }
        Some("server") => {
            let (mut socket_flag, mut config) = (None, None);
            let mut rest = args.iter().skip(1);
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    flag @ ("--socket" | "--config") => {
                        let Some(value) = rest.next().cloned() else {
                            return usage_error(&format!("server {flag} needs a value"), None);
                        };
                        if flag == "--socket" {
                            socket_flag = Some(value);
                        } else {
                            config = Some(value);
                        }
                    }
                    client::SETSID => {
                        fuxix::process::setsid().map_err(Error::Setsid)?;
                    }
                    other => return usage_error(&format!("server: unexpected {other:?}"), None),
                }
            }
            let socket = socket::socket_path(socket_flag.as_deref())?;
            let config = config
                .map(std::path::PathBuf::from)
                .or_else(config::default_path);
            server::serve(&socket, config).map_err(Error::Server)?;
            Ok(0)
        }
        Some("kill-server") => {
            if args.len() > 1 {
                return usage_error("kill-server takes no arguments", Some("kill-server"));
            }
            let socket = socket::socket_path(None)?;
            client::kill_server(&socket)?;
            Ok(0)
        }
        Some("help" | "--help" | "-h") => {
            let text = match args.get(1).map(String::as_str) {
                None => usage(),
                Some("agents") => help::AGENTS.to_owned(),
                Some(name) => match help_of(name) {
                    Some(text) => text,
                    None => {
                        let message = format!("no command {name:?}");
                        return usage_error(&message, None);
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
            // Parsed here as well, so a usage error needs no server; a
            // tab, or its pane, named by the tab's name only the server can
            // tell.
            match command::parse(args) {
                Err(command::Usage::Not(command::Kind::Pane | command::Kind::Tab, _)) | Ok(_) => {}
                Err(usage) => {
                    return usage_error(&usage.to_string(), args.first().map(String::as_str));
                }
            }
            let socket = socket::socket_path(None)?;
            Ok(client::command(&socket, args)?)
        }
    }
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
