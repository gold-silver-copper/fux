//! Zor-owned worktree creation intent and conservative reconciliation.
use super::{git, model::*, store::Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub struct Create {
    pub id: String,
    pub repo: PathBuf,
    pub branch: String,
    pub base: String,
}
fn command(repo: &Path, args: &[&str], deadline: Instant) -> Result<Vec<u8>> {
    git::run(
        repo,
        &args.iter().map(OsStr::new).collect::<Vec<_>>(),
        deadline,
    )
}
fn line(bytes: Vec<u8>) -> Result<String> {
    let value = String::from_utf8(bytes).context("git returned non-UTF-8 metadata")?;
    Ok(value.strip_suffix('\n').unwrap_or(&value).to_string())
}
fn identity(path: &Path) -> Result<(u64, u64)> {
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "worktree identity is not a directory"
    );
    Ok((meta.dev(), meta.ino()))
}
fn verify_owner(root: &Path, record: &Worktree) -> Result<()> {
    anyhow::ensure!(
        record.parent.parent() == Some(fs::canonicalize(root)?.as_path()),
        "worktree parent belongs to another state directory"
    );
    let meta = fs::symlink_metadata(&record.parent)?;
    anyhow::ensure!(
        meta.is_dir()
            && meta.uid() == nix::unistd::geteuid().as_raw()
            && meta.mode() & 0o077 == 0
            && (meta.dev(), meta.ino()) == (record.parent_dev, record.parent_ino),
        "owned worktree parent changed"
    );
    anyhow::ensure!(
        identity(&record.repo)? == (record.repo_dev, record.repo_ino)
            && identity(&record.common)? == (record.common_dev, record.common_ino),
        "repository identity changed"
    );
    let common = PathBuf::from(line(command(
        &record.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        Instant::now() + Duration::from_secs(2),
    )?)?);
    anyhow::ensure!(
        fs::canonicalize(common)? == record.common,
        "repository git directory changed"
    );
    Ok(())
}
fn inspect_store(store: &Store, id: &str) -> Result<Value> {
    Ok(
        json!({"generation":store.journal().generation,"worktree":store.journal().worktrees.get(id).context("worktree not found")?}),
    )
}
pub fn inspect(root: &Path, id: &str) -> Result<Value> {
    inspect_store(&Store::open(root)?, id)
}
pub fn list(root: &Path) -> Result<Value> {
    let store = Store::open(root)?;
    Ok(
        json!({"generation":store.journal().generation,"worktrees":store.journal().worktrees.values().collect::<Vec<_>>()}),
    )
}
pub fn create(root: &Path, request: Create) -> Result<Value> {
    anyhow::ensure!(
        super::model::id(&request.id)
            && !request.branch.is_empty()
            && request.branch.len() <= 128
            && !request.branch.starts_with('-')
            && !request.branch.chars().any(char::is_control)
            && !request.base.is_empty()
            && request.base.len() <= 256
            && !request.base.chars().any(char::is_control),
        "invalid worktree intent"
    );
    let requested_repo = std::path::absolute(&request.repo)?;
    let mut store = Store::open(root)?;
    if let Some(existing) = store.journal().worktrees.get(&request.id) {
        anyhow::ensure!(
            existing.requested_repo == requested_repo
                && existing.branch == request.branch
                && existing.base == request.base,
            "worktree ID already has different intent"
        );
        if matches!(
            existing.phase,
            WorktreePhase::Ready | WorktreePhase::Removed
        ) {
            return inspect_store(&store, &request.id);
        }
    } else {
        let deadline = Instant::now() + Duration::from_secs(6);
        let input = fs::canonicalize(&requested_repo)?;
        let repo = fs::canonicalize(line(command(
            &input,
            &["rev-parse", "--show-toplevel"],
            deadline,
        )?)?)?;
        let literal_branch = line(command(
            &repo,
            &["check-ref-format", "--branch", &request.branch],
            deadline,
        )?)?;
        anyhow::ensure!(
            literal_branch == request.branch,
            "branch must be a literal name, not checkout shorthand"
        );
        let commit = line(command(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{}^{{commit}}", request.base),
            ],
            deadline,
        )?)?;
        let common = fs::canonicalize(line(command(
            &repo,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            deadline,
        )?)?)?;
        let (repo_dev, repo_ino) = identity(&repo)?;
        let (common_dev, common_ino) = identity(&common)?;
        let parent = fs::canonicalize(root)?.join(format!("worktree-{}", super::store::nonce()?));

        let record = Worktree {
            id: request.id.clone(),
            requested_repo,
            repo,
            common,
            parent: parent.clone(),
            path: parent.join("tree"),
            branch: request.branch,
            base: request.base,
            commit,
            repo_dev,
            repo_ino,
            common_dev,
            common_ino,
            parent_dev: 0,
            parent_ino: 0,
            checkout_identity: None,
            phase: WorktreePhase::Allocating,
            remove_force: None,
            problem: None,
        };
        store.transaction(|journal| {
            journal.worktrees.insert(record.id.clone(), record);
            Ok(())
        })?;
    }
    let allocating = store
        .journal()
        .worktrees
        .get(&request.id)
        .context("worktree missing")?
        .clone();
    if allocating.phase == WorktreePhase::Allocating {
        anyhow::ensure!(
            allocating.parent.parent() == Some(fs::canonicalize(root)?.as_path())
                && identity(&allocating.repo)? == (allocating.repo_dev, allocating.repo_ino)
                && identity(&allocating.common)? == (allocating.common_dev, allocating.common_ino),
            "reserved worktree location or repository changed"
        );
        // Persist the reserved path before mkdir. A crash never leaves an unbounded
        // unrecorded directory. Existing paths are not silently claimed on retry.
        fs::DirBuilder::new().mode(0o700).create(&allocating.parent)
            .context("allocate reserved worktree parent; an existing directory requires explicit recovery")?;
        fs::File::open(&allocating.parent)?.sync_all()?;
        let (dev, ino) = identity(&allocating.parent)?;
        store.transaction(|journal| {
            let record = journal
                .worktrees
                .get_mut(&request.id)
                .context("worktree missing")?;
            record.parent_dev = dev;
            record.parent_ino = ino;
            record.phase = WorktreePhase::Prepared;
            Ok(())
        })?;
    }
    let record = store
        .journal()
        .worktrees
        .get(&request.id)
        .context("worktree missing")?
        .clone();
    if record.phase == WorktreePhase::Prepared {
        verify_owner(root, &record)?;
        anyhow::ensure!(
            !record.path.try_exists()?,
            "worktree destination already exists"
        );
        store.transaction(|journal| {
            journal
                .worktrees
                .get_mut(&request.id)
                .context("worktree missing")?
                .phase = WorktreePhase::Creating;
            Ok(())
        })?;
        let result = git::run(
            &record.repo,
            &[
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("-b"),
                OsStr::new(&record.branch),
                OsStr::new("--"),
                record.path.as_os_str(),
                OsStr::new(&record.commit),
            ],
            Instant::now() + Duration::from_secs(10),
        );
        if let Err(error) = result {
            problem(&mut store, &request.id, &error)?;
            return Err(error.context("worktree creation outcome uncertain; reconcile the same ID"));
        }
    }
    reconcile_store(root, &mut store, &request.id)
}
fn problem(store: &mut Store, id: &str, error: &anyhow::Error) -> Result<()> {
    let message: String = error.to_string().chars().take(128).collect();
    store.transaction(|journal| {
        let record = journal.worktrees.get_mut(id).context("worktree missing")?;
        if !matches!(
            record.phase,
            WorktreePhase::Ready | WorktreePhase::Removing | WorktreePhase::Removed
        ) {
            record.phase = WorktreePhase::Uncertain;
        }
        record.problem = Some(message);
        Ok(())
    })
}
pub fn reconcile(root: &Path, id: &str) -> Result<Value> {
    reconcile_store(root, &mut Store::open(root)?, id)
}

