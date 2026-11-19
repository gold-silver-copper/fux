//! Opening files as fux needs, beyond what std's `OpenOptions` names.

/// `O_NOFOLLOW`, for `OpenOptions::custom_flags`: a symbolic link at the
/// path is refused, not followed.
#[cfg(unix)]
pub const NOFOLLOW: i32 = libc::O_NOFOLLOW;
/// Opens without waiting: a FIFO opened so returns at once.
#[cfg(unix)]
pub const NONBLOCK: i32 = libc::O_NONBLOCK;
/// Opening anything but a directory fails.
#[cfg(unix)]
pub const DIRECTORY: i32 = libc::O_DIRECTORY;
/// Opening a terminal does not make it the caller's controlling terminal.
#[cfg(unix)]
pub const NOCTTY: i32 = libc::O_NOCTTY;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// `options` set to make or open a file only its user may use, never
/// through a link: on Unix mode 0600 and `O_NOFOLLOW`; on Windows the
/// reparse point itself is opened, never followed, and no one may delete
/// or rename it while it is open (no `FILE_SHARE_DELETE`), its access its
/// private directory's ([`private_dir`]).
pub fn private_file(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(NOFOLLOW)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
    }
}

/// Makes the directory `path`, only its user's: on Unix mode 0700 (the
/// umask not asked); on Windows with a protected descriptor letting only
/// this process's default owner in, which what is made in it inherits. One
/// there already fails, as `create_dir` does.
pub fn private_dir(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(windows)]
    {
        windows::make_private_dir(path)
    }
}

/// Makes the FIFO `path`, only its user's: mode 0600 (the umask not
/// asked). One there already fails, as `create_dir` does; a link at the
/// path is not followed.
#[cfg(unix)]
pub fn private_fifo(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "a NUL in the path"))?;
    // SAFETY: `name` is a NUL-terminated string that outlives the call,
    // which reads it and touches no other memory.
    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Where fux's tests and tools make their short-lived files and sockets,
/// made if it is missing: `$TMPDIR` if it is set and resolves to at most
/// 40 bytes, else `~/.cache/fux`. A socket's path must stay under 104
/// bytes, and macOS's `$TMPDIR` alone takes half of that; `/tmp` is memory
/// on some machines, so it is used only when `$TMPDIR` names it.
#[cfg(unix)]
pub fn scratch() -> std::io::Result<std::path::PathBuf> {
    if let Some(tmp) = std::env::var_os("TMPDIR").filter(|v| !v.is_empty())
        && let Ok(tmp) = std::fs::canonicalize(tmp)
        && tmp.as_os_str().len() <= 40
    {
        return Ok(tmp);
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"))?;
    let dir = std::path::Path::new(&home).join(".cache/fux");
    std::fs::create_dir_all(&dir)?;
    dir.canonicalize()
}

/// Where a user's config lives: `XDG_CONFIG_HOME`, else `~/.config`.
#[cfg(unix)]
pub fn config_dir() -> Option<std::path::PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|d| !d.is_empty());
    set("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| set("HOME").map(|home| std::path::PathBuf::from(home).join(".config")))
}

