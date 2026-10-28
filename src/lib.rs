//! fux: a terminal multiplexer. One server holds workspaces, tabs and split
//! panes; `fux` shows them in a terminal; `fux WORDS…`, a fuxth program,
//! drives them.
pub use fux_vt::bytes;
pub use fux_vt::keys;
pub use fux_vt::keys::{decode, encode};
/// Writes to the server's log, its stderr: `log!("…", args)` writes the
/// line after `fux server: `. A log that cannot be written, a full disk,
/// say, loses the line and stops nothing.
macro_rules! log {
    ($($arg:tt)*) => {{
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("fux server: {}\n", format_args!($($arg)*)),
        );
    }};
}

pub mod act;
pub mod answer;
pub mod capture;
pub mod client;
pub mod client_state;
pub mod code;
pub mod command;
pub mod config;
pub mod copy;
pub mod events;
mod help;
pub mod id;
mod input;
pub mod inspect;
pub mod json;
pub mod keymap;
pub mod layout;
pub mod outer;
mod overlay;
pub mod pane;
pub mod process;
pub mod protocol;
pub mod render;
pub mod screens;
pub mod script;
pub mod server;
pub mod session;
pub mod socket;
pub mod view;
pub mod wait;
pub mod wire;
pub mod words;
pub mod workspace;

use fuxix::process::Signal;
use json::Json;
use std::io::{PipeReader, Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// `wait` after `from`. A time too far off for an `Instant` is taken as
/// `from`, so that a deadline fires at once rather than never.
pub(crate) fn after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
}

/// The signals that stop a server and detach a client.
pub(crate) const STOPS: [Signal; 3] = [Signal::Term, Signal::Int, Signal::Hup];

/// Reads whatever a pipe holds.
pub(crate) fn drain(mut pipe: &PipeReader) {
    let mut buffer = [0u8; 256];
    while matches!(pipe.read(&mut buffer), Ok(n) if n > 0) {}
}

/// The command line of the `fux` binary: no words attaches; `-` runs the
/// fuxth on stdin; `--file FILE ARGS…` runs a file; any other words are a
/// fuxth program.
pub fn main() -> ExitCode {
    // Words are text: an argument that is not is refused, not mangled.
    let args: Result<Vec<String>, _> = std::env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect();
    let args = args.map_err(|arg| {
        let why = format!("an argument is not text: {}", arg.display());
        script::Failed::new(code::Code::Usage, why)
    });
    if let Ok(args) = &args
        && let Some((SERVE, rest)) = args.split_first().map(|(a, rest)| (a.as_str(), rest))
    {
        return serve(rest);
    }
    let status = match args.and_then(|args| script::main(&args, &mut std::io::stdout())) {
        Ok(status) => status,
        Err(failed) => {
            // Nowhere is left to say that stderr failed.
            let _ = writeln!(std::io::stderr(), "fux: {failed}");
            failed.code.status().get()
        }
    };
    ExitCode::from(status)
}

/// How a client starts a server, in the background: `fux __serve SOCKET`.
/// A client types it, the
/// server leading a session of its own (`client::start_server`); no help
/// lists it. A person can type it too: that server stays in the
/// terminal's session, and the terminal's hangup or Ctrl-C stops it.
pub const SERVE: &str = "__serve";

/// A server, started by a client (`client::start_server`).
fn serve(args: &[String]) -> ExitCode {
    let fail = |message: String| {
        let _ = writeln!(std::io::stderr(), "fux server: {message}");
        ExitCode::from(1)
    };
    let Some(path) = args.first() else {
        return fail("no socket".to_owned());
    };
    let socket = match socket::checked(path, "socket") {
        Ok(socket) => socket,
        Err(error) => return fail(error.to_string()),
    };
    match server::serve(&socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error.to_string()),
    }
}

/// `fux status`: the server that answers on `socket`, and whether it is
/// one this fux can drive; with none, why not.
pub(crate) fn status(socket: &socket::SocketPath, probe: &client::Probe) -> Result<Json, String> {
    let path = socket.path().display().to_string();
    let ours = env!("CARGO_PKG_VERSION");
    let theirs = match probe {
        client::Probe::Absent => return Err(format!("no fux server at {path}")),
        client::Probe::Left => {
            return Err(format!(
                "no fux server at {path} (its socket is left from one that stopped)"
            ));
        }
        client::Probe::Server { protocol, version } => vec![
            ("server_version", Json::str(version.as_str())),
            ("server_protocol", Json::int(*protocol)),
            ("compatible", Json::Bool(*protocol == protocol::PROTOCOL)),
        ],
    };
    let mut fields = vec![
        ("socket", Json::str(&path)),
        ("version", Json::str(ours)),
        ("protocol", Json::int(protocol::PROTOCOL)),
    ];
    fields.extend(theirs);
    Ok(Json::object(fields))
}
