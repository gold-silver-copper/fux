//! fux: a terminal multiplexer. One server holds workspaces, tabs and split
//! panes; `fux` shows them in a terminal; `fux WORDS…`, a fuxth program,
//! drives them.
pub use fux_vt::bytes;
pub use fux_vt::keys;
pub use fux_vt::keys::{decode, encode};
pub mod act;
pub mod capture;
pub mod classify;
pub mod client;
pub mod code;
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
pub mod log;
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

use json::Json;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use std::io::PipeReader;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// `wait` after `from`. A time too far off for an `Instant` is taken as
/// `from`, so that a deadline fires at once rather than never.
pub(crate) fn after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
}

/// The signals that stop a server and detach a client.
pub(crate) const STOPS: [std::ffi::c_int; 3] = [SIGTERM, SIGINT, SIGHUP];

/// A pipe that becomes readable when any of its signals arrives: each
/// writes a byte to the other end, of which its handler holds a copy.
/// Dropped, it unregisters them, which closes those copies. A pipe, not a
/// socket pair: a sandbox may refuse send(2), which a signal writes a
/// socket with.
pub(crate) struct SignalPipe {
    pipe: PipeReader,
    registered: Vec<signal_hook::SigId>,
}

impl SignalPipe {
    pub(crate) fn new(signals: &[std::ffi::c_int]) -> std::io::Result<SignalPipe> {
        let (pipe, write) = std::io::pipe()?;
        fuxix::io::set_nonblocking(&pipe, true)?;
        // A signal never waits on a full pipe: one byte there is enough.
        fuxix::io::set_nonblocking(&write, true)?;
        // Each registered as it is made, so a failure part-way through
        // unregisters those before it.
        let mut made = SignalPipe {
            pipe,
            registered: Vec::new(),
        };
        for &signal in signals {
            let id = signal_hook::low_level::pipe::register(signal, write.try_clone()?)?;
            made.registered.push(id);
        }
        Ok(made)
    }

    /// The pipe, to wait on and drain.
    pub(crate) fn pipe(&self) -> &PipeReader {
        &self.pipe
    }
}

impl Drop for SignalPipe {
    fn drop(&mut self) {
        for &id in &self.registered {
            signal_hook::low_level::unregister(id);
        }
    }
}

/// Reads whatever a pipe holds.
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
            failed.status.get()
        }
    };
    let _ = stdout.flush();
    ExitCode::from(status)
}

/// How a client starts a server, in the background: `fux __serve SOCKET`,
/// with `fresh` to forget what the last one held. A client types it; no
/// help lists it, though a person can type it too.
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
    let socket = match socket::checked(path, "socket") {
        Ok(socket) => socket,
        Err(error) => return fail(error.to_string()),
    };
    match server::serve(&socket, fresh) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error.to_string()),
    }
}

/// `fux status`: what answers on `socket`, as text and as JSON, and
/// whether it is a server this fux can drive.
pub(crate) fn status(socket: &socket::SocketPath, probe: &client::Probe) -> (String, String) {
    let path = socket.path().display().to_string();
    let ours = env!("CARGO_PKG_VERSION");
    let (running, theirs, text) = match probe {
        client::Probe::Absent => (false, vec![], format!("no fux server at {path}")),
        client::Probe::Left => {
            let text =
                format!("no fux server at {path} (its socket is left from one that stopped)");
            (false, vec![("left", Json::Bool(true))], text)
        }
        client::Probe::Server { protocol, version } => {
            let compatible = *protocol == protocol::PROTOCOL;
            let mut text = format!("fux {version} at {path}");
            if !compatible {
                let ours = format!("{}, fux {ours}", protocol::PROTOCOL);
                text.push_str(&format!(
                    " speaks protocol {protocol}, not this fux's ({ours}): \
                     `fux force-kill-server` stops it"
                ));
            }
            let theirs = vec![
                ("server_version", Json::str(version.as_str())),
                ("server_protocol", Json::int(*protocol)),
                ("compatible", Json::Bool(compatible)),
            ];
            (true, theirs, text)
        }
    };
    let mut fields = vec![
        ("running", Json::Bool(running)),
        ("socket", Json::str(&path)),
        ("version", Json::str(ours)),
        ("protocol", Json::int(protocol::PROTOCOL)),
    ];
    fields.extend(theirs);
    (text, Json::Object(fields).render())
}

/// `fux servers`: every server that answers in fux's directory, a line
/// each, and the same as a JSON array, from one look at each.
pub(crate) fn servers() -> Result<(String, String), socket::Error> {
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
        let Ok(socket) = socket::socket_path(&socket::Choice::Name(name.clone())) else {
            continue;
        };
        let Ok(client::Probe::Server { protocol, version }) = client::probe(&socket) else {
            continue;
        };
        let path = socket.path().display().to_string();
        lines.push(format!("{name}  {path}  fux {version}\n"));
        objects.push(Json::Object(vec![
            ("name", Json::str(name.clone())),
            ("socket", Json::str(path.clone())),
            ("version", Json::str(version.clone())),
            ("compatible", Json::Bool(protocol == protocol::PROTOCOL)),
        ]));
    }
    let json = format!("{}\n", Json::Array(objects).render());
    Ok((lines.concat(), json))
}

#[cfg(test)]
mod tests {
    /// The descriptors this process holds on the pipe whose inode is `ino`.
    #[cfg(target_os = "linux")]
    fn holding(ino: u64) -> usize {
        let pipe = format!("pipe:[{ino}]");
        std::fs::read_dir("/proc/self/fd")
            .into_iter()
            .flatten()
            .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
            .filter(|link| link.as_os_str() == pipe.as_str())
            .count()
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_dropped_signal_pipe_leaves_nothing_registered() -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let signals = [signal_hook::consts::SIGWINCH, signal_hook::consts::SIGUSR2];
        let pipe = super::SignalPipe::new(&signals)?;
        let ino = std::fs::metadata(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&pipe.pipe)
        ))?
        .ino();
        // The read end, and a write end for each signal's handler.
        assert_eq!(holding(ino), 3);
        drop(pipe);
        assert_eq!(holding(ino), 0, "a handler still holds its write end");
        Ok(())
    }
}
