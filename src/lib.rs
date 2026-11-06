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
use fux_wire::{code, command, json::Json, protocol};
use std::io::Write;
use std::process::ExitCode;

#[cfg(test)]
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[cfg(test)]
mod testing {
    /// A code value of `words`, split at spaces, as `code … end` around
    /// them reads them: for tests, as no word turns text into code.
    pub fn code(words: &str) -> fux_wire::callback::Code {
        let words = words.split_whitespace().map(String::from).collect();
        fux_wire::callback::Code { words }
    }
}

// The tests measure their own allocations' peak (`fux_vt_peak`).
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: fux_vt_peak::ThreadPeak = fux_vt_peak::ThreadPeak;

/// The command line of the `fux` binary: no words attaches; `-` runs the
/// fuxth on stdin; `--file FILE ARGS…` runs a file; any other words are a
/// fuxth program.
pub fn main() -> ExitCode {
    if let Some(status) = internal() {
        return status;
    }
    client()
}

/// The server's own processes, when the words name one.
fn internal() -> Option<ExitCode> {
    let args: Option<Vec<String>> = (std::env::args_os().skip(1))
        .map(|a| a.into_string().ok())
        .collect();
    let args = args?;
    let (first, rest) = args.split_first()?;
    match (first.as_str(), rest) {
        (SERVE, rest) => Some(serve(rest)),
        (fux_server::link::PANE, rest) => Some(pane(rest)),
        (fux_server::link::CONN, []) => Some(conn(false)),
        (fux_server::link::CONN, [taken]) if taken == fux_server::link::TAKEN => Some(conn(true)),
        _ => None,
    }
}

/// The client: a fuxth program, or attaching.
fn client() -> ExitCode {
    fuxix::terminal::open_stdio();
    let argv: Vec<_> = std::env::args_os().collect();
    // Words are text: an argument that is not is refused, not mangled.
    let args: Result<Vec<String>, _> = (argv.iter().skip(1))
        .map(|a| a.to_str().map(str::to_owned).ok_or(a))
        .collect();
    let args = args.map_err(|arg| {
        let why = format!("an argument is not text: {}", arg.display());
        script::Failed::new(code::Code::Usage, why)
    });
    let ran = match args {
        // It tells its own failure.
        Ok(args) => script::main(&args, Some(argv), &mut std::io::stdout()),
        Err(failed) => {
            // Nowhere is left to say that stderr failed.
            let _ = writeln!(std::io::stderr(), "fux: {failed}");
            Err(failed)
        }
    };
    let status = ran.unwrap_or_else(|failed| failed.code.status().get());
    ExitCode::from(status)
}

/// How a client starts a server, in the background: `fux __serve SOCKET
/// ENTRIES`, its journal keeping ENTRIES events (`journal-entries`).
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
    let [path, entries] = args else {
        return fail("it takes a socket and the events its journal keeps".to_owned());
    };
    let socket = match socket::checked(path, "socket") {
        Ok(socket) => socket,
        Err(error) => return fail(error.to_string()),
    };
    let journal = entries.parse().ok().and_then(command::JournalEntries::new);
    let Some(journal) = journal else {
        return fail(format!("{entries:?}: a journal keeps 1 event or more"));
    };
    match server::serve(&socket, journal) {
        Ok(()) => ExitCode::SUCCESS,
        // Said to the client that started it, which reads its stdout.
        Err(error @ server::Error::Refused(_)) => {
            let _ = writeln!(std::io::stdout(), "fux server: {error}");
            fail(error.to_string())
        }
        Err(error) => fail(error.to_string()),
    }
}

/// A pane's own process, started by its server (`link::Link::start`), its
/// socket listening on stdin (on Windows, the socket's path in `args`,
/// which names the pipe it makes).
fn pane(args: &[String]) -> ExitCode {
    let served = socket::pane_listener(args)
        .and_then(|(listener, socket)| fux_server::host::serve(listener, socket));
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "fux pane: {error}");
            ExitCode::from(1)
        }
    }
}

/// A client's connection's own process, started by its server: its link
/// on stdin, and on stdout the server's socket, where it takes its client
/// (`server::Conn::spare`), or, `taken`, a client the server took itself.
fn conn(taken: bool) -> ExitCode {
    let served = match taken {
        true => socket::taken_stdio()
            .map_err(|e| format!("its sockets: {e}"))
            .and_then(|(client, server)| fux_server::conn::taken(client, server)),
        false => socket::waiting_stdio(),
    };
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "fux connection: {error}");
            ExitCode::from(1)
        }
    }
}

/// `fux status`: the server that answers on `socket`, its process where
/// the system says it, and whether it is one this fux can drive; with
/// none, why not.
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
        client::Probe::Server {
            protocol,
            version,
            pid,
        } => {
            let pid = pid.map(|pid| ("server_pid", Json::int(pid.as_raw())));
            [
                Some(("server_release", Json::str(version.as_str()))),
                Some(("server_protocol", Json::int(*protocol))),
                pid,
                Some(("compatible", Json::Bool(*protocol == protocol::PROTOCOL))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
        }
    };
    let mut fields = vec![
        ("socket", Json::str(&path)),
        ("release", Json::str(ours)),
        ("protocol", Json::int(protocol::PROTOCOL)),
    ];
    fields.extend(theirs);
    Ok(Json::object(fields))
}
