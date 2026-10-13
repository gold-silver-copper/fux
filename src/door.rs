//! A door: the way into a server for a client in a sandbox that forbids
//! Unix sockets (Codex's denies every `connect` and `bind`; FIFOs and socket
//! pairs it allows). The door is a FIFO beside the socket, `NAME.door`, in
//! the same private directory (the user's own, mode 0700).
//!
//! A client makes two FIFOs of its own there, `c-HEX.up` and `c-HEX.down`
//! (mode 0600), and knocks: it writes `c-HEX` and a newline to the door. The
//! server opens them only if each is a FIFO owned by its user with no access
//! for others, removes their names, and writes one byte down to say it has.
//! From then on each side relays between the FIFOs and one end of a socket
//! pair, whose other end it uses as it would a connected socket: the server
//! serves it as an accepted connection, the client speaks the protocol on
//! it. Who may knock is who may write the directory: the user, as for the
//! socket.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a client waits for the server to open its FIFOs.
const ANSWER: Duration = Duration::from_secs(5);

/// The door beside `socket`.
pub fn path(socket: &Path) -> PathBuf {
    let mut path = socket.as_os_str().to_owned();
    path.push(".door");
    PathBuf::from(path)
}

/// A server's door, open for knocks.
pub struct Door {
    /// The door's read end, polled; and a write end of its own, so that it
    /// never reads an end when the last client closes.
    knocks: File,
    _held: File,
    path: PathBuf,
    /// What a knock has written of a line so far.
    partial: Vec<u8>,
}

