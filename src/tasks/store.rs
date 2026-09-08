//! Private, bounded, atomic journal replacement under a cross-process lock.
use super::model::Journal;
use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub const MAX_BYTES: usize = 4 * 1024 * 1024;
// 128 worktree parents + 128 check roots + 32 waiter locks + journal/runner locks,
// adapters and up to 16 interrupted writes must remain recoverable together.
const MAX_STATE_ENTRIES: usize = 512;

pub fn directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path.join("zor"));
    }
    if let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(home.join(".local/state/zor"));
    }
    anyhow::bail!("set HOME/XDG_STATE_HOME or specify --state-directory")
}
pub fn nonce() -> Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug)]
pub(crate) struct Busy;
impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("zor journal is busy; retry the operation ID")
    }
}
impl std::error::Error for Busy {}

pub struct Store {
    root: PathBuf,
    _lock: nix::fcntl::Flock<File>,
    journal: Journal,
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        anyhow::ensure!(root.is_absolute(), "zor state directory must be absolute");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        let meta = fs::symlink_metadata(root)?;
        anyhow::ensure!(
            meta.is_dir()
                && meta.uid() == nix::unistd::geteuid().as_raw()
                && meta.mode() & 0o077 == 0,
            "zor state directory must be an owned private directory"
        );
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("journal.lock"))?;
        private_file(&file)?;
        let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| -> anyhow::Error {
                if error == nix::errno::Errno::EWOULDBLOCK {
                    Busy.into()
                } else {
                    error.into()
                }
            })?;
        let path = root.join("journal.json");
        let journal = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => {
                private_file(&file)?;
                let mut bytes = Vec::new();
                file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
                anyhow::ensure!(bytes.len() <= MAX_BYTES, "zor journal byte limit exceeded");
                serde_json::from_slice::<Journal>(&bytes)
                    .context("invalid zor journal; refusing to replace it")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Journal::default(),
            Err(error) => return Err(error.into()),
        };
        journal.validate()?;
        anyhow::ensure!(
            serde_json::to_vec(&journal)?.len() + journal.check_reserve_bytes() <= MAX_BYTES,
            "zor journal check-result reserve exceeded"
        );
        remove_uncommitted(root)?;
        Ok(Self {
            root: root.into(),
            _lock: lock,
            journal,
        })
    }
    pub fn journal(&self) -> &Journal {
        &self.journal
    }
    /// Shared by runners from before durable intent through result publication. Recovery
    /// takes exclusive admission under the journal lock. Never unlink this inode.
    pub(super) fn check_runners(&self, exclusive: bool) -> Result<Option<nix::fcntl::Flock<File>>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(self.root.join("check-runners.lock"))?;
        private_file(&file)?;
        anyhow::ensure!(
            file.metadata()?.len() == 0,
            "check runner lock must be empty"
        );
        let operation = if exclusive {
            nix::fcntl::FlockArg::LockExclusiveNonblock
        } else {
            nix::fcntl::FlockArg::LockSharedNonblock
        };
        match nix::fcntl::Flock::lock(file, operation) {
            Ok(lock) => Ok(Some(lock)),
            Err((_, error)) if error == nix::errno::Errno::EWOULDBLOCK => Ok(None),
            Err((_, error)) => Err(error.into()),
        }
    }

    /// Persistent lock inodes prevent concurrent waiters from splitting admission state.
    /// The returned OS lock is released on normal return, signal termination, or process crash.
    pub(super) fn waiter(&self) -> Result<nix::fcntl::Flock<File>> {
        for slot in 0..32 {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(self.root.join(format!("waiter-{slot}.lock")))?;
            private_file(&file)?;
            anyhow::ensure!(file.metadata()?.len() == 0, "waiter lock must be empty");
            match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
                Ok(lock) => return Ok(lock),
                Err((_, error)) if error == nix::errno::Errno::EWOULDBLOCK => continue,
                Err((_, error)) => return Err(error.into()),
            }
        }
        anyhow::bail!("zor waiter limit reached (32 per state directory)")
    }

    /// The callback edits candidate state only. External effects must happen after a successful
    /// commit: validation, writing or synchronization can fail after this callback returns.
    pub fn transaction<T>(&mut self, change: impl FnOnce(&mut Journal) -> Result<T>) -> Result<T> {
        let mut next = self.journal.clone();
        let result = change(&mut next)?;
        next.generation = next
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("zor journal generation exhausted"))?;
        next.validate()?;
        let bytes = serde_json::to_vec(&next)?;
        anyhow::ensure!(
            bytes.len() + next.check_reserve_bytes() <= MAX_BYTES,
            "zor journal byte limit exceeded; forget unused tasks"
        );
        sync_ancestry(&self.root)?;
        let temporary = self.root.join(format!(".journal-{}.tmp", nonce()?));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let guard = Temporary {
            path: temporary,
            inode: file.metadata()?.ino(),
        };
        file.write_all(&bytes)?;
        file.sync_all().context("sync zor journal candidate")?;
        fs::rename(&guard.path, self.root.join("journal.json"))?;
        // Rename is already externally visible; a sync failure is an uncertain commit. In-memory
        // state must match it, and callers retry their stable operation ID rather than new input.
        self.journal = next;
        File::open(&self.root)?.sync_all().context(
            "zor journal was renamed but directory sync failed; commit durability uncertain",
        )?;
        Ok(result)
    }
}
// Persist links for user-owned ancestors too, including a newly created first-use hierarchy.
// The first ancestor owned by somebody else is synced for our child entry, then traversal stops.
fn sync_ancestry(root: &Path) -> Result<()> {
    let mut path = root;
    for _ in 0..256 {
        let directory = File::open(path)?;
        let owner = directory.metadata()?.uid();
        directory
            .sync_all()
            .with_context(|| format!("sync zor state ancestry {}", path.display()))?;
        if owner != nix::unistd::geteuid().as_raw() {
            return Ok(());
        }
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        path = parent;
    }
    anyhow::bail!("zor state directory ancestry exceeds limit")
}