/// Removal authority comes only from a confirmed owned checkout. A recorded
/// submission is reconciled without replaying destructive git operations.
pub fn remove(root: &Path, id: &str, force: bool) -> Result<Value> {
    let mut store = Store::open(root)?;
    let record = store
        .journal()
        .worktrees
        .get(id)
        .context("worktree not found")?
        .clone();
    if let Some(previous) = record.remove_force {
        anyhow::ensure!(
            previous == force,
            "worktree removal has different force intent"
        );
        return reconcile_store(root, &mut store, id);
    }
    anyhow::ensure!(
        record.phase == WorktreePhase::Ready,
        "only ready owned worktrees may be removed"
    );
    anyhow::ensure!(!store.journal().checks.values().any(|check|
        check.phase != CheckPhase::Finished && check.cwd.starts_with(&record.path)),
        "worktree has an unfinished check; removal refused");
    // Cancellation is not process termination; pending and uncertain launches
    // also retain use of their cwd. Literal cwd launches count too.
    anyhow::ensure!(
        !store.journal().launches.values().any(|launch| {
            launch.phase != LaunchPhase::Closed
                && (launch.worktree.as_deref() == Some(id) || launch.cwd.starts_with(&record.path))
        }),
        "worktree has an active or unresolved managed launch; stop and reconcile it first"
    );
    launch_path(root, &mut store, id)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut checked = std::collections::BTreeSet::new();
    for session in store.journal().sessions.values() {
        // Live routes retain their own availability checks. Only a confirmed
        // closed managed identity can discharge an unavailable alias below.
        if !checked.insert(&session.target) {
            continue;
        }
        let closed = store.journal().sessions.values().any(|other| {
            other.target.identity() == session.target.identity()
                && other
                    .launch
                    .as_ref()
                    .and_then(|id| store.journal().launches.get(id))
                    .is_some_and(|launch| launch.phase == LaunchPhase::Closed)
        });
        if closed {
            continue;
        }
        super::worktree_use::check(&session.target, &record.path, deadline)?;
    }
    // Observation/adoption is not required for a pane to be using this checkout.
    super::worktree_use::unadopted(store.journal(), &record.path, deadline)?;
    if !force {
        let status = command(
            &record.path,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
            ],
            Instant::now() + Duration::from_secs(3),
        )?;
        anyhow::ensure!(
            status.is_empty(),
            "worktree is dirty; force is required to discard files"
        );
    }
    store.transaction(|journal| {
        let record = journal.worktrees.get_mut(id).context("worktree missing")?;
        record.phase = WorktreePhase::Removing;
        record.remove_force = Some(force);
        record.problem = None;
        Ok(())
    })?;
    let mut args = vec![OsStr::new("worktree"), OsStr::new("remove")];
    if force {
        args.push(OsStr::new("--force"));
    }
    args.extend([OsStr::new("--"), record.path.as_os_str()]);
    if let Err(error) = git::run(
        &record.repo,
        &args,
        Instant::now() + Duration::from_secs(10),
    ) {
        problem(&mut store, id, &error)?;
        return Err(error.context("worktree removal outcome uncertain; reconcile the same ID"));
    }
    reconcile_store(root, &mut store, id)
}

