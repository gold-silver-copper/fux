//! The capped daemon diagnostics log.

/// A private, exclusively locked, one-MiB-capped append log; anything not private or already
/// held by another process becomes a sink so diagnostics never block or leak.
pub enum CappedLog {
    File {
        file: nix::fcntl::Flock<std::fs::File>,
        remaining: usize,
    },
    Sink(std::io::Sink),
}

impl CappedLog {
    #[must_use]
    pub fn open(path: &std::path::Path) -> Self {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut options = std::fs::OpenOptions::new();
        options
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW);
        let Ok(file) = options.open(path) else {
            return Self::Sink(std::io::sink());
        };
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let private = file.metadata().is_ok_and(|metadata| {
            metadata.is_file()
                && metadata.permissions().mode() & 0o077 == 0
                && path
                    .parent()
                    .and_then(|parent| std::fs::metadata(parent).ok())
                    .is_some_and(|parent| parent.uid() == metadata.uid())
        });
        if !private {
            return Self::Sink(std::io::sink());
        }
        let Ok(file) = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        else {
            return Self::Sink(std::io::sink());
        };
        if file
            .metadata()
            .is_ok_and(|metadata| metadata.len() >= 1024 * 1024)
        {
            let _ = file.set_len(0);
        }
        let used = file.metadata().map_or(1024 * 1024, |metadata| {
            metadata.len().min(1024 * 1024) as usize
        });
        Self::File {
            file,
            remaining: (1024 * 1024_usize).saturating_sub(used),
        }
    }
}

impl std::io::Write for CappedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::File { file, remaining } => {
                let kept = bytes.len().min(*remaining);
                std::io::Write::write_all(&mut **file, bytes.get(..kept).unwrap_or_default())?;
                *remaining -= kept;
                // Diagnostics are best-effort: discard overflow without delaying
                // or failing the operation that produced the record.
                Ok(bytes.len())
            }
            Self::Sink(sink) => std::io::Write::write(sink, bytes),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::File { file, .. } => std::io::Write::flush(&mut **file),
            Self::Sink(sink) => std::io::Write::flush(sink),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_log_is_private_and_truncated_at_one_mib() -> std::io::Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("fux-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let path = root.join("daemon.log");
        std::fs::write(&path, vec![b'x'; 1024 * 1024])?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let mut log = CappedLog::open(&path);
        log.write_all(b"fresh")?;
        log.flush()?;
        assert!(std::fs::metadata(&path)?.len() < 1024 * 1024);
        let before = std::fs::metadata(&path)?.len();
        let mut contender = CappedLog::open(&path);
        contender.write_all(b"must not interleave")?;
        assert_eq!(std::fs::metadata(&path)?.len(), before);
        log.write_all(&vec![b'y'; 2 * 1024 * 1024])?;
        log.flush()?;
        assert_eq!(std::fs::metadata(&path)?.len(), 1024 * 1024);
        drop(log);
        let mut reopened = CappedLog::open(&path);
        reopened.write_all(b"next record")?;
        reopened.flush()?;
        assert_eq!(std::fs::read(&path)?, b"next record");
        assert_eq!(std::fs::metadata(&path)?.permissions().mode() & 0o077, 0);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
