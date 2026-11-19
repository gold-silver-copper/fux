//! What differs between systems: where a pane listens, how it is reached,
//! how its claim is held, how it starts and how its program starts.
use crate::wire::{Code, Failure};
use std::ffi::OsString;
use std::fs::File;
use std::io::{PipeReader, PipeWriter};
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

fn io(what: impl std::fmt::Display, error: impl std::fmt::Display) -> Failure {
    Failure::new(Code::Io, format!("{what}: {error}"))
}

/// The claim on a pane's directory, held while its lock is: `None` if
/// another process holds it.
pub fn try_claim(dir: &Path) -> Result<Option<File>, Failure> {
    let file = claim_file(dir).map_err(|e| io(format!("opening {}", dir.display()), e))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(io(format!("locking {}", dir.display()), e)),
    }
}

/// A started daemon, not yet told to go on: closing `go` unsaid ends it
/// with nothing started.
pub struct Launch {
    pub go: PipeWriter,
    pub ready: PipeReader,
    /// What the caller must be told before the pane's program starts.
    pub bound: Option<&'static str>,
}

/// The null device, for the standard streams of a process that is done
/// with them.
pub fn detach_stdio() -> std::io::Result<()> {
    let null = File::options()
        .read(true)
        .write(true)
        .open(if cfg!(windows) { "NUL" } else { "/dev/null" })?;
    fuxix::io::redirect_stdout(&null).map_err(std::io::Error::from)?;
    fuxix::io::redirect_stdin(&null).map_err(std::io::Error::from)
}

#[cfg(unix)]
mod unix {
    use super::{Launch, OsString, Path, PathBuf, io};
    use crate::wire::{Code, Failure, PaneId};
    use fuxix::Errno;
    use fuxix::process::{Dispositions, Job};
    use fuxix::pty::Slave;
    use std::fs::File;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::process::{Command, Stdio};

    pub type Stream = UnixStream;
    pub type Listener = UnixListener;

    fn socket(dir: &Path) -> PathBuf {
        dir.join("socket")
    }

