//! fux: a terminal multiplexer. One server holds workspaces, tabs and split
//! panes; `fux` shows them in a terminal; `fux WORDS…`, a fuxth program,
//! drives them.
//!
//! This crate is the client: the terminal it shows, fux's fuxth vocabulary,
//! and the `fux` binary, which also runs the server (`fux __serve`). The
//! server is `fux-server`, and what the two say to each other is
//! `fux-wire`.
pub mod client;
pub mod client_state;
mod help;
pub mod keymap;
pub mod screens;
pub mod script;

use fux_server::{server, socket};
use fux_wire::{code, json::Json, protocol};
use std::io::Write;
use std::process::ExitCode;

#[cfg(test)]
type TestResult = Result<(), Box<dyn std::error::Error>>;

// The tests measure their own allocations' peak (`fux_vt_peak`).
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: fux_vt_peak::ThreadPeak = fux_vt_peak::ThreadPeak;

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
            ("server_release", Json::str(version.as_str())),
            ("server_protocol", Json::int(*protocol)),
            ("compatible", Json::Bool(*protocol == protocol::PROTOCOL)),
        ],
    };
    let mut fields = vec![
        ("socket", Json::str(&path)),
        ("release", Json::str(ours)),
        ("protocol", Json::int(protocol::PROTOCOL)),
    ];
    fields.extend(theirs);
    Ok(Json::object(fields))
}
