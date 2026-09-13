//! Opt-in JSON-lines observations of durable transitions. Never a journal or
//! authority source. Records deliberately exclude argv, paths, text and errors.
use super::model::*;
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::OnceLock,
};

const MAX_LOG_BYTES: u64 = 1024 * 1024;
const MAX_RECORD_BYTES: usize = 4096;
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("ZOR_DIAGNOSTICS").is_some_and(|value| value == "1"))
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Event<'a> {
    Launch {
        id: &'a str,
        phase: &'a LaunchPhase,
        instance: &'a str,
        pane: Option<u32>,
        pid: Option<u32>,
        stop_requested: bool,
        problem: bool,
    },
    Attempt {
        id: &'a str,
        session: &'a str,
        state: &'a AttemptState,
    },
    Delivery {
        id: &'a str,
        phase: &'a Delivery,
        wait: &'a WaitOutcome,
        operation: Option<u64>,
        bytes_written: Option<usize>,
        released: bool,
    },
    Worktree {
        id: &'a str,
        phase: &'a WorktreePhase,
        remove_force: Option<bool>,
    },
    Recovery {
        id: &'a str,
        phase: &'a LaunchPhase,
        succeeded: bool,
    },
}

struct Log(nix::fcntl::Flock<File>);
impl Log {
    fn open(root: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("diagnostics.jsonl"))?;
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file()
                && metadata.uid() == nix::unistd::geteuid().as_raw()
                && metadata.mode() & 0o077 == 0
                && metadata.nlink() == 1,
            "diagnostic file is not private"
        );
        let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| error)?;
        Ok(Self(lock))
    }
    fn record(
        &mut self,
        generation: u64,
        directory_synced: Option<bool>,
        event: Event<'_>,
    ) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "pid":std::process::id(), "generation":generation, "directory_synced":directory_synced, "event":event
        }))?;
        bytes.push(b'\n');
        anyhow::ensure!(
            bytes.len() <= MAX_RECORD_BYTES,
            "diagnostic record exceeds bound"
        );
        if self.0.metadata()?.len().saturating_add(bytes.len() as u64) > MAX_LOG_BYTES {
            self.0.set_len(0)?;
        }
        self.0.write_all(&bytes)?;
        Ok(())
    }
}

/// Called after journal rename, including when the following directory sync is
/// uncertain. Failure to open/write diagnostics cannot alter the operation result.
pub(super) fn journal(root: &Path, before: &Journal, after: &Journal, directory_synced: bool) {
    if !enabled() {
        return;
    }
    if let Ok(mut log) = Log::open(root) {
        changes(&mut log, before, after, directory_synced);
    }
}

fn changes(log: &mut Log, before: &Journal, after: &Journal, directory_synced: bool) {
    let mut emit = |event| {
        let _ = log.record(after.generation, Some(directory_synced), event);
    };
    for (id, launch) in &after.launches {
        if before.launches.get(id).is_none_or(|old| {
            old.phase != launch.phase
                || old.stop_requested != launch.stop_requested
                || old.pane != launch.pane
                || old.session != launch.session
                || old.problem.is_some() != launch.problem.is_some()
        }) {
            let pid = launch
                .session
                .as_ref()
                .and_then(|session| after.sessions.get(session))
                .and_then(|session| session.target.pid);
            emit(Event::Launch {
                id,
                phase: &launch.phase,
                instance: &launch.instance,
                pane: launch.pane,
                pid,
                stop_requested: launch.stop_requested,
                problem: launch.problem.is_some(),
            });
        }
    }
    for (id, attempt) in &after.attempts {
        if before
            .attempts
            .get(id)
            .is_none_or(|old| old.state != attempt.state)
        {
            emit(Event::Attempt {
                id,
                session: &attempt.session,
                state: &attempt.state,
            });
        }
    }
    for (id, prompt) in &after.prompts {
        let evidence = |prompt: &Prompt| {
            prompt.receipt.as_ref().map(|receipt| {
                (
                    receipt.operation,
                    receipt.bytes_written,
                    receipt.input_sequence,
                )
            })
        };
        if before.prompts.get(id).is_none_or(|old| {
            old.delivery != prompt.delivery
                || old.wait != prompt.wait
                || old.released != prompt.released
                || evidence(old) != evidence(prompt)
        }) {
            emit(Event::Delivery {
                id,
                phase: &prompt.delivery,
                wait: &prompt.wait,
                operation: prompt.receipt.as_ref().map(|receipt| receipt.operation),
                bytes_written: prompt.receipt.as_ref().map(|receipt| receipt.bytes_written),
                released: prompt.released,
            });
        }
    }
    for (id, tree) in &after.worktrees {
        if before
            .worktrees
            .get(id)
            .is_none_or(|old| old.phase != tree.phase || old.remove_force != tree.remove_force)
        {
            emit(Event::Worktree {
                id,
                phase: &tree.phase,
                remove_force: tree.remove_force,
            });
        }
    }
}

pub(super) fn recovery(root: &Path, journal: &Journal, id: &str, succeeded: bool) {
    if !enabled() {
        return;
    }
    if let Some(launch) = journal.launches.get(id)
        && let Ok(mut log) = Log::open(root)
    {
        // This is a fresh observation of the loaded journal, not a new commit.
        let _ = log.record(
            journal.generation,
            None,
            Event::Recovery {
                id,
                phase: &launch.phase,
                succeeded,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_bounded_records_and_contention() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let mut log = Log::open(root.path())?;
        assert!(Log::open(root.path()).is_err());
        let event = || Event::Recovery {
            id: "launch-1",
            phase: &LaunchPhase::Prepared,
            succeeded: false,
        };
        log.record(7, None, event())?;
        let path = root.path().join("diagnostics.jsonl");
        let record: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(record.get("generation"), Some(&serde_json::json!(7)));
        assert!(
            record
                .get("directory_synced")
                .is_some_and(serde_json::Value::is_null)
        );
        assert_eq!(
            record
                .get("event")
                .and_then(|event| event.get("kind"))
                .and_then(serde_json::Value::as_str),
            Some("recovery")
        );
        assert_eq!(std::fs::metadata(&path)?.mode() & 0o077, 0);
        log.0.set_len(MAX_LOG_BYTES)?;
        log.record(8, Some(false), event())?;
        let bytes = std::fs::read(&path)?;
        assert!(bytes.len() <= MAX_RECORD_BYTES);
        let record: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(record.get("generation"), Some(&serde_json::json!(8)));
        assert_eq!(
            record.get("directory_synced"),
            Some(&serde_json::json!(false))
        );
        let oversized = "x".repeat(MAX_RECORD_BYTES);
        assert!(
            log.record(
                9,
                None,
                Event::Recovery {
                    id: &oversized,
                    phase: &LaunchPhase::Prepared,
                    succeeded: false
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path)?, bytes);
        drop(log);
        assert!(Log::open(root.path()).is_ok());
        Ok(())
    }

    #[test]
    fn refuses_symlink_and_shared_permissions() -> anyhow::Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir()?;
        let target = root.path().join("target");
        std::fs::write(&target, b"unchanged")?;
        let path = root.path().join("diagnostics.jsonl");
        symlink(&target, &path)?;
        assert!(Log::open(root.path()).is_err());
        assert_eq!(std::fs::read(&target)?, b"unchanged");
        std::fs::remove_file(&path)?;
        std::fs::write(&path, b"")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(Log::open(root.path()).is_err());
        Ok(())
    }
}
