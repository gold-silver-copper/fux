//! A door: the way into a server for a client in a sandbox that forbids
//! Unix sockets (Codex's denies every `connect` and `bind`; FIFOs and socket
//! pairs it allows). The door is a FIFO beside the socket, `NAME.door`, in
//! the same private directory (the user's own, mode 0700).
//!
//! A client makes two FIFOs of its own there, `c-HEX.up` and `c-HEX.down`
//! (mode 0600), and knocks: it writes `c-HEX` and a newline to the door. The
//! server opens them only if each is a FIFO owned by its user with no access
//! for others, removes their names, and writes one byte down to say it has.
//! From then on the two FIFOs are the connection: the client writes up and
//! reads down, the server reads up and writes down, as each would a
//! connected socket, with read(2), write(2) and poll(2) alone. No
//! descriptor goes through them. Who may knock is who may write the
//! directory: the user, as for the socket.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a client waits for a server's answer: its Hello, or the FIFOs
/// a door's knock asks for.
pub(crate) const ANSWER: Duration = Duration::from_secs(5);

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
        if let Err(e) = fuxix::file::make_fifo(&path, 0o600)
            && e.kind() != io::ErrorKind::AlreadyExists
        {
            return Err(e);
        }
        // Whatever is there, checked as it was opened: no symbolic link is
        // followed, and opening a file or a directory changes nothing.
        let knocks = open_fifo(&path, true, false)?;
        let held = open_fifo(&path, false, true)?;
        if !ours_fifo(&knocks.metadata()?) {
            return Err(io::Error::other(format!(
                "{} is not a FIFO of this user's",
                path.display()
            )));
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

    /// The connections the knocks waiting bring. A knock that names FIFOs
    /// not fit to open is dropped.
    pub fn knocks(&mut self) -> Vec<Fifos> {
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
        (lines
            .get(..whole)
            .unwrap_or_default()
            .split(|b| *b == b'\n'))
        .filter_map(|line| answer(self.path.parent()?, &String::from_utf8_lossy(line)).ok())
        .collect()
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

/// Opens the FIFO at `path` without waiting for its other end, a symbolic
/// link refused.
fn open_fifo(path: &Path, read: bool, write: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(read)
        .write(write)
        .custom_flags(fuxix::file::NONBLOCK | fuxix::file::NOFOLLOW)
        .open(path)
}

/// A knock's FIFOs in `dir`: what the client writes goes up, what the
/// server writes down.
fn fifos(dir: &Path, name: &str) -> [PathBuf; 2] {
    [
        dir.join(format!("{name}.up")),
        dir.join(format!("{name}.down")),
    ]
}

fn remove(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

/// A door's connection as the server holds it: `up`, read for what the
/// client sends, and `down`, written with what it is sent; both
/// nonblocking.
pub struct Fifos {
    pub up: File,
    pub down: File,
}

/// Opens the FIFOs a knock names, in `dir`, and says so down them.
fn answer(dir: &Path, name: &str) -> io::Result<Fifos> {
    let name = knock_name(name).ok_or_else(|| io::Error::other("not a knock"))?;
    let paths = fifos(dir, name);
    let [up_path, down_path] = &paths;
    // Checked as they were opened: no symbolic link is followed, and
    // opening anything else the private directory may hold changes nothing.
    let up = open_fifo(up_path, true, false)?;
    let mut down = open_fifo(down_path, false, true)?;
    if !ours_fifo(&up.metadata()?) || !ours_fifo(&down.metadata()?) {
        return Err(io::Error::other("not a FIFO of this user's"));
    }
    remove(&paths);
    // One byte, into a FIFO no one has written: it has room.
    down.write_all(b"+")?;
    Ok(Fifos { up, down })
}

/// Waits until `file` has something to read: `Ok(false)` if `stop` ends
/// first, a `TimedOut` error if `timeout` (`None`, for ever) passes. With
/// neither to wait on, at once.
fn ready(
    file: &impl AsFd,
    stop: Option<&io::PipeReader>,
    timeout: Option<Duration>,
) -> io::Result<bool> {
    use fuxix::poll::{Events, PollFd};
    let mut fds = match stop {
        Some(stop) => vec![PollFd::new(file, Events::IN), PollFd::new(stop, Events::IN)],
        None if timeout.is_some() => vec![PollFd::new(file, Events::IN)],
        None => return Ok(true),
    };
    if fuxix::poll::poll(&mut fds, timeout)? == 0 {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(fds
        .get(1)
        .is_none_or(|stopped| stopped.revents().is_empty()))
}

/// The door beside `socket` opened to knock at, if a server holds it open
/// and it is a FIFO of this user's that others cannot reach, checked on
/// the opened file: whether a server serves it. One no server reads, left
/// by a server that stopped, is refused as a socket left so is.
pub(crate) fn open_door(socket: &Path) -> io::Result<File> {
    let door = match open_fifo(&path(socket), false, true) {
        Err(e) if fuxix::Errno::from_io_error(&e) == Some(fuxix::Errno::NXIO) => {
            return Err(io::ErrorKind::ConnectionRefused.into());
        }
        opened => opened?,
    };
    if !ours_fifo(&door.metadata()?) {
        return Err(io::Error::other("the door is not a FIFO of this user's"));
    }
    Ok(door)
}

/// Knocks at the door beside `socket`, as a client: its FIFOs, to read and
/// to write, once the server has opened them.
pub fn knock(socket: &Path) -> io::Result<Link> {
    let mut knocked = open_door(socket)?;
    let dir = socket
        .parent()
        .ok_or_else(|| io::Error::other("a door with no directory"))?;
    let hex = crate::marks::nonce().ok_or_else(|| io::Error::other("no random bytes"))?;
    let name = format!("c-{hex}");
    let paths = fifos(dir, &name);
    let [up_path, down_path] = &paths;
    fuxix::file::make_fifo(up_path, 0o600)?;
    let opened = (|| {
        fuxix::file::make_fifo(down_path, 0o600)?;
        let down = open_fifo(down_path, true, false)?;
        // Write alone, so that once the server is gone a write fails rather
        // than waits: opened read and write first, so that the write-only
        // open finds a reader and comes at once, whether or not the server
        // reads yet.
        let either = open_fifo(up_path, true, true)?;
        let up = open_fifo(up_path, false, true)?;
        drop(either);
        knocked.write_all(format!("{name}\n").as_bytes())?;
        // The server's byte: it has opened both. Read from the FIFO itself,
        // which shows nothing to poll until the server has written.
        let mut fds = [fuxix::poll::PollFd::new(&down, fuxix::poll::Events::IN)];
        if fuxix::poll::poll(&mut fds, Some(ANSWER))? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no server answered at the door",
            ));
        }
        let mut byte = [0u8; 1];
        match (&down).read(&mut byte) {
            Ok(1) => {
                fuxix::io::set_nonblocking(&down, false)?;
                fuxix::io::set_nonblocking(&up, false)?;
                Ok(Link {
                    file: down,
                    up: Some(up),
                    timeout: None,
                    stop: None,
                })
            }
            Ok(_) => Err(io::Error::other("the server closed the door")),
            Err(e) => Err(e),
        }
    })();
    remove(&paths);
    opened
}

/// A client's connection to its server: its socket, or a door's two FIFOs.
/// Read and written with read(2) and write(2) alone, and timed and stopped
/// with poll(2): Codex's sandbox refuses send(2), setsockopt(2) and
/// shutdown(2), even on a socket pair.
pub struct Link {
    /// What is read: the socket, as a file, or the door's `down`.
    file: File,
    /// What a door's writes go to, its `up`; a socket's go to `file`. A
    /// door carries no descriptor.
    up: Option<File>,
    timeout: Option<Duration>,
    /// Ends its reads once its other end closes (`Link::stopper`).
    stop: Option<io::PipeReader>,
}

impl Link {
    pub fn socket(socket: UnixStream) -> Link {
        Link {
            file: File::from(OwnedFd::from(socket)),
            up: None,
            timeout: None,
            stop: None,
        }
    }

    /// How long a read waits for something to come; `None`, for ever.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }

    /// What is written to: a terminal can go with it over a socket.
    pub fn output(&self) -> &File {
        self.up.as_ref().unwrap_or(&self.file)
    }

    /// Whether it is the server's socket: what can carry a descriptor.
    pub fn is_socket(&self) -> bool {
        self.up.is_none()
    }

    /// What stops it from another thread: dropped, its reads end, as at
    /// the server's end, from then on.
    pub fn stopper(&mut self) -> io::Result<io::PipeWriter> {
        let (stop, stopper) = io::pipe()?;
        self.stop = Some(stop);
        Ok(stopper)
    }
}

impl Read for Link {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !ready(&self.file, self.stop.as_ref(), self.timeout)? {
            return Ok(0);
        }
        self.file.read(buffer)
    }
}