/// A descriptor naming the file at `path`, a symbolic link refused,
/// without opening it for reading or writing (`O_PATH`). While it is held
/// the file's inode stays allocated, so its number cannot be given to
/// another file.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn pin(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn a_pin_names_its_file_and_keeps_its_inode() -> std::result::Result<(), String> {
        let path = scratch()
            .map_err(|e| e.to_string())?
            .join(format!("fuxix-pin-{}", std::process::id()));
        std::fs::write(&path, b"x").map_err(|e| e.to_string())?;
        let pinned = pin(&path).map_err(|e| e.to_string())?;
        let inode = pinned.metadata().map_err(|e| e.to_string())?.ino();
        assert_eq!(std::fs::metadata(&path).map(|m| m.ino()).ok(), Some(inode));
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        std::fs::write(&path, b"y").map_err(|e| e.to_string())?;
        assert_ne!(std::fs::metadata(&path).map(|m| m.ino()).ok(), Some(inode));
        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod fifo_tests {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    /// A FIFO is made private whatever the umask, and never twice.
    #[test]
    fn a_private_fifo_is_its_users_alone() -> std::result::Result<(), String> {
        let path = scratch()
            .map_err(|e| e.to_string())?
            .join(format!("fuxix-fifo-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        private_fifo(&path).map_err(|e| e.to_string())?;
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        assert!(meta.file_type().is_fifo());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert!(private_fifo(&path).is_err());
        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}

/// Files named relative to a directory held open, never by a path from
/// the root: whoever holds the directory stays on it when it is renamed
/// or moved, and reaches nothing that later takes its old name. Each
/// `name` is a relative path; no call follows a symbolic link at its end.
#[cfg(unix)]
pub mod at {
    use std::ffi::{CString, OsString};
    use std::fs::File;
    use std::io::{Error, ErrorKind, Result};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;

    /// How [`open`] opens: for reading, for writing, or both.
    pub const READ: i32 = libc::O_RDONLY;
    pub const WRITE: i32 = libc::O_WRONLY;
    pub const READ_WRITE: i32 = libc::O_RDWR;
    /// Makes the file if it is missing; with [`NEW`], fails if it is there.
    pub const CREATE: i32 = libc::O_CREAT;
    pub const NEW: i32 = libc::O_CREAT | libc::O_EXCL;
    /// Every write goes to the file's end.
    pub const APPEND: i32 = libc::O_APPEND;

    fn c(name: &Path) -> Result<CString> {
        CString::new(name.as_os_str().as_bytes())
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "a NUL in the name"))
    }

    fn done(returned: libc::c_int) -> Result<()> {
        match returned {
            0 => Ok(()),
            _ => Err(Error::last_os_error()),
        }
    }

    /// Opens `name` in `dir` with `flags` (the constants here and
    /// [`NONBLOCK`](super::NONBLOCK), [`DIRECTORY`](super::DIRECTORY),
    /// [`NOCTTY`](super::NOCTTY)); a file it makes is its user's alone
    /// (mode 0600). A link at the name is refused, and the descriptor is
    /// closed on exec.
    pub fn open(dir: &File, name: impl AsRef<Path>, flags: i32) -> Result<File> {
        let name = c(name.as_ref())?;
        let flags = flags | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `name` is a NUL-terminated string that outlives the
        // call, which reads it and touches no other memory.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        // SAFETY: `fd` was just opened and nothing else owns it.
        let file = unsafe { File::from_raw_fd(fd) };
        if flags & libc::O_CREAT != 0 {
            use std::os::unix::fs::PermissionsExt;
            // The umask may only have taken bits away.
            let meta = file.metadata()?;
            if meta.is_file() && meta.permissions().mode() & 0o777 != 0o600 {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
        }
        Ok(file)
    }

    /// Opens the directory `name` in `dir`, to hold it: nothing but a
    /// directory, and no link.
    pub fn open_dir(dir: &File, name: impl AsRef<Path>) -> Result<File> {
        open(dir, name, READ | libc::O_DIRECTORY | libc::O_NONBLOCK)
    }

    /// Makes the directory `name` in `dir`, its user's alone (mode 0700,
    /// the umask not asked). One there already fails.
    pub fn make_dir(dir: &File, name: impl AsRef<Path>) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let path = name.as_ref();
        let name = c(path)?;
        // SAFETY: as in `open`.
        done(unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) })?;
        open_dir(dir, path)?.set_permissions(std::fs::Permissions::from_mode(0o700))
    }

    /// Makes the FIFO `name` in `dir`, its user's alone (mode 0600, the
    /// umask not asked). One there already fails.
    pub fn make_fifo(dir: &File, name: impl AsRef<Path>) -> Result<()> {
        let name = c(name.as_ref())?;
        // SAFETY: as in `open`.
        done(unsafe { libc::mkfifoat(dir.as_raw_fd(), name.as_ptr(), 0o600) })?;
        // SAFETY: as in `open`.
        done(unsafe { libc::fchmodat(dir.as_raw_fd(), name.as_ptr(), 0o600, 0) })
    }

    /// Renames `from` in `from_dir` to `to` in `to_dir`, replacing what is
    /// there as `rename` does.
    pub fn rename(
        from_dir: &File,
        from: impl AsRef<Path>,
        to_dir: &File,
        to: impl AsRef<Path>,
    ) -> Result<()> {
        let (from, to) = (c(from.as_ref())?, c(to.as_ref())?);
        // SAFETY: both names are NUL-terminated strings that outlive the
        // call, which reads them and touches no other memory.
        done(unsafe {
            libc::renameat(
                from_dir.as_raw_fd(),
                from.as_ptr(),
                to_dir.as_raw_fd(),
                to.as_ptr(),
            )
        })
    }

    /// Renames as [`rename`] does, but never onto anything: with a file,
    /// a directory or a link at `to` it fails (`AlreadyExists`) and
    /// nothing changes, in one step.
    pub fn rename_new(
        from_dir: &File,
        from: impl AsRef<Path>,
        to_dir: &File,
        to: impl AsRef<Path>,
    ) -> Result<()> {
        let (from, to) = (c(from.as_ref())?, c(to.as_ref())?);
        let (from_fd, to_fd) = (from_dir.as_raw_fd(), to_dir.as_raw_fd());
        // SAFETY: both names are NUL-terminated strings that outlive the
        // call, which reads them and touches no other memory.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let returned = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                from_fd,
                from.as_ptr(),
                to_fd,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        // SAFETY: as above.
        #[cfg(target_vendor = "apple")]
        let returned = unsafe {
            libc::renameatx_np(
                from_fd,
                from.as_ptr(),
                to_fd,
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        match returned {
            0 => Ok(()),
            _ => Err(Error::last_os_error()),
        }
    }

    /// Removes the file `name` in `dir`, a link itself and never what it
    /// points at.
    pub fn remove_file(dir: &File, name: impl AsRef<Path>) -> Result<()> {
        let name = c(name.as_ref())?;
        // SAFETY: as in `open`.
        done(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) })
    }

    /// Removes the empty directory `name` in `dir`.
    pub fn remove_dir(dir: &File, name: impl AsRef<Path>) -> Result<()> {
        let name = c(name.as_ref())?;
        // SAFETY: as in `open`.
        done(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) })
    }

    /// The names in `dir` and each one's inode number, `.` and `..` left
    /// out, in the directory's own order; with more than `most`, an error.
    pub fn names(dir: &File, most: usize) -> Result<Vec<(OsString, u64)>> {
        // A stream of its own, from the directory's start: `dir`'s own
        // position is shared by nothing.
        let own = open_dir(dir, ".")?.into_raw_fd();
        // SAFETY: `own` is an open directory nothing else owns; the
        // stream owns it from here, and `closedir` closes it.
        let stream = unsafe { libc::fdopendir(own) };
        if stream.is_null() {
            let error = Error::last_os_error();
            // SAFETY: the stream was not made, so `own` is still this
            // function's to close.
            drop(unsafe { File::from_raw_fd(own) });
            return Err(error);
        }
        let mut names = Vec::new();
        let outcome = loop {
            // SAFETY: `stream` is open and read by this thread alone; the
            // entry is read before the next call.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                break Ok(());
            }
            // SAFETY: `entry` points at a valid entry until the next
            // `readdir`, which comes after its last use here.
            let entry = unsafe { &*entry };
            // SAFETY: the entry's name is NUL-terminated.
            let name = unsafe { std::ffi::CStr::from_ptr(entry.d_name.as_ptr()) };
            let (name, inode) = (name.to_bytes().to_vec(), entry.d_ino);
            if name == b"." || name == b".." {
                continue;
            }
            if names.len() >= most {
                break Err(Error::other(format!("more than {most} entries")));
            }
            names.push((OsString::from_vec(name), inode));
        };
        // SAFETY: `stream` is open; closing it closes its descriptor.
        unsafe { libc::closedir(stream) };
        outcome.map(|()| names)
    }
}