    /// The directory itself is what is locked: a cleaner of old files
    /// leaves a directory holding a live socket. A link, or anything but a
    /// directory, is refused unopened.
    pub(super) fn claim_file(dir: &Path) -> std::io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        File::options()
            .read(true)
            .custom_flags(libc_flags())
            .open(dir)
    }

    fn libc_flags() -> i32 {
        fuxix::file::NOFOLLOW | fuxix::file::DIRECTORY | fuxix::file::NONBLOCK
    }

    /// Whether `dir` is still the directory `claim` holds.
    pub fn still_claimed(dir: &Path, claim: &File) -> bool {
        use std::os::unix::fs::MetadataExt;
        let held = claim.metadata();
        let there = std::fs::symlink_metadata(dir);
        matches!((held, there), (Ok(a), Ok(b)) if a.dev() == b.dev() && a.ino() == b.ino())
    }

    /// Checks that `path`, a directory, is ours alone.
    pub fn check_private(path: &Path) -> Result<(), Failure> {
        use std::os::unix::fs::MetadataExt;
        let shown = path.display();
        let meta = std::fs::symlink_metadata(path).map_err(|e| io(&shown, e))?;
        if !meta.is_dir() {
            let why = format!("{shown} is not a directory (a link is refused)");
            return Err(Failure::new(Code::Denied, why));
        }
        if meta.uid() != fuxix::process::geteuid() {
            let why = format!("{shown} belongs to uid {}", meta.uid());
            return Err(Failure::new(Code::Denied, why));
        }
        if meta.mode() & 0o077 != 0 {
            let why = format!(
                "{shown} has mode {:o}, and must be 700",
                meta.mode() & 0o777
            );
            return Err(Failure::new(Code::Denied, why));
        }
        Ok(())
    }

    /// Listens at `dir`'s socket, only its user able to reach it.
    pub fn bind(dir: &Path, _id: PaneId) -> Result<Listener, Failure> {
        let path = socket(dir);
        let most = fuxix::socket::max_path_len();
        let len = path.as_os_str().len();
        if len > most {
            let why = format!("{} is {len} bytes, at most {most}", path.display());
            return Err(Failure::new(Code::Limit, why));
        }
        let shown = path.display();
        let fd = fuxix::socket::stream().map_err(|e| io("socket", e))?;
        fuxix::socket::bind(&fd, &path).map_err(|e| io(&shown, e))?;
        std::fs::set_permissions(&path, PermissionsExt::from_mode(0o600))
            .map_err(|e| io(&shown, e))?;
        fuxix::socket::listen(&fd, 64).map_err(|e| io(&shown, e))?;
        let listener = UnixListener::from(fd);
        listener.set_nonblocking(true).map_err(|e| io(&shown, e))?;
        Ok(listener)
    }

    /// The next connection of this user, if one waits.
    pub fn accept(listener: &mut Listener) -> Result<Option<Stream>, Failure> {
        loop {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io("accepting", e)),
            };
            let ours = fuxix::socket::peer(&stream).is_ok_and(|peer| peer.user.is_current());
            if ours && stream.set_nonblocking(true).is_ok() {
                return Ok(Some(stream));
            }
        }
    }

    /// Connects to the pane in `dir`, and to no one but this user's. There
    /// is no id to compare on Unix: the socket's place says whose it is.
    pub fn connect(dir: &Path) -> Result<(Stream, Option<PaneId>), Failure> {
        let path = socket(dir);
        let shown = path.display();
        let fd = fuxix::socket::stream().map_err(|e| io("socket", e))?;
        fuxix::io::set_nonblocking(&fd, true).map_err(|e| io("socket", e))?;
        match fuxix::socket::connect(&fd, &path) {
            Ok(()) => {}
            Err(Errno::AGAIN | Errno::INPROGRESS) => {
                return Err(Failure::new(Code::Busy, format!("{shown} is full")));
            }
            Err(e) => {
                let kind = std::io::Error::from(e).kind();
                let gone = matches!(
                    kind,
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                );
                let code = if gone { Code::Missing } else { Code::Io };
                return Err(Failure::new(code, format!("{shown}: {e}")));
            }
        }
        let stream = UnixStream::from(fd);
        let peer = fuxix::socket::peer(&stream).map_err(|e| io(&shown, e))?;
        if !peer.user.is_current() {
            let why = format!("{shown} is served by {}", peer.user);
            return Err(Failure::new(Code::Denied, why));
        }
        Ok((stream, None))
    }

    /// Removes what `bind` left in `dir`.
    pub fn remove_endpoint(dir: &Path) {
        let _ = std::fs::remove_file(socket(dir));
    }

    /// Whether `dir` holds an endpoint.
    pub fn has_endpoint(dir: &Path) -> bool {
        std::fs::symlink_metadata(socket(dir)).is_ok()
    }

    /// Gives up a stream with nothing further owed: nothing lingers here.
    pub fn abandon(_stream: &Stream) {}

    /// Starts this program as a pane's daemon: in a session of its own, in
    /// `root`, holding nothing of the caller's but the two pipes.
    pub fn launch(root: &Path, args: &[OsString]) -> Result<Launch, Failure> {
        let exe = std::env::current_exe().map_err(|e| io("finding this program", e))?;
        let (go_read, go) = std::io::pipe().map_err(|e| io("pipe", e))?;
        let (ready, ready_write) = std::io::pipe().map_err(|e| io("pipe", e))?;
        let mut command = Command::new(&exe);
        command
            .args(args)
            .current_dir(root)
            .stdin(Stdio::from(go_read))
            .stdout(Stdio::from(ready_write))
            .stderr(Stdio::null());
        fuxix::process::lead_session(&mut command, false, Dispositions::Default);
        let child = command
            .spawn()
            .map_err(|e| io(format!("starting {}", exe.display()), e))?;
        drop(child);
        Ok(Launch {
            go,
            ready,
            bound: None,
        })
    }

    /// Starts `argv` on `slave`.
    pub fn spawn(
        slave: Slave,
        argv: &[OsString],
        setup: impl FnOnce(&mut Command),
    ) -> std::io::Result<Job> {
        let (child, job) = slave.spawn(argv, setup, Dispositions::Default)?;
        drop(child);
        Ok(job)
    }

    /// A pane's ConPTY choice: Unix has none.
    pub fn prepare_terminal(_root: &Path) -> Option<String> {
        None
    }
}

