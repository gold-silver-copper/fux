//! Opening files as fux needs, beyond what std's `OpenOptions` names.

/// `O_NOFOLLOW`, for `OpenOptions::custom_flags`: a symbolic link at the
/// path is refused, not followed.
pub const NOFOLLOW: i32 = libc::O_NOFOLLOW;

/// A descriptor naming the file at `path`, a symbolic link refused,
/// without opening it for reading or writing (`O_PATH`). While it is held
/// the file's inode stays allocated, so its number cannot be given to
/// another file (bevy-final finding 014).
#[cfg(any(target_os = "linux", target_os = "android"))]
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
        let path = std::env::temp_dir().join(format!("fuxix-pin-{}", std::process::id()));
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
