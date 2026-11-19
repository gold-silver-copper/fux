//! What differs between systems: where a pane listens, how it is reached,
//! how its claim is held, how it starts and how its program starts.
use crate::wire::{Code, Failure};
use std::ffi::OsString;
use std::fs::File;
use std::io::{PipeReader, PipeWriter};
use std::path::{Path, PathBuf};

pub use unix::*;

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
}

/// The null device, for the standard streams of a process that is done
/// with them.
pub fn detach_stdio() -> std::io::Result<()> {
    let null = File::options().read(true).write(true).open("/dev/null")?;
    fuxix::io::redirect_stdout(&null).map_err(std::io::Error::from)?;
    fuxix::io::redirect_stdin(&null).map_err(std::io::Error::from)
}

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
        Ok(Launch { go, ready })
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
}