impl Write for Link {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output().write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsFd for Link {
    /// What is polled to read.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A knock's two FIFOs made in `dir` with `mode`, opened as a client
    /// holds them: down to read, up to read and write.
    fn fifos_of(dir: &Path, name: &str, mode: u32) -> io::Result<[File; 2]> {
        let [up, down] = fifos(dir, name);
        for path in [&up, &down] {
            fuxix::file::make_fifo(path, 0o600)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        }
        Ok([open_fifo(&down, true, false)?, open_fifo(&up, true, true)?])
    }

    /// Knocks are whole lines, a part line waiting for the rest; a long
    /// line of noise is dropped without the knock after it, and what is
    /// kept of a part line is bounded. A knock is answered only for a
    /// client's name and FIFOs of the user's that others cannot reach; the
    /// door itself must be such a FIFO.
    #[test]
    fn knocks_are_whole_lines_naming_fifos_fit_to_open() -> io::Result<()> {
        let dir = fuxix::file::scratch()?.join(format!("fux-door-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let socket = dir.join("s.sock");
        let mut door = Door::open(&socket)?;
        let mut knock = open_fifo(&path(&socket), false, true)?;
        let answered = |down: &File| {
            let mut byte = [0u8; 1];
            (&*down)
                .read(&mut byte)
                .is_ok_and(|n| n == 1 && byte == *b"+")
        };

        let [down, _up] = fifos_of(&dir, "c-0123456789abcdef", 0o600)?;
        knock.write_all(b"c-0123456789")?;
        assert!(door.knocks().is_empty(), "a part line waits");
        knock.write_all(b"abcdef\n")?;
        assert_eq!(door.knocks().len(), 1);
        assert!(answered(&down));

        let [down, _up] = fifos_of(&dir, "c-00000000000000aa", 0o600)?;
        let mut noise = vec![b'x'; 3000];
        noise.extend_from_slice(b"\nc-00000000000000aa\n");
        knock.write_all(&noise)?;
        assert_eq!(door.knocks().len(), 1, "the knock after the noise");
        assert!(answered(&down));

        let _open = fifos_of(&dir, "c-00000000000000bb", 0o660)?;
        let _short = fifos_of(&dir, "c-abc", 0o600)?;
        knock.write_all(b"c-00000000000000bb\nc-abc\n")?;
        assert!(door.knocks().is_empty(), "others may reach it; not a name");

        for _ in 0..20 {
            knock.write_all(&[b'y'; 4000])?;
            assert!(door.knocks().is_empty());
            assert!(door.partial.len() <= 64 * 1024, "{}", door.partial.len());
        }

        let file = dir.join("f.sock");
        let regular = path(&file);
        std::fs::write(&regular, "")?;
        std::fs::set_permissions(&regular, std::fs::Permissions::from_mode(0o600))?;
        assert!(Door::open(&file).is_err(), "a file is no door");
        // Nor to a client, which neither waits on it nor writes its knock
        // there, nor counts it as a server's.
        let started = std::time::Instant::now();
        assert!(super::knock(&file).is_err(), "knocked at a file");
        assert!(started.elapsed() < ANSWER, "waited at a file");
        assert!(open_door(&file).is_err(), "a file served");
        assert_eq!(std::fs::read(&regular)?, b"", "a knock written to a file");
        drop(door);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// A door's FIFOs are the connection: what the client writes the
    /// server reads, and back; and a client whose server is gone gets an
    /// error on writing more than a FIFO holds, and an end on reading, not
    /// a wait for ever.
    #[test]
    fn a_door_client_hears_its_server_go() -> io::Result<()> {
        let dir = fuxix::file::scratch()?.join(format!("fux-door-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let socket = dir.join("s.sock");
        let mut door = Door::open(&socket)?;
        let (sent, came) = std::sync::mpsc::channel();
        let client = std::thread::spawn(move || {
            let mut link = super::knock(&socket)?;
            link.write_all(b"up")?;
            let mut back = [0u8; 4];
            link.read_exact(&mut back)?;
            sent.send(back == *b"down").map_err(io::Error::other)?;
            // The server goes here.
            let mut byte = [0u8; 1];
            let ended = link.read(&mut byte)?;
            let wrote = link.write_all(&vec![0u8; 512 * 1024]);
            sent.send(ended == 0 && wrote.is_err())
                .map_err(io::Error::other)
        });
        let wait = Duration::from_secs(5);
        let started = std::time::Instant::now();
        let mut fifos = loop {
            match door.knocks().pop() {
                Some(fifos) => break fifos,
                None if started.elapsed() > wait => return Err(io::ErrorKind::TimedOut.into()),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        let mut got = [0u8; 2];
        fuxix::io::set_nonblocking(&fifos.up, false)?;
        fifos.up.read_exact(&mut got)?;
        assert_eq!(&got, b"up");
        fifos.down.write_all(b"down")?;
        assert_eq!(
            came.recv_timeout(wait).ok(),
            Some(true),
            "it reads what is sent"
        );
        drop(fifos);
        drop(door);
        assert_eq!(
            came.recv_timeout(wait).ok(),
            Some(true),
            "it hears its server go"
        );
        client
            .join()
            .map_err(|_| io::Error::other("the client panicked"))??;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