#[cfg(all(test, unix))]
mod at_tests {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

    /// Whoever holds a directory stays on it through a rename, makes
    /// private files in it, and never renames onto what is there.
    #[test]
    fn a_held_directory_is_followed_through_a_rename() -> std::result::Result<(), String> {
        use std::io::Write;
        let e = |e: std::io::Error| e.to_string();
        let base = scratch()
            .map_err(e)?
            .join(format!("fuxix-at-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        private_dir(&base).map_err(e)?;
        let parent = std::fs::File::open(&base).map_err(e)?;
        at::make_dir(&parent, "a").map_err(e)?;
        let held = at::open_dir(&parent, "a").map_err(e)?;
        std::fs::rename(base.join("a"), base.join("b")).map_err(e)?;
        at::make_fifo(&held, "fifo").map_err(e)?;
        let mut file = at::open(&held, "file", at::WRITE | at::NEW).map_err(e)?;
        file.write_all(b"x").map_err(e)?;
        let meta = std::fs::symlink_metadata(base.join("b/fifo")).map_err(e)?;
        assert!(meta.file_type().is_fifo());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let made = std::fs::metadata(base.join("b/file")).map_err(e)?;
        assert_eq!(made.permissions().mode() & 0o777, 0o600);
        let mut names = at::names(&held, 8).map_err(e)?;
        names.sort();
        let found: Vec<_> = names.iter().map(|(n, _)| n.to_string_lossy()).collect();
        assert_eq!(found, ["fifo", "file"]);
        assert!(names.iter().any(|(_, inode)| *inode == made.ino()));
        assert!(at::names(&held, 1).is_err());
        // Never onto anything: a file, or a link to nothing.
        std::os::unix::fs::symlink("nowhere", base.join("b/link")).map_err(e)?;
        for there in ["fifo", "link"] {
            let refused = at::rename_new(&held, "file", &held, there);
            assert_eq!(
                refused.map_err(|e| e.kind()),
                Err(std::io::ErrorKind::AlreadyExists)
            );
        }
        assert!(at::open(&held, "link", at::READ).is_err());
        at::rename_new(&held, "file", &parent, "out").map_err(e)?;
        at::rename(&held, "fifo", &parent, "out").map_err(e)?;
        at::remove_file(&held, "link").map_err(e)?;
        at::remove_file(&parent, "out").map_err(e)?;
        at::remove_dir(&parent, "b").map_err(e)?;
        std::fs::remove_dir(&base).map_err(e)
    }
}
