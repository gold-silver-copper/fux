//! fux: a terminal multiplexer. One server holds workspaces, tabs and split
//! panes; `fux attach` shows them in a terminal; every other `fux` command
//! changes them.
pub mod bytes;
pub mod client;
pub mod command;
pub mod config;
pub mod copy;
pub mod decode;
pub mod encode;
pub mod input;
pub mod json;
pub mod keys;
pub mod layout;
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

use std::process::ExitCode;
use std::time::{Duration, Instant};

/// `wait` after `from`. A time too far off for an `Instant` is taken as
/// `from`, so that a deadline fires at once rather than never.
pub(crate) fn after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
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
  capture-pane [-t %N] [-S -LINES] [--json]
  set OPTION VALUE | bind [-g GROUP] KEY COMMAND... | unbind KEY | unbind-all | reload
  list-buffers | show-buffer [-b N] | paste-buffer [-b N] [-t %N]
  list-keys | detach [-c CLIENT]
On a client's screen (from a key, the : prompt, or with -c CLIENT):
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

fn usage_error(message: &str) -> Result<u8, String> {
    eprintln!("fux: {message}\n{USAGE}");
    Ok(2)
}

fn run(args: &[String]) -> Result<u8, String> {
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
                return Err(
                    "this is already a fux pane; attaching here would show fux inside itself (--nested does it anyway)"
                        .into(),
                );
            }
            let socket = socket::socket_path(None)?;
            if !socket::exists(&socket) || std::os::unix::net::UnixStream::connect(&socket).is_err()
            {
                client::start_server(&socket)?;
            }
            client::attach(&socket, workspace).map(|()| 0)
        }
        Some("server") => {
            let (mut socket_flag, mut config) = (None, None);
            let mut rest = args.iter().skip(1);
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--socket" => socket_flag = rest.next().cloned(),
                    "--config" => config = rest.next().cloned(),
                    client::SETSID => {
                        rustix::process::setsid().map_err(|e| format!("setsid: {e}"))?;
                    }
                    other => return usage_error(&format!("server: unexpected {other:?}")),
                }
            }
            let socket = socket::socket_path(socket_flag.as_deref())?;
            let config = config
                .map(std::path::PathBuf::from)
                .or_else(config::default_path);
            server::serve(&socket, config).map(|()| 0)
        }
        Some("kill-server") => {
            let socket = socket::socket_path(None)?;
            client::kill_server(&socket).map(|()| 0)
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
            if let Err(command::Usage(message)) = command::parse(args) {
                return usage_error(&message);
            }
            let socket = socket::socket_path(None)?;
            client::command(&socket, args)
        }
    }
}
