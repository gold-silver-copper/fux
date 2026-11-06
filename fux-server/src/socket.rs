//! Where the server is found, and the rules that keep it private. What
//! is the same on every OS is here: the path a server is known by
//! (`FUX_SOCKET`, else `server.sock` in fux's directory), its checks, and
//! who may connect. How an OS finds a server at that path, keeps it
//! private, and links fux's own processes is in `socket/unix.rs` (a Unix
//! domain socket at the path) and `socket/windows.rs` (a named pipe the
//! path names).
use std::io;
use std::path::{Path, PathBuf};

#[cfg_attr(unix, path = "socket/unix.rs")]
#[cfg_attr(windows, path = "socket/windows.rs")]
mod endpoint;
pub use endpoint::*;

/// Why a socket path is refused, or its directory, or the socket itself.
#[derive(Debug)]
pub enum Error {
    /// No socket at the path: no server runs there.
    NoServer(PathBuf),
    /// A call about `path` that failed: what it was doing, as the message
    /// begins ("locking "), and why.
    Io {
        doing: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// Anything else refused: why.
    Refused(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoServer(path) => {
                write!(
                    f,
                    "no fux server is running at {}; {}",
                    path.display(),
                    start_hint(path)
                )
            }
            Error::Io {
                doing,
                path,
                source,
            } => write!(f, "{doing}{}: {source}", path.display()),
            Error::Refused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

/// An I/O error about `path`.
pub(crate) fn io<'a>(path: &'a Path) -> impl FnOnce(io::Error) -> Error + 'a {
    failed("", path)
}

/// An error about `path`, while `doing` what the message begins with.
pub(crate) fn failed<'a, E: Into<io::Error>>(
    doing: &'static str,
    path: &'a Path,
) -> impl FnOnce(E) -> Error + 'a {
    move |source| Error::Io {
        doing,
        path: path.to_owned(),
        source: source.into(),
    }
}

/// A socket's path, checked: absolute, short enough for `sockaddr_un`, and
/// naming a file in a directory below `/`, whose privacy is checked as it
/// is used (`private_directory`).
#[derive(Clone, Debug)]
pub struct SocketPath {
    path: PathBuf,
    directory: PathBuf,
}

impl SocketPath {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The directory holding the socket, and its log.
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

/// Where a run on the socket at `socket` keeps what outlives its server
/// process (D-124): `SOCKET.panes`, private as the socket's directory is,
/// holding each pane's own socket (`N`), the run's record (`run`) and the
/// fux it runs (`exe`). fux's own, changing with it; no interface.
pub fn panes(socket: &Path) -> PathBuf {
    let mut panes = socket.as_os_str().to_owned();
    panes.push(".panes");
    PathBuf::from(panes)
}

/// The socket a command uses: `socket` if given, else `FUX_SOCKET`, else
/// `server.sock` in fux's directory.
pub fn socket_path(socket: Option<&Path>) -> Result<SocketPath, Error> {
    if let Some(path) = socket {
        return checked(path, "socket");
    }
    match std::env::var_os("FUX_SOCKET") {
        Some(value) => {
            let value = value.into_string().map_err(|_| not_utf8("FUX_SOCKET"))?;
            checked(&value, "FUX_SOCKET")
        }
        None => in_directory("server.sock"),
    }
}

/// What starts the server at `path`: `fux start` the default's (or
/// `FUX_SOCKET`'s), with `FUX_SOCKET` set to it another's.
pub fn start_hint(path: &Path) -> String {
    let default = socket_path(None).is_ok_and(|s| s.path == path);
    if default {
        return "`fux` in a terminal starts one, or `fux start` one with no terminal".into();
    }
    // Quoted for a shell, as the hint is a command to run.
    let path = fux_wire::words::quote(&path.to_string_lossy());
    format!("`FUX_SOCKET={path} fux start` starts one")
}

/// fux's directory: under the first of `BASES` that is set (Unix:
/// `XDG_RUNTIME_DIR`, else `TMPDIR`; Windows: `LOCALAPPDATA`).
pub fn directory() -> Result<PathBuf, Error> {
    for base in BASES {
        if let Some(value) = std::env::var_os(base) {
            let value = value.into_string().map_err(|_| not_utf8(base))?;
            return base_directory(base, &value);
        }
    }
    Err(Error::Refused(NO_BASE.to_owned()))
}

pub(crate) fn not_utf8(variable: &str) -> Error {
    Error::Refused(format!("{variable} is not valid UTF-8"))
}

/// The socket `name` in fux's directory.
fn in_directory(name: &str) -> Result<SocketPath, Error> {
    let path = directory()?.join(name);
    checked(path, "the socket's directory")
}

pub(crate) fn base_directory(base: &'static str, value: &str) -> Result<PathBuf, Error> {
    if value.is_empty() {
        return Err(Error::Refused(format!(
            "{base} is set but empty; set it to a directory or set FUX_SOCKET"
        )));
    }
    if !Path::new(value).is_absolute() {
        return Err(Error::Refused(format!(
            "{base} is {value:?}, not an absolute path; fix it or set FUX_SOCKET"
        )));
    }
    Ok(Path::new(value).join(folder()?))
}

pub fn checked(value: impl AsRef<Path>, from: &'static str) -> Result<SocketPath, Error> {
    let refused = |why: String| Err(Error::Refused(why));
    let path = value.as_ref().to_owned();
    if path.as_os_str().is_empty() {
        return refused(format!(
            "{from} is empty; it must be an absolute socket path"
        ));
    }
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return refused(format!("{from} contains a NUL byte"));
    }
    if !path.is_absolute() {
        return refused(format!("{from} is {path:?}; it must be an absolute path"));
    }
    let directory = (path.file_name())
        .and(path.parent())
        .filter(|directory| directory.parent().is_some())
        .map(Path::to_owned);
    let Some(directory) = directory else {
        return refused(format!(
            "{from} is {path:?}; it must name a socket file in a directory below /"
        ));
    };
    check_length(&path, from)?;
    Ok(SocketPath { path, directory })
}

/// Why a client whose peer is `peer` (`fuxix::socket::peer`) is refused,
/// if it is: it is not the server's own user's, or its user can't be told.
pub fn stranger(peer: fuxix::Result<fuxix::socket::Peer>) -> Option<String> {
    match (peer.map(|peer| peer.user), fuxix::socket::User::current()) {
        (Ok(user), Ok(own)) if user == own => None,
        (Ok(user), Ok(own)) => Some(format!(
            "refused a connection from {user}; only {own} may use this server"
        )),
        (Ok(_), Err(error)) => Some(format!(
            "refused a connection, as this server's own user could not be read: {error}"
        )),
        (Err(error), _) => Some(format!(
            "refused a connection whose peer could not be identified: {error}"
        )),
    }
}
