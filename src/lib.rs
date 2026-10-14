//! fux: a terminal multiplexer. One server holds workspaces, tabs and split
//! panes; `fux` shows them in a terminal; `fux WORDS…`, a fuxth program,
//! drives them.
pub use fux_vt::bytes;
pub use fux_vt::keys;
pub use fux_vt::keys::{decode, encode};
pub mod act;
pub mod capture;
pub mod client;
pub mod command;
pub mod config;
pub mod copy;
pub mod door;
pub mod echo;
pub mod events;
pub mod help;
pub mod id;
pub mod input;
pub mod json;
pub mod keymap;
pub mod layout;
pub mod marks;
pub mod outer;
pub mod overlay;
pub mod pane;
pub mod process;
pub mod protocol;
pub mod ready;
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

use std::io::PipeReader;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// `wait` after `from`. A time too far off for an `Instant` is taken as
/// `from`, so that a deadline fires at once rather than never.
pub(crate) fn after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
}

/// A pipe that becomes readable when any of `signals` arrives: each writes
/// a byte to the other end, of which it holds a copy. A pipe, not a socket
/// pair: a sandbox may refuse send(2), which a signal writes a socket with.
pub(crate) fn signal_pipe(signals: &[std::ffi::c_int]) -> std::io::Result<PipeReader> {
    let (pipe, write) = std::io::pipe()?;
    fuxix::io::set_nonblocking(&pipe, true)?;
    // A signal never waits on a full pipe: one byte there is enough.
    fuxix::io::set_nonblocking(&write, true)?;
    for &signal in signals {
        signal_hook::low_level::pipe::register(signal, write.try_clone()?)?;
    }
    Ok(pipe)
}

/// Reads whatever a signal pipe holds.
pub(crate) fn drain(mut pipe: &PipeReader) {
    let mut buffer = [0u8; 256];
    while matches!(pipe.read(&mut buffer), Ok(n) if n > 0) {}
}

/// The command line of the `fux` binary: no words attaches; `-` runs the
/// fuxth on stdin; one word naming a file (with a `/`, or ending `.fux`)
/// runs it; any other words are a fuxth program.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some(fuxix::pty::LAUNCH) => {
            return ExitCode::from(fuxix::pty::launched(args.get(1..).unwrap_or_default()));
        }
        Some(SERVE) => return serve(args.get(1..).unwrap_or_default()),
        Some(_) | None => {}
    }
    let mut stdout = std::io::stdout();
    let status = match script::main(&args, &mut stdout) {
        Ok(status) => status,
        Err(failed) => {
            // Nowhere is left to say that stderr failed.
            let _ = writeln!(std::io::stderr(), "fux: {failed}");
            failed.status
        }
    };
    let _ = stdout.flush();
    ExitCode::from(status)
}

/// How a client starts a server, in the background: `fux __serve SOCKET`,
/// with `fresh` to forget what the last one held. No person types it.
pub const SERVE: &str = "__serve";

/// A server, started by a client: it leaves the client's session first, so
/// that it outlives the terminal the client ran in.
fn serve(args: &[String]) -> ExitCode {
    let fail = |message: String| {
        let _ = writeln!(std::io::stderr(), "fux server: {message}");
        ExitCode::from(1)
    };
    let Some(path) = args.first() else {
        return fail("no socket".to_owned());
    };
    let fresh = args.get(1).is_some_and(|a| a == "fresh");
    // Fails only for a session leader already, which is as good.
    let _ = fuxix::process::setsid();
    let socket = match socket::socket_path(socket::Choice::Path(path, "socket")) {
        Ok(socket) => socket,
        Err(error) => return fail(error.to_string()),
    };
    match server::serve(&socket, fresh) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error.to_string()),
    }
}

/// `fux status`: what answers on `socket`, as text or JSON, and whether it
/// is a server this fux can drive.
pub(crate) fn status(
    socket: &socket::SocketPath,
    probe: &client::Probe,
    json: bool,
) -> (String, bool) {
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
                 fux {ours}): `fux force-kill-server` stops it",
                protocol::PROTOCOL
            ),
        }
    };
    (text, compatible)
}

/// `fux servers`: every server that answers in fux's directory, a line
/// each, or a JSON array.
pub(crate) fn servers(json: bool) -> Result<String, socket::Error> {
    let directory = socket::directory()?;
    let mut names: Vec<String> = match std::fs::read_dir(&directory) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            // A server that serves its door alone has no socket.
            .filter_map(|name| {
                let name = name.strip_suffix(".door").unwrap_or(&name);
                Some(name.strip_suffix(".sock")?.to_owned())
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names.dedup();
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