fn reconcile_removal(root: &Path, store: &mut Store, record: &Worktree) -> Result<Value> {
    let result = (|| -> Result<()> {
        verify_owner(root, record)?;
        match fs::symlink_metadata(&record.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!("checkout still exists; removal remains uncertain"),
        }
        let data = command(
            &record.repo,
            &["worktree", "list", "--porcelain", "-z"],
            Instant::now() + Duration::from_secs(3),
        )?;
        anyhow::ensure!(
            !data
                .split(|b| *b == 0)
                .any(|field| field.strip_prefix(b"worktree ")
                    == Some(record.path.as_os_str().as_encoded_bytes())),
            "worktree registration remains; removal is incomplete"
        );
        // Retain the private parent and branch. Do not prune unrelated git
        // registrations or recursively delete anything during recovery.
        store.transaction(|journal| {
            let tree = journal
                .worktrees
                .get_mut(&record.id)
                .context("worktree missing")?;
            tree.phase = WorktreePhase::Removed;
            tree.problem = None;
            Ok(())
        })
    })();
    if let Err(error) = result {
        problem(store, &record.id, &error)?;
        return Err(
            error.context("worktree removal reconciliation incomplete; no remove was repeated")
        );
    }
    inspect_store(store, &record.id)
}
/// Validate under the caller's journal lock before reserving or sending a launch.
pub(super) fn launch_path(root: &Path, store: &mut Store, id: &str) -> Result<PathBuf> {
    let record = store
        .journal()
        .worktrees
        .get(id)
        .context("worktree not found")?;
    anyhow::ensure!(
        record.phase == WorktreePhase::Ready,
        "worktree is not ready; reconcile creation first"
    );
    reconcile_store(root, store, id)?;
    Ok(store
        .journal()
        .worktrees
        .get(id)
        .context("worktree missing")?
        .path
        .clone())
}
fn reconcile_store(root: &Path, store: &mut Store, id: &str) -> Result<Value> {
    let record = store
        .journal()
        .worktrees
        .get(id)
        .context("worktree not found")?
        .clone();
    if record.phase == WorktreePhase::Removed {
        return inspect_store(store, id);
    }
    if record.phase == WorktreePhase::Removing {
        return reconcile_removal(root, store, &record);
    }
    if record.phase == WorktreePhase::Allocating {
        if !record.parent.try_exists()? {
            return inspect_store(store, id);
        }
        // Explicit reconciliation may pin an empty private directory at the exact
        // durably reserved path. Nonempty or replaced locations are never claimed.
        let meta = fs::symlink_metadata(&record.parent)?;
        anyhow::ensure!(
            meta.is_dir()
                && meta.uid() == nix::unistd::geteuid().as_raw()
                && meta.mode() & 0o077 == 0
                && fs::read_dir(&record.parent)?.next().is_none(),
            "reserved parent is not an empty private directory"
        );
        let mut pinned = record.clone();
        pinned.parent_dev = meta.dev();
        pinned.parent_ino = meta.ino();
        verify_owner(root, &pinned)?;
        fs::File::open(&record.parent)?.sync_all()?;
        store.transaction(|journal| {
            let record = journal.worktrees.get_mut(id).context("worktree missing")?;
            record.parent_dev = pinned.parent_dev;
            record.parent_ino = pinned.parent_ino;
            record.phase = WorktreePhase::Prepared;
            record.problem = None;
            Ok(())
        })?;
        return inspect_store(store, id);
    }
    if record.phase == WorktreePhase::Prepared {
        return inspect_store(store, id);
    }
    let result = (|| -> Result<()> {
        verify_owner(root, &record)?;
        let data = command(
            &record.repo,
            &["worktree", "list", "--porcelain", "-z"],
            Instant::now() + Duration::from_secs(3),
        )?;
        let path = record
            .path
            .to_str()
            .context("worktree path must be UTF-8")?;
        let mut matched = false;
        let mut found = 0;
        let mut head = false;
        let mut branch = false;
        let mut unavailable = false;
        for field in data.split(|b| *b == 0) {
            if let Some(value) = field.strip_prefix(b"worktree ") {
                matched = value == path.as_bytes();
                if matched {
                    found += 1;
                }
            } else if matched
                && (field == b"locked"
                    || field.starts_with(b"locked ")
                    || field == b"prunable"
                    || field.starts_with(b"prunable "))
            {
                unavailable = true;
            } else if matched && field.strip_prefix(b"HEAD ") == Some(record.commit.as_bytes()) {
                head = true;
            } else if matched
                && field.strip_prefix(b"branch ")
                    == Some(format!("refs/heads/{}", record.branch).as_bytes())
            {
                branch = true;
            }
        }
        anyhow::ensure!(
            found == 1 && branch && !unavailable && (record.phase == WorktreePhase::Ready || head),
            "worktree registration does not match creation intent"
        );
        anyhow::ensure!(
            fs::canonicalize(&record.path)? == record.path,
            "worktree path changed or is a symlink"
        );
        let checkout_identity = identity(&record.path)?;
        anyhow::ensure!(
            record
                .checkout_identity
                .is_none_or(|expected| expected == checkout_identity),
            "worktree checkout directory changed"
        );
        let common = fs::canonicalize(line(command(
            &record.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            Instant::now() + Duration::from_secs(2),
        )?)?)?;
        anyhow::ensure!(
            common == record.common,
            "worktree belongs to another repository"
        );
        if record.phase != WorktreePhase::Ready {
            command(
                &record.path,
                &["diff", "--quiet", "HEAD", "--"],
                Instant::now() + Duration::from_secs(3),
            )?;
        }
        if record.phase != WorktreePhase::Ready || record.problem.is_some() {
            store.transaction(|journal| {
                let record = journal.worktrees.get_mut(id).context("worktree missing")?;
                record.phase = WorktreePhase::Ready;
                record.checkout_identity = Some(checkout_identity);
                record.problem = None;
                Ok(())
            })?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        problem(store, id, &error)?;
        return Err(error.context("worktree reconciliation incomplete; no add was repeated"));
    }
    inspect_store(store, id)
}