#[cfg(windows)]
mod windows {
    use super::{Launch, OsString, Path, PathBuf, io};
    use crate::wire::{Code, Failure, PaneId};
    use fuxix::Errno;
    use fuxix::process::{Breakaway, Dispositions, Job};
    use fuxix::pty::Slave;
    use std::ffi::OsStr;
    use std::fs::File;
    use std::os::windows::io::AsHandle;
    use std::process::Command;

    pub type Stream = fuxix::io::Stream;
    pub type Listener = fuxix::socket::Listener;

    /// A marker holds a pane id: 32 hex digits.
    const MARKER_MOST: u64 = 64;

    fn marker(dir: &Path) -> PathBuf {
        dir.join("marker")
    }

    fn pipe_name(id: PaneId) -> Result<String, Failure> {
        fuxix::socket::name(&format!("fux2-{id}")).map_err(|e| io("pipe name", e))
    }

    /// A file in the directory: a held one can be neither deleted nor
    /// renamed.
    pub(super) fn claim_file(dir: &Path) -> std::io::Result<File> {
        let mut options = File::options();
        options.read(true).write(true).create(true).truncate(false);
        fuxix::file::private_file(&mut options).open(dir.join("claim"))
    }

    /// Whether `dir` is still the directory `claim` holds: a directory
    /// holding a file open without delete sharing cannot be moved, so it is.
    pub fn still_claimed(_dir: &Path, _claim: &File) -> bool {
        true
    }

    /// Checks that `path`, a directory, is ours alone.
    pub fn check_private(path: &Path) -> Result<(), Failure> {
        let shown = path.display();
        match fuxix::file::is_private_dir(path) {
            Ok(true) => Ok(()),
            Ok(false) => {
                let why = format!("{shown} is a link, or open to others, or not ours");
                Err(Failure::new(Code::Denied, why))
            }
            Err(e) => Err(io(&shown, e)),
        }
    }

    /// Listens on the pipe named for `id`, and writes `id` where a client
    /// finds it.
    pub fn bind(dir: &Path, id: PaneId) -> Result<Listener, Failure> {
        let listener =
            Listener::bind(&pipe_name(id)?).map_err(|e| io(format!("pipe of pane {id}"), e))?;
        let path = marker(dir);
        let mut options = File::options();
        options.write(true).create_new(true);
        let mut file = fuxix::file::private_file(&mut options)
            .open(&path)
            .map_err(|e| io(path.display(), e))?;
        std::io::Write::write_all(&mut file, id.to_string().as_bytes())
            .map_err(|e| io(path.display(), e))?;
        Ok(listener)
    }

    /// The next connection of this user, if one waits.
    pub fn accept(listener: &mut Listener) -> Result<Option<Stream>, Failure> {
        loop {
            let connection = match listener.accept() {
                Ok(connection) => connection,
                Err(Errno::AGAIN) => return Ok(None),
                Err(e) => return Err(io("accepting", e)),
            };
            let ours = connection.connected().ok().is_some_and(|at| {
                fuxix::socket::peer(&connection, at).is_ok_and(|peer| peer.user.is_current())
            });
            if ours {
                return connection
                    .stream()
                    .map(Some)
                    .map_err(|e| io("accepting", e));
            }
        }
    }

    /// Connects to the pane `dir`'s marker names; fuxix refuses a pipe
    /// that is not this user's. The id is for the caller to compare with
    /// the pane's greeting.
    pub fn connect(dir: &Path) -> Result<(Stream, Option<PaneId>), Failure> {
        let path = marker(dir);
        let shown = path.display();
        let mut text = String::new();
        let read = File::open(&path).and_then(|file| {
            std::io::Read::read_to_string(&mut std::io::Read::take(file, MARKER_MOST), &mut text)
        });
        read.map_err(|e| {
            let gone = e.kind() == std::io::ErrorKind::NotFound;
            let code = if gone { Code::Missing } else { Code::Io };
            Failure::new(code, format!("{shown}: {e}"))
        })?;
        let id = PaneId::parse(text.trim())
            .ok_or_else(|| Failure::new(Code::Io, format!("{shown} names no pane")))?;
        let stream = fuxix::socket::connect_now(&pipe_name(id)?).map_err(|e| match e {
            Errno::AGAIN => Failure::new(Code::Busy, format!("pane {id} has no free instance")),
            e if std::io::Error::from(e).kind() == std::io::ErrorKind::NotFound => {
                Failure::new(Code::Missing, format!("pane {id} is gone"))
            }
            e => io(format!("pane {id}"), e),
        })?;
        Ok((stream, Some(id)))
    }