fn remove_uncommitted(root: &Path) -> Result<()> {
    let mut candidates = Vec::new();
    for (index, entry) in fs::read_dir(root)?.enumerate() {
        anyhow::ensure!(
            index < MAX_STATE_ENTRIES,
            "too many entries in zor state directory"
        );
        let entry = entry?;
        let name = entry.file_name();
        let Some(nonce) = name
            .to_str()
            .and_then(|name| name.strip_prefix(".journal-"))
            .and_then(|name| name.strip_suffix(".tmp"))
        else {
            continue;
        };
        if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        anyhow::ensure!(
            candidates.len() < 16,
            "too many uncommitted zor journal files"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(entry.path())?;
        private_file(&file)?;
        anyhow::ensure!(
            file.metadata()?.len() <= MAX_BYTES as u64,
            "oversized uncommitted journal file"
        );
        candidates.push(entry.path());
    }
    if !candidates.is_empty() {
        for path in candidates {
            fs::remove_file(path)?;
        }
        File::open(root)?
            .sync_all()
            .context("sync discarded journal candidates")?;
    }
    Ok(())
}

fn private_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    anyhow::ensure!(
        meta.is_file()
            && meta.uid() == nix::unistd::geteuid().as_raw()
            && meta.mode() & 0o077 == 0
            && meta.nlink() == 1,
        "zor journal/lock must be a private owned regular file with one link"
    );
    Ok(())
}
struct Temporary {
    path: PathBuf,
    inode: u64,
}
impl Drop for Temporary {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|meta| meta.ino() == self.inode && meta.is_file())
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    fn root() -> PathBuf {
        let path = std::env::temp_dir().join(format!("zor-journal-{}", nonce().expect("nonce")));
        fs::create_dir(&path).expect("private test root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("permissions");
        path
    }
    #[test]
    fn supported_retained_directories_do_not_lock_out_journal_recovery() {
        let root = root();
        let mut store = Store::open(&root).expect("store");
        store.transaction(|_| Ok(())).expect("commit");
        for kind in ["worktree", "check"] {
            for index in 0..128 {
                fs::create_dir(root.join(format!("{kind}-{index:032x}")))
                    .expect("retained directory");
            }
        }
        let locks: Vec<_> = (0..32).map(|_| store.waiter().expect("waiter")).collect();
        drop(locks);
        drop(store.check_runners(false).expect("runner"));
        drop(store);
        let orphan = root.join(".journal-00000000000000000000000000000000.tmp");
        fs::write(&orphan, b"interrupted write").expect("orphan");
        fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600)).expect("private orphan");
        let mut recovered = Store::open(&root).expect("valid journal remains accessible");
        assert!(!orphan.exists());
        recovered
            .transaction(|_| Ok(()))
            .expect("recovery remains writable");
        assert_eq!(recovered.journal().generation, 2);
        assert!(
            root.join("worktree-0000000000000000000000000000007f")
                .is_dir()
        );
        assert!(root.join("check-0000000000000000000000000000007f").is_dir());
        drop(recovered);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn waiter_slots_are_bounded_private_and_released_without_unlinking() {
        let root = root();
        let store = Store::open(&root).expect("store");
        assert!(Store::open(&root).err().expect("busy").is::<Busy>());
        let mut locks = Vec::new();
        for _ in 0..32 {
            locks.push(store.waiter().expect("slot"));
        }
        assert!(store.waiter().is_err());
        let path = root.join("waiter-0.lock");
        let inode = fs::metadata(&path).expect("metadata").ino();
        locks.clear();
        let recovered = store.waiter().expect("released slot");
        assert_eq!(fs::metadata(&path).expect("retained inode").ino(), inode);
        assert_eq!(fs::metadata(&path).expect("mode").mode() & 0o777, 0o600);
        drop(recovered);
        fs::write(&path, b"unexpected contents").expect("fixture");
        assert!(store.waiter().is_err());
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn check_runner_admission_is_shared_until_all_runners_release() {
        let root = root();
        let store = Store::open(&root).expect("store");
        let first = store.check_runners(false).expect("lock").expect("runner");
        let second = store.check_runners(false).expect("lock").expect("runner");
        assert!(store.check_runners(true).expect("probe").is_none());
        drop(first);
        assert!(store.check_runners(true).expect("probe").is_none());
        drop(second);
        let exclusive = store.check_runners(true).expect("lock").expect("recovery");
        assert!(store.check_runners(false).expect("probe").is_none());
        drop(exclusive);
        let path = root.join("check-runners.lock");
        fs::write(&path, b"invalid").expect("fixture");
        assert!(store.check_runners(true).is_err());
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn transaction_reopens_and_invalid_update_preserves_previous_commit() {
        let root = root();
        let mut store = Store::open(&root).expect("open");
        assert!(Store::open(&root).is_err());
        store.transaction(|_| Ok(())).expect("commit");
        let before = fs::read(root.join("journal.json")).expect("snapshot");
        assert_eq!(store.journal().generation, 1);
        assert!(
            store
                .transaction(|journal| {
                    journal.version = 99;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(store.journal().version, 1);
        assert_eq!(
            fs::read(root.join("journal.json")).expect("unchanged"),
            before
        );
        assert_eq!(
            fs::metadata(root.join("journal.json"))
                .expect("mode")
                .mode()
                & 0o777,
            0o600
        );
        drop(store);
        let orphan = root.join(".journal-00000000000000000000000000000000.tmp");
        fs::write(&orphan, b"partial candidate").expect("interrupted candidate");
        fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600)).expect("private candidate");
        fs::write(root.join("notes"), b"preserve").expect("unrelated file");
        let store = Store::open(&root).expect("reopen");
        assert!(!orphan.exists());
        assert_eq!(
            fs::read(root.join("notes")).expect("unrelated preserved"),
            b"preserve"
        );
        assert_eq!(store.journal().generation, 1);
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }
    #[test]
    fn duplicate_records_are_rejected_before_relationship_validation() {
        let task = r#"{"id":"x","requested_runtime":"/tmp","title":"test","created_ms":1,"outcome":"open","attempt":"a"}"#;
        let source = format!(
            r#"{{"version":1,"generation":1,"tasks":{{"x":{task},"x":{task}}},"sessions":{{}},"attempts":{{}},"prompts":{{}}}}"#
        );
        let error = serde_json::from_str::<Journal>(&source).expect_err("duplicate record");
        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn unsafe_or_corrupted_evidence_is_rejected_without_replacement() {
        let root = root();
        let file = root.join("journal.json");
        fs::write(&file, b"corrupt evidence").expect("write");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("private");
        assert!(Store::open(&root).is_err());
        assert_eq!(fs::read(&file).expect("preserved"), b"corrupt evidence");
        fs::rename(&file, root.join("original")).expect("move own fixture");
        symlink(root.join("original"), &file).expect("symlink");
        assert!(Store::open(&root).is_err());
        fs::remove_file(&file).expect("unlink symlink");
        fs::hard_link(root.join("original"), &file).expect("hard link");
        assert!(Store::open(&root).is_err());
        fs::remove_file(&file).expect("unlink hard link");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("unsafe root");
        assert!(Store::open(&root).is_err());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
