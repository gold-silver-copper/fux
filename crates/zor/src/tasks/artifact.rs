//! Retained output bytes are evidence, never an agent success verdict.
use super::{
    model::{Artifact, Journal, TaskOutcome},
    store::Store,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

pub(super) fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && path.split('/').count() <= 32
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
fn view(store: &Store, id: &str) -> Result<Value> {
    let artifact = store
        .journal()
        .artifacts
        .get(id)
        .context("artifact not found")?;
    Ok(json!({"generation":store.journal().generation,"artifact":artifact}))
}
pub fn inspect(root: &Path, id: &str) -> Result<Value> {
    view(&Store::open(root)?, id)
}
fn signature(meta: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
pub(super) fn read(directory: File, path: &str) -> Result<Vec<u8>> {
    let mut directory = directory;
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        let mut file = crate::platform::files::child(&directory, part, parts.peek().is_some())?;
        if parts.peek().is_some() {
            directory = file;
            continue;
        }
        let before = file.metadata()?;
        anyhow::ensure!(
            before.is_file() && before.nlink() == 1 && before.len() <= 65536,
            "artifact must be a single-link regular file at most 64 KiB"
        );
        let mut bytes = Vec::new();
        (&mut file).take(65537).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= 65536
                && bytes.len() as u64 == before.len()
                && signature(&before) == signature(&file.metadata()?),
            "artifact changed during collection"
        );
        return Ok(bytes);
    }
    anyhow::bail!("artifact path empty")
}
pub(super) fn policy_sealed(journal: &Journal, task: &str) -> bool {
    journal
        .artifacts
        .values()
        .any(|artifact| artifact.task == task)
        || journal.checks.values().any(|check| check.task == task)
}
pub(super) fn requirements(journal: &Journal, task_id: &str) -> Vec<Value> {
    journal.tasks.get(task_id).into_iter().flat_map(|task| &task.required_artifacts)
        .map(|(name, path)| {
            let latest = journal.artifacts.values()
                .filter(|artifact| artifact.task == task_id
                    && journal.tasks.get(task_id).is_some_and(|task| artifact.attempt == task.attempt)
                    && artifact.requirement.as_ref() == Some(name))
                .max_by_key(|artifact| artifact.created_generation);
            json!({"name":name,"path":path,"status":if latest.is_some() {"collected"} else {"missing"},
                "artifact":latest.map(|artifact| &artifact.id)})
        }).collect()
}
pub fn require(root: &Path, task_id: &str, name: &str, path: String) -> Result<Value> {
    anyhow::ensure!(
        super::model::id(name) && valid_path(&path),
        "invalid required artifact name or relative path"
    );
    let mut store = Store::open(root)?;
    let task = store
        .journal()
        .tasks
        .get(task_id)
        .context("task not found")?;
    if let Some(existing) = task.required_artifacts.get(name) {
        anyhow::ensure!(
            existing == &path,
            "required artifact already has different path"
        );
        return super::inspect_journal(store.journal(), task_id);
    }
    anyhow::ensure!(task.outcome == TaskOutcome::Open, "task is no longer open");
    anyhow::ensure!(
        store.journal().launches.contains_key(task_id),
        "required artifacts need a managed task"
    );
    anyhow::ensure!(
        !policy_sealed(store.journal(), task_id),
        "artifact policy is sealed after first collection or check submission"
    );
    store.transaction(|journal| {
        journal
            .tasks
            .get_mut(task_id)
            .context("task disappeared")?
            .required_artifacts
            .insert(name.into(), path);
        Ok(())
    })?;
    super::inspect_journal(store.journal(), task_id)
}
pub fn collect(
    root: &Path,
    task_id: &str,
    id: &str,
    path: String,
    requirement: Option<String>,
) -> Result<Value> {
    anyhow::ensure!(
        super::model::id(id) && valid_path(&path),
        "invalid artifact ID or relative path"
    );
    let mut store = Store::open(root)?;
    if let Some(artifact) = store.journal().artifacts.get(id) {
        anyhow::ensure!(
            artifact.check.is_none(),
            "artifact ID belongs to a check capture"
        );
        anyhow::ensure!(
            artifact.task == task_id
                && artifact.path == path
                && artifact.requirement == requirement,
            "artifact ID already has different intent"
        );
        return view(&store, id);
    }
    anyhow::ensure!(
        !super::check_artifacts::reserved(store.journal(), id),
        "artifact ID is reserved by a check"
    );
    let task = store
        .journal()
        .tasks
        .get(task_id)
        .context("task not found")?
        .clone();
    anyhow::ensure!(task.outcome == TaskOutcome::Open, "task is no longer open");
    if let Some(name) = &requirement {
        anyhow::ensure!(
            task.required_artifacts.get(name) == Some(&path),
            "artifact does not match its required name and path"
        );
    }
    let launch = store
        .journal()
        .launches
        .get(task_id)
        .context("artifact collection requires a managed task")?
        .clone();
    if let Some(tree) = &launch.worktree {
        super::worktree::launch_path(root, &mut store, tree)?;
    }
    anyhow::ensure!(
        !store
            .journal()
            .worktrees
            .values()
            .any(|tree| tree.remove_force.is_some() && launch.cwd.starts_with(&tree.path)),
        "artifact cwd belongs to a worktree with removal intent"
    );
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK)
        .open(&launch.cwd)?;
    if let Some(tree) = &launch.worktree {
        let identity = store
            .journal()
            .worktrees
            .get(tree)
            .context("worktree disappeared")?
            .checkout_identity
            .as_ref()
            .context("worktree identity missing")?;
        let meta = directory.metadata()?;
        anyhow::ensure!(
            (meta.dev(), meta.ino()) == *identity,
            "artifact worktree identity changed"
        );
    }
    let bytes = read(directory, &path)?;
    let artifact = Artifact {
        check: None,
        source: None,
        id: id.into(),
        requirement,
        created_generation: store
            .journal()
            .generation
            .checked_add(1)
            .context("journal generation exhausted")?,
        task: task_id.into(),
        attempt: task.attempt,
        path,
        created_ms: super::now_ms()?,
        bytes,
    };
    store.transaction(|journal| {
        journal.artifacts.insert(id.into(), artifact);
        Ok(())
    })?;
    view(&store, id)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn exact_binary_limit_is_retained_and_larger_files_are_rejected() {
        let root = tempfile::tempdir().expect("private fixture");
        let path = root.path().join("file");
        let bytes: Vec<_> = (0..65536).map(|n| (n % 256) as u8).collect();
        std::fs::write(&path, &bytes).expect("write fixture");
        assert_eq!(
            read(File::open(root.path()).expect("root"), "file").expect("exact limit"),
            bytes
        );
        std::fs::write(&path, vec![0; 65537]).expect("write larger fixture");
        assert!(read(File::open(root.path()).expect("root"), "file").is_err());
    }
}
