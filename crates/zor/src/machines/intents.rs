//! Controller-owned intent evidence, persisted before dispatch and never used as replay authority.
use crate::tasks::supervise::Expected;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 512 * 1024;
const MAX_INTENTS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeIntent {
    pub machine: String,
    pub endpoint: String,
    pub service_instance: String,
    pub operation: String,
    pub fux_instance: String,
    pub expected: Expected,
}
impl ResumeIntent {
    fn validate(&self) -> Result<()> {
        let hex = |value: &str, length| {
            value.len() == length && value.bytes().all(|b| b.is_ascii_hexdigit())
        };
        let identity = |value: &str| {
            !value.is_empty()
                && value.len() <= 256
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        };
        let route = if self.machine == "local" {
            self.endpoint.strip_prefix("local:").is_some_and(|socket| {
                Path::new(socket).is_absolute()
                    && socket.len() <= 4096
                    && !socket.chars().any(char::is_control)
            })
        } else {
            hex(&self.machine, 32) && hex(&self.endpoint, 64)
        };
        ensure!(
            route
                && identity(&self.service_instance)
                && identity(&self.fux_instance)
                && crate::tasks::model::id(&self.operation)
                && self.operation != self.expected.task
                && crate::tasks::model::id(&self.expected.task)
                && crate::tasks::model::id(&self.expected.attempt)
                && crate::tasks::model::id(&self.expected.session),
            "invalid controller resume intent"
        );
        Ok(())
    }
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Log {
    intents: Vec<ResumeIntent>,
}

pub fn path(catalog: &Path) -> Result<PathBuf> {
    ensure!(catalog.is_absolute(), "catalog path must be absolute");
    let mut name = catalog
        .file_name()
        .context("catalog filename")?
        .to_os_string();
    name.push(".resume-intents.json");
    Ok(catalog.with_file_name(name))
}
fn private(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == nix::unistd::geteuid().as_raw()
            && metadata.permissions().mode() & 0o077 == 0,
        "intent log must be a private owned regular file"
    );
    Ok(())
}
pub fn list(path: &Path) -> Result<Vec<ResumeIntent>> {
    ensure!(path.is_absolute(), "intent log path must be absolute");
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    local_ipc::ensure_private_directory(path.parent().context("intent parent")?)?;
    private(&file)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "intent log byte limit");
    let log: Log = serde_json::from_slice(&bytes)?;
    ensure!(log.intents.len() <= MAX_INTENTS, "intent record limit");
    let mut keys = std::collections::BTreeSet::new();
    for intent in &log.intents {
        intent.validate()?;
        ensure!(
            keys.insert((&intent.machine, &intent.operation)),
            "duplicate controller operation"
        );
    }
    Ok(log.intents)
}
pub fn record(path: &Path, intent: ResumeIntent) -> Result<()> {
    intent.validate()?;
    ensure!(path.is_absolute(), "intent log path must be absolute");
    let parent = path.parent().context("intent parent")?;
    local_ipc::ensure_private_directory(parent)?;
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(PathBuf::from(lock))?;
    private(&file)?;
    ensure!(file.metadata()?.len() == 0, "intent lock must be empty");
    let _lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|(_, error)| anyhow::anyhow!("controller intent log busy: {error}"))?;
    let mut intents = list(path)?;
    if let Some(existing) = intents
        .iter()
        .find(|old| old.machine == intent.machine && old.operation == intent.operation)
    {
        ensure!(
            existing.endpoint == intent.endpoint
                && existing.expected.task == intent.expected.task
                && existing.fux_instance == intent.fux_instance,
            "retained controller operation has different intent; inspect before acting"
        );
        // Keep the original pre-dispatch selection even after a deliberate retry reads a new attempt.
        return Ok(());
    }
    ensure!(
        intents.len() < MAX_INTENTS,
        "controller intent capacity reached; archive reviewed records before further resume operations"
    );
    intents.push(intent);
    let bytes = serde_json::to_vec(&Log { intents })?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "intent log byte limit");
    let candidate = parent.join(format!(
        ".resume-intents-{}.tmp",
        local_ipc::random_token()?
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&candidate)?;
    let temporary = Temporary(candidate);
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary.0, path)?;
    File::open(parent)?
        .sync_all()
        .context("intent saved but directory sync failed; inspect before dispatch")?;
    Ok(())
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn intent() -> ResumeIntent {
        ResumeIntent {
            machine: "a".repeat(32),
            endpoint: "b".repeat(64),
            service_instance: "service-one".into(),
            operation: "resume-one".into(),
            fux_instance: "fux-one".into(),
            expected: Expected {
                task: "worker".into(),
                attempt: "attempt-one".into(),
                session: "session-one".into(),
                target: crate::tasks::model::Target {
                    runtime: "/remote/fux".into(),
                    instance: "old-fux".into(),
                    workspace: "agent".into(),
                    stream: 1,
                    pane: 2,
                    pid: Some(3),
                    origin: None,
                },
            },
        }
    }
    #[test]
    fn intent_survives_reopen_and_retry_preserves_original_selection() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        let path = path(&root.path().join("machines.json"))?;
        let original = intent();
        record(&path, original.clone())?;
        let retained = fs::read(&path)?;
        let loaded = list(&path)?;
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.first().context("intent")?.expected,
            original.expected
        );
        let mut retry = original.clone();
        retry.expected.attempt = "attempt-two".into();
        retry.service_instance = "service-two".into();
        record(&path, retry)?;
        assert_eq!(fs::read(&path)?, retained);
        for field in ["endpoint", "task", "fux"] {
            let mut changed = original.clone();
            match field {
                "endpoint" => changed.endpoint = "c".repeat(64),
                "task" => changed.expected.task = "other".into(),
                _ => changed.fux_instance = "replacement".into(),
            }
            assert!(record(&path, changed).is_err());
            assert_eq!(fs::read(&path)?, retained);
        }
        Ok(())
    }
    #[test]
    fn local_intents_require_local_socket_identity() -> Result<()> {
        let mut local = intent();
        local.machine = "local".into();
        assert!(local.validate().is_err());
        local.endpoint = "local:/private/runtime/zor/control.sock".into();
        local.validate()?;
        local.endpoint = "local:relative.sock".into();
        assert!(local.validate().is_err());
        local.endpoint = "local:/tmp/control\n.sock".into();
        assert!(local.validate().is_err());
        Ok(())
    }

    #[test]
    fn malformed_and_symlinked_logs_block_recording() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        let path = path(&root.path().join("machines.json"))?;
        record(&path, intent())?;
        fs::write(&path, b"{invalid")?;
        assert!(record(&path, intent()).is_err());
        assert_eq!(fs::read(&path)?, b"{invalid");
        fs::remove_file(&path)?;
        let other = root.path().join("other");
        fs::write(&other, b"untouched")?;
        std::os::unix::fs::symlink(&other, &path)?;
        assert!(record(&path, intent()).is_err());
        assert_eq!(fs::read(other)?, b"untouched");
        Ok(())
    }
}
