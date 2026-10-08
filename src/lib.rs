//! fux: a terminal multiplexer. One server holds workspaces, tabs and split
//! panes; `fux attach` shows them in a terminal; every other `fux` command
//! changes them.
pub use fux_vt::bytes;
pub use fux_vt::keys;
pub use fux_vt::keys::{decode, encode};
pub mod client;
pub mod command;
pub mod config;
pub mod copy;
pub mod input;
pub mod json;
pub mod layout;
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
pub mod words;

use std::io::Read;
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
pub(crate) fn drain(pipe: &mut UnixStream) {
    let mut buffer = [0u8; 256];
    while matches!(pipe.read(&mut buffer), Ok(n) if n > 0) {}
}

const USAGE: &str = "\
usage: fux [attach] [-t WORKSPACE] [--nested]
       fux server [--socket PATH] [--config FILE]
       fux kill-server
       fux COMMAND [ARGS...]

Commands:
  ls [--json]                          workspaces, tabs, panes and clients
  new-workspace [-n NAME] [-- CMD...]  a workspace with a shell (CMD typed into it)
  new-tab [-t WS] [-n NAME] [-- CMD...]
  split -h|-v [-t %N] [-- CMD...]      -h side by side, -v stacked
  kill-pane|kill-tab|kill-workspace [-t TARGET]
  rename -t TARGET NAME                TARGET is %N, @N, +N or a workspace name
  move-pane [-t %N] --to @N|+N|new-tab|new-workspace   or -L/-R/-U/-D
  swap-pane [-t %N] %M                 or -L/-R/-U/-D
  resize-pane [-t %N] -L|-R|-U|-D [CELLS]
  reorder pane|tab|workspace [-t TARGET] --next|--previous
  terminate [-t %N]                    SIGTERM to what runs in the pane's foreground
  send-keys [-t %N] [-l] KEYS...
  send-prefix [-t %N]                  the prefix key, to the pane
  capture-pane [-t %N] [-S -LINES] [--json]
  capture-client [-c CLIENT] [--json]  what a client's terminal shows
  set OPTION VALUE | unbind [-n] KEY... | unbind-all | reload
  bind [-g GROUP] [-r] KEY... COMMAND...   keys after the prefix; V is Shift-v
  bind -n [-g GROUP] KEY COMMAND...    a key without the prefix
  list-buffers | show-buffer [-b N] | paste-buffer [-b N] [-t %N]
  list-keys | detach [-c CLIENT]
On a client's screen (from a key, the command prompt, or with -c CLIENT):
  command-column, command-prompt, copy-mode, zoom, choose-tab, choose-workspace,
  choose-pane, menu pane|tab|workspace, rename-prompt, confirm-close,
  select-pane, select-tab, select-workspace

The socket is FUX_SOCKET, else $XDG_RUNTIME_DIR/fux/server.sock, else
$TMPDIR/fux/server.sock. Inside a pane, FUX_PANE names it, so commands there
target it without -t.";

/// The command line of the `fux` binary.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(status) => ExitCode::from(status),
        Err(error) => {
            eprintln!("fux: {error}");
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

fn usage_error(message: &str) -> Result<u8, Error> {
    eprintln!("fux: {message}\n{USAGE}");
    Ok(2)
}

fn run(args: &[String]) -> Result<u8, Error> {
    let first = args.first().map(String::as_str);
    match first {
        Some(process::LAUNCH) => Ok(process::launched(args.get(1..).unwrap_or_default())),
        None | Some("attach") => {
            let mut workspace = None;
            let mut nested = false;
            let mut rest = args.iter().skip(usize::from(first.is_some()));
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "-t" => match rest.next() {
                        Some(name) => workspace = Some(name.clone()),
                        None => return usage_error("attach -t needs a workspace"),
                    },
                    "--nested" => nested = true,
                    other => return usage_error(&format!("attach: unexpected {other:?}")),
                }
            }
            if !nested && std::env::var_os("FUX_PANE").is_some_and(|p| !p.is_empty()) {
                return Err(Error::Nested);
            }
            let socket = socket::socket_path(None)?;
            if UnixStream::connect(socket.path()).is_err() {
                client::start_server(&socket)?;
            }
            client::attach(&socket, workspace)?;
            Ok(0)
        }
        Some("server") => {
            let (mut socket_flag, mut config) = (None, None);
            let mut rest = args.iter().skip(1);
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--socket" => socket_flag = rest.next().cloned(),
                    "--config" => config = rest.next().cloned(),
                    client::SETSID => {
                        fuxix::process::setsid().map_err(Error::Setsid)?;
                    }
                    other => return usage_error(&format!("server: unexpected {other:?}")),
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
            let socket = socket::socket_path(None)?;
            client::kill_server(&socket)?;
            Ok(0)
        }
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            Ok(0)
        }
        Some("--version" | "-V" | "version") => {
            println!("fux {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Some(_) => {
            // Parsed here as well, so a usage error needs no server.
            if let Err(usage) = command::parse(args) {
                return usage_error(&usage.to_string());
            }
            let socket = socket::socket_path(None)?;
            Ok(client::command(&socket, args)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command the usage and the README name is one the parser knows,
    /// and every command here is named in both: a command added to the
    /// parser is added here, which holds the two to it.
    #[test]
    fn the_usage_and_the_readme_name_the_commands_the_parser_knows() {
        const COMMANDS: &[&str] = &[
            "ls",
            "kill-server",
            "new-workspace",
            "new-tab",
            "split",
            "kill-pane",
            "kill-tab",
            "kill-workspace",
            "rename",
            "move-pane",
            "swap-pane",
            "resize-pane",
            "reorder",
            "terminate",
            "send-keys",
            "send-prefix",
            "capture-pane",
            "capture-client",
            "set",
            "bind",
            "unbind",
            "unbind-all",
            "reload",
            "list-buffers",
            "show-buffer",
            "paste-buffer",
            "list-keys",
            "detach",
            "command-column",
            "command-prompt",
            "copy-mode",
            "zoom",
            "choose-tab",
            "choose-workspace",
            "choose-pane",
            "menu",
            "rename-prompt",
            "confirm-close",
            "select-pane",
            "select-tab",
            "select-workspace",
        ];
        let readme = include_str!("../README.md");
        for name in COMMANDS {
            let parsed = crate::command::parse(&[(*name).to_owned()]);
            assert!(
                !matches!(parsed, Err(crate::command::Usage::UnknownCommand(_))),
                "{name}: the parser does not know it"
            );
            let named = |text: &str| {
                text.split(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                    .any(|word| word == *name)
            };
            assert!(named(USAGE), "{name}: not in the usage");
            assert!(named(readme), "{name}: not in the README");
        }
    }
}