    /// Removes what `bind` left in `dir`.
    pub fn remove_endpoint(dir: &Path) {
        let _ = std::fs::remove_file(marker(dir));
    }

    /// Whether `dir` holds an endpoint.
    pub fn has_endpoint(dir: &Path) -> bool {
        std::fs::symlink_metadata(marker(dir)).is_ok()
    }

    /// Gives up a stream: its write in flight is cancelled, so no thread
    /// is left waiting on a reader that stopped.
    pub fn abandon(stream: &Stream) {
        stream.cancel_write();
    }

    /// A copy of this program kept in `root`, which a running pane holds
    /// instead of the installed file, so that the installed one can be
    /// replaced. Named for its contents; copies no pane runs are removed.
    fn kept_copy(root: &Path) -> Result<PathBuf, Failure> {
        use std::hash::{Hash, Hasher};
        let exe = std::env::current_exe().map_err(|e| io("finding this program", e))?;
        let bytes = std::fs::read(&exe).map_err(|e| io(exe.display(), e))?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let bin = root.join("bin");
        match fuxix::file::private_dir(&bin) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(bin.display(), e)),
        }
        check_private(&bin)?;
        let name = format!("fux2-{:016x}.exe", hasher.finish());
        let path = bin.join(&name);
        if !path.exists() {
            let scratch = bin.join(format!("{name}.{}", std::process::id()));
            std::fs::write(&scratch, &bytes).map_err(|e| io(scratch.display(), e))?;
            std::fs::rename(&scratch, &path).map_err(|e| io(path.display(), e))?;
        }
        // Only this program's own copies, finished ones, go: the caller holds
        // the session's lock, so no start is between staging and renaming.
        // A copy a running pane uses cannot be removed, and stays.
        for entry in std::fs::read_dir(&bin).into_iter().flatten().flatten() {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            let copy = file
                .strip_prefix("fux2-")
                .and_then(|rest| rest.strip_suffix(".exe"))
                .is_some_and(|hash| {
                    hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                });
            if copy && file != name.as_str() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(path)
    }

    /// Starts this program as a pane's daemon, out of any job it may
    /// leave. A job that keeps it in means the pane ends with that job:
    /// said in `bound`, for the caller to report before the program starts.
    pub fn launch(root: &Path, args: &[OsString]) -> Result<Launch, Failure> {
        let exe = kept_copy(root)?;
        let (go_read, go) = std::io::pipe().map_err(|e| io("pipe", e))?;
        let (ready, ready_write) = std::io::pipe().map_err(|e| io("pipe", e))?;
        let args: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
        let stdio = [
            Some(go_read.as_handle()),
            Some(ready_write.as_handle()),
            None,
        ];
        let started = fuxix::process::start_in(&exe, &args, stdio, Some(root))
            .map_err(|e| io(format!("starting {}", exe.display()), e))?;
        let bound = (started.breakaway == Breakaway::Refused).then_some(
            "this process is in a job that lets nothing leave it: \
             the pane ends when that job does",
        );
        Ok(Launch { go, ready, bound })
    }

    /// Starts `argv` on `slave`.
    pub fn spawn(
        slave: Slave,
        argv: &[OsString],
        setup: impl FnOnce(&mut Command),
    ) -> std::io::Result<Job> {
        slave.spawn(argv, setup, Dispositions::Default)
    }

    /// Chooses the pinned ConPTY kept beside the session, fetching it the
    /// first time; what it says, if the built-in one is used instead, is
    /// shown with the pane's screen.
    pub fn prepare_terminal(root: &Path) -> Option<String> {
        let dir = root.join("conpty");
        let fetched = fuxix::pty::fetch(&dir).err();
        let loaded = fuxix::pty::load(&dir).err()?;
        let why = fetched.map_or_else(|| loaded.to_string(), |e| e.to_string());
        Some(format!("Windows' built-in ConPTY is in use: {why}"))
    }
}