impl Door {
    /// Opens the door beside `socket`, making it if it is not there; one of
    /// another's, or anything else there, is refused.
    pub fn open(socket: &Path) -> io::Result<Door> {
        let path = path(socket);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !ours_fifo(&meta) => {
                return Err(io::Error::other(format!(
                    "{} is not a FIFO of this user's",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => fuxix::file::make_fifo(&path, 0o600)?,
            Err(e) => return Err(e),
        }
        let knocks = OpenOptions::new()
            .read(true)
            .custom_flags(fuxix::file::NONBLOCK | fuxix::file::NOFOLLOW)
            .open(&path)?;
        let held = OpenOptions::new()
            .write(true)
            .custom_flags(fuxix::file::NONBLOCK | fuxix::file::NOFOLLOW)
            .open(&path)?;
        if !ours_fifo(&knocks.metadata()?) {
            return Err(io::Error::other("the door changed as it was opened"));
        }
        Ok(Door {
            knocks,
            _held: held,
            path,
            partial: Vec::new(),
        })
    }

    /// What the server polls for knocks.
    pub fn fd(&self) -> &File {
        &self.knocks
    }

    /// The connections the knocks waiting bring: for each, the server's end
    /// of the pair its relays fill. A knock that names FIFOs not fit to
    /// open is dropped.
    pub fn knocks(&mut self) -> Vec<UnixStream> {
        let mut buffer = [0u8; 4096];
        loop {
            match self.knocks.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => self
                    .partial
                    .extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
            // A knock is short: more than a few lines' worth is noise.
            if self.partial.len() > 64 * 1024 {
                self.partial.clear();
                break;
            }
        }
        // Whole lines are knocks; a part line waits for the rest.
        let whole = self
            .partial
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |end| end.saturating_add(1));
        let rest = self.partial.get(whole..).unwrap_or_default().to_vec();
        let lines = std::mem::replace(&mut self.partial, rest);
        let mut streams = Vec::new();
        for line in lines
            .get(..whole)
            .unwrap_or_default()
            .split(|b| *b == b'\n')
        {
            let name = String::from_utf8_lossy(line);
            if let Some(dir) = self.path.parent()
                && let Ok(stream) = answer(dir, &name)
            {
                streams.push(stream);
            }
        }
        streams
    }
}

impl Drop for Door {
    fn drop(&mut self) {
        // Its own still: removed.
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| ours_fifo(&m)) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Whether `meta` is a FIFO of this user's that others cannot reach.
fn ours_fifo(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_fifo()
        && meta.uid() == fuxix::process::geteuid()
        && meta.mode() & 0o077 == 0
}

/// A knock's name, if it is one a client makes: `c-` and 16 hex digits.
fn knock_name(name: &str) -> Option<&str> {
    let hex = name.strip_prefix("c-")?;
    (hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(name)
}

/// Opens the FIFOs a knock names, in `dir`, and starts relaying them: the
/// server's end of the pair.
fn answer(dir: &Path, name: &str) -> io::Result<UnixStream> {
    let name = knock_name(name).ok_or_else(|| io::Error::other("not a knock"))?;
    let up_path = dir.join(format!("{name}.up"));
    let down_path = dir.join(format!("{name}.down"));
    for path in [&up_path, &down_path] {
        if !ours_fifo(&std::fs::symlink_metadata(path)?) {
            return Err(io::Error::other("not a FIFO of this user's"));
        }
    }
    let flags = fuxix::file::NONBLOCK | fuxix::file::NOFOLLOW;
    let up = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(&up_path)?;
    let mut down = OpenOptions::new()
        .write(true)
        .custom_flags(flags)
        .open(&down_path)?;
    if !ours_fifo(&up.metadata()?) || !ours_fifo(&down.metadata()?) {
        return Err(io::Error::other("a FIFO changed as it was opened"));
    }
    let _ = std::fs::remove_file(&up_path);
    let _ = std::fs::remove_file(&down_path);
    fuxix::io::set_nonblocking(&up, false)?;
    fuxix::io::set_nonblocking(&down, false)?;
    down.write_all(b"+")?;
    let (ours, theirs) = UnixStream::pair()?;
    // Read and written as a file: a sandbox that refuses send(2) allows
    // write(2).
    let shut = theirs.try_clone()?;
    let theirs = File::from(OwnedFd::from(theirs));
    let back = theirs.try_clone()?;
    // The end of either ends both.
    std::thread::spawn(move || {
        copy(up, &theirs);
        let _ = shut.shutdown(std::net::Shutdown::Both);
    });
    std::thread::spawn(move || copy(&back, down));
    Ok(ours)
}

/// Copies `from` into `into` until either ends.
fn copy(mut from: impl Read, mut into: impl Write) {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => {
                if into.write_all(buffer.get(..n).unwrap_or_default()).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Knocks at the door beside `socket`, as a client: its FIFOs, to read and
/// to write, once the server has opened them.
pub fn knock(socket: &Path) -> io::Result<Link> {
    let door = path(socket);
    let dir = door
        .parent()
        .ok_or_else(|| io::Error::other("a door with no directory"))?;
    let mut random = [0u8; 8];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let name = format!("c-{hex}");
    let up_path = dir.join(format!("{name}.up"));
    let down_path = dir.join(format!("{name}.down"));
    fuxix::file::make_fifo(&up_path, 0o600)?;
    let made = fuxix::file::make_fifo(&down_path, 0o600);
    let clean = || {
        let _ = std::fs::remove_file(&up_path);
        let _ = std::fs::remove_file(&down_path);
    };
    if let Err(e) = made {
        clean();
        return Err(e);
    }
    let opened = (|| {
        let flags = fuxix::file::NONBLOCK | fuxix::file::NOFOLLOW;
        let down = OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(&down_path)?;
        // Read and write: open at once, whether or not the server reads yet.
        let up = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(flags)
            .open(&up_path)?;
        let mut knocked = OpenOptions::new()
            .write(true)
            .custom_flags(flags)
            .open(&door)?;
        knocked.write_all(format!("{name}\n").as_bytes())?;
        // The server's byte: it has opened both.
        let mut down = Link::door(down, up)?;
        down.set_read_timeout(Some(ANSWER));
        let mut byte = [0u8; 1];
        match down.read(&mut byte) {
            Ok(1) => Ok(down),
            Ok(_) => Err(io::Error::other("the server closed the door")),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no server answered at the door",
            )),
            Err(e) => Err(e),
        }
    })();
    clean();
    opened
}

/// A client's connection to its server: a socket, or a door's two FIFOs.
/// Read and written with read(2) and write(2) alone, and timed with
/// poll(2): Codex's sandbox refuses send(2) and setsockopt(2), even on a
/// socket pair.
pub struct Link {
    input: File,
    output: File,
    /// The socket, to shut down; a door has none.
    socket: Option<UnixStream>,
    timeout: Option<Duration>,
}

impl Link {
    pub fn socket(stream: UnixStream) -> io::Result<Link> {
        let input = File::from(OwnedFd::from(stream.try_clone()?));
        let output = input.try_clone()?;
        Ok(Link {
            input,
            output,
            socket: Some(stream),
            timeout: None,
        })
    }

    fn door(down: File, up: File) -> io::Result<Link> {
        fuxix::io::set_nonblocking(&down, false)?;
        fuxix::io::set_nonblocking(&up, false)?;
        Ok(Link {
            input: down,
            output: up,
            socket: None,
            timeout: None,
        })
    }

    /// How long a read waits for something to come; `None`, for ever.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        Ok(fuxix::io::set_nonblocking(&self.input, nonblocking)?)
    }

    /// What is written to: a terminal can go with it over a socket.
    pub fn output(&self) -> &File {
        &self.output
    }

    /// Whether it is a socket: what can carry a descriptor.
    pub fn is_socket(&self) -> bool {
        self.socket.is_some()
    }

    /// A copy of its socket, to shut down from another thread.
    pub fn socket_copy(&self) -> Option<UnixStream> {
        self.socket.as_ref().and_then(|s| s.try_clone().ok())
    }
}

impl Read for Link {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(timeout) = self.timeout {
            let mut fds = [fuxix::poll::PollFd::new(
                &self.input,
                fuxix::poll::Events::IN,
            )];
            if fuxix::poll::poll(&mut fds, Some(timeout))? == 0 {
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
        self.input.read(buffer)
    }
}

impl Write for Link {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsFd for Link {
    /// What is polled to read.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.input.as_fd()
    }
}
