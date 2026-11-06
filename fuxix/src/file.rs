//! Opening files as fux needs, beyond what std's `OpenOptions` names.

/// `O_NOFOLLOW`, for `OpenOptions::custom_flags`: a symbolic link at the
/// path is refused, not followed.
#[cfg(unix)]
pub const NOFOLLOW: i32 = libc::O_NOFOLLOW;
/// Opens without waiting: a FIFO opened so returns at once.
#[cfg(unix)]
pub const NONBLOCK: i32 = libc::O_NONBLOCK;
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
