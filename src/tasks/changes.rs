//! Bounded retained git change observations; these are not immutable source verification.
use super::{git, model::*, store::Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};

fn command(path: &Path, argv: &[&str], deadline: Instant) -> Result<Vec<u8>> {
    git::run(
        path,
        &argv.iter().map(OsStr::new).collect::<Vec<_>>(),
        deadline,
    )
}
fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(super) fn validate(changes: &Changes) -> Result<()> {
    anyhow::ensure!(
        oid(&changes.head) && oid(&changes.base_commit),
        "invalid change commit ID"
    );
    anyhow::ensure!(
        changes.committed.len() + changes.working.len() <= 256,
        "changed file count exceeds 256"
    );
    let mut size = 0;
    for (entries, working) in [(&changes.committed, false), (&changes.working, true)] {
        let mut paths = std::collections::BTreeSet::new();
        for file in entries {
            size += file.path.len();
            anyhow::ensure!(
                !file.path.is_empty()
                    && file.path.len() <= 4096
                    && !file.path.contains(&0)
                    && file
                        .path
                        .split(|b| *b == b'/')
                        .all(|p| !p.is_empty() && p != b"." && p != b"..")
                    && paths.insert(&file.path),
                "invalid or duplicate changed path"
            );
            anyhow::ensure!(
                if working {
                    file.status.len() == 2
                        && file.status.bytes().all(|b| b" MTADRCU?!".contains(&b))
                        && file.status != "  "
                } else {
                    file.status.len() == 1 && file.status.bytes().all(|b| b"ADMTUXB".contains(&b))
                },
                "invalid change status"
            );
        }
    }
    anyhow::ensure!(size <= 32768, "changed filenames exceed 32 KiB");
    Ok(())
}
fn parse(bytes: &[u8], working: bool) -> Result<Vec<ChangedFile>> {
    anyhow::ensure!(
        bytes.is_empty() || bytes.last() == Some(&0),
        "unterminated git change data"
    );
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut fields = bytes
        .strip_suffix(&[0])
        .context("missing change terminator")?
        .split(|b| *b == 0);
    let mut files = Vec::new();
    while let Some(field) = fields.next() {
        let (status, path) = if working {
            anyhow::ensure!(field.get(2) == Some(&b' '), "invalid git status framing");
            (
                field.get(..2).context("missing status")?,
                field.get(3..).context("missing path")?,
            )
        } else {
            (field, fields.next().context("missing git diff path")?)
        };
        files.push(ChangedFile {
            status: String::from_utf8(status.to_vec())?,
            path: path.to_vec(),
        });
        anyhow::ensure!(files.len() <= 256, "changed file count exceeds 256");
    }
    Ok(files)
}
fn view(store: &Store, id: &str) -> Result<Value> {
    Ok(
        json!({"generation":store.journal().generation,"changes":store.journal().changes.get(id).context("change evidence not found")?}),
    )
}
pub fn inspect(root: &Path, id: &str) -> Result<Value> {
    view(&Store::open(root)?, id)
}
pub fn collect(root: &Path, task_id: &str, id: &str) -> Result<Value> {
    anyhow::ensure!(super::model::id(id), "invalid change evidence ID");
    let mut store = Store::open(root)?;
    if let Some(existing) = store.journal().changes.get(id) {
        anyhow::ensure!(
            existing.task == task_id,
            "change evidence ID belongs to another task"
        );
        return view(&store, id);
    }
    let task = store
        .journal()
        .tasks
        .get(task_id)
        .context("task not found")?
        .clone();
    anyhow::ensure!(task.outcome == TaskOutcome::Open, "task is no longer open");
    let tree_id = store
        .journal()
        .launches
        .get(task_id)
        .and_then(|launch| launch.worktree.clone())
        .context("change collection needs a managed task with an owned worktree")?;
    let path = super::worktree::launch_path(root, &mut store, &tree_id)?;
    let tree = store
        .journal()
        .worktrees
        .get(&tree_id)
        .context("worktree disappeared")?
        .clone();
    let deadline = Instant::now() + Duration::from_secs(6);
    let head_argv = ["rev-parse", "--verify", "HEAD^{commit}"];
    let head_bytes = command(&path, &head_argv, deadline)?;
    let head = String::from_utf8(head_bytes.clone())?
        .trim_end_matches('\n')
        .to_owned();
    anyhow::ensure!(oid(&head), "invalid HEAD commit ID");
    let diff_argv = [
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--ignore-submodules=none",
        "--name-status",
        "-z",
        &tree.commit,
        &head,
        "--",
    ];
    let status_argv = [
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignored=no",
        "--no-renames",
        "--ignore-submodules=none",
    ];
    let committed = command(&path, &diff_argv, deadline)?;
    let working = command(&path, &status_argv, deadline)?;
    anyhow::ensure!(
        command(&path, &head_argv, deadline)? == head_bytes
            && command(&path, &diff_argv, deadline)? == committed
            && command(&path, &status_argv, deadline)? == working,
        "git evidence changed during collection"
    );
    let meta = fs::symlink_metadata(&path)?;
    anyhow::ensure!(
        meta.is_dir() && Some((meta.dev(), meta.ino())) == tree.checkout_identity,
        "worktree identity changed during collection"
    );
    let changes = Changes {
        id: id.into(),
        task: task_id.into(),
        attempt: task.attempt,
        worktree: tree_id,
        base_commit: tree.commit,
        head,
        created_ms: super::now_ms()?,
        created_generation: store
            .journal()
            .generation
            .checked_add(1)
            .context("journal generation exhausted")?,
        committed: parse(&committed, false)?,
        working: parse(&working, true)?,
    };
    store.transaction(|journal| {
        journal.changes.insert(id.into(), changes);
        Ok(())
    })?;
    view(&store, id)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn byte_paths_and_nul_framing_do_not_depend_on_utf8_or_lines() {
        let working = parse(b"?? raw-\xff\nname\0 M tab\tname\0", true).expect("raw status");
        let mut files = working.iter();
        let first = files.next().expect("first path");
        assert_eq!(first.path, b"raw-\xff\nname");
        assert_eq!(first.status, "??");
        assert_eq!(files.next().expect("second path").path, b"tab\tname");
        assert!(parse(b"?? missing terminator", true).is_err());
        assert!(parse(b"M\0", false).is_err());
        assert!(parse(b" Mbroken\0", true).is_err());
        let committed = parse(b"A\0new\0D\0old\0", false).expect("paired records");
        assert_eq!(committed.len(), 2);
    }
}
