//! Retained committed input bytes, materialized separately for each explicit check.
use super::{git, model::*, store::Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha1::Digest;
use std::collections::BTreeMap;
use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, Instant},
};

fn command(repo: &Path, argv: &[&str], deadline: Instant) -> Result<Vec<u8>> {
    git::run(
        repo,
        &argv.iter().map(OsStr::new).collect::<Vec<_>>(),
        deadline,
    )
}
fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn revision(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn path(value: &str) -> bool {
    super::artifact::valid_path(value)
        && !value.contains('\\')
        && value
            .split('/')
            .all(|part| !part.eq_ignore_ascii_case(".git"))
}
pub(super) fn valid_check_directory(value: &Path) -> bool {
    value
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix("check-"))
        .is_some_and(|nonce| nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn hash(kind: &str, bytes: &[u8], width: usize) -> String {
    let header = format!("{kind} {}\0", bytes.len());
    if width == 40 {
        let mut hash = sha1::Sha1::new();
        hash.update(header.as_bytes());
        hash.update(bytes);
        format!("{:x}", hash.finalize())
    } else {
        let mut hash = sha2::Sha256::new();
        hash.update(header.as_bytes());
        hash.update(bytes);
        format!("{:x}", hash.finalize())
    }
}
enum Node {
    File(bool, String),
    Directory(BTreeMap<String, Node>),
}
fn insert(tree: &mut BTreeMap<String, Node>, parts: &[&str], file: &SourceFile) -> Result<()> {
    let (name, rest) = parts.split_first().context("empty source path")?;
    if rest.is_empty() {
        anyhow::ensure!(
            !tree.contains_key(*name),
            "duplicate source path or file/directory conflict"
        );
        tree.insert(
            (*name).into(),
            Node::File(file.executable, file.blob.clone()),
        );
    } else {
        let node = tree
            .entry((*name).into())
            .or_insert_with(|| Node::Directory(BTreeMap::new()));
        let Node::Directory(children) = node else {
            anyhow::bail!("source file/directory conflict");
        };
        insert(children, rest, file)?;
    }
    Ok(())
}
fn tree_hash(tree: &BTreeMap<String, Node>, width: usize) -> Result<String> {
    let mut entries = Vec::new();
    for (name, node) in tree {
        let (mode, hash, mut order) = match node {
            Node::File(executable, blob) => (
                if *executable { "100755" } else { "100644" },
                blob.clone(),
                name.as_bytes().to_vec(),
            ),
            Node::Directory(children) => (
                "40000",
                tree_hash(children, width)?,
                name.as_bytes().to_vec(),
            ),
        };
        if matches!(node, Node::Directory(_)) {
            order.push(b'/');
        }
        entries.push((order, name, mode, hash));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut bytes = Vec::new();
    for (_, name, mode, hash) in entries {
        bytes.extend_from_slice(mode.as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        for pair in hash.as_bytes().as_chunks::<2>().0 {
            bytes.push(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?);
        }
    }
    Ok(hash("tree", &bytes, width))
}
pub(super) fn validate(source: &Source) -> Result<()> {
    anyhow::ensure!(
        revision(&source.revision) && oid(&source.commit) && oid(&source.tree),
        "invalid source revision"
    );
    anyhow::ensure!(
        source.files.len() <= 256
            && source
                .files
                .iter()
                .map(|file| file.bytes.len())
                .sum::<usize>()
                <= 262144,
        "source exceeds 256 files or 256 KiB"
    );
    anyhow::ensure!(
        source.commit_bytes.len() <= 65536
            && hash("commit", &source.commit_bytes, source.commit.len()) == source.commit
            && source
                .commit_bytes
                .starts_with(format!("tree {}\n", source.tree).as_bytes()),
        "source commit content does not match its identity/tree"
    );
    let mut tree = BTreeMap::new();
    let mut paths = std::collections::BTreeSet::new();
    let mut names = 0;
    for file in &source.files {
        names += file.path.len();
        anyhow::ensure!(
            file.blob.len() == source.commit.len()
                && hash("blob", &file.bytes, file.blob.len()) == file.blob,
            "source bytes do not match blob identity"
        );
        anyhow::ensure!(
            path(&file.path)
                && oid(&file.blob)
                && file.bytes.len() <= 65536
                && paths.insert(&file.path),
            "invalid source file"
        );
    }
    // Paths have already been bounded and validated above.
    for file in &source.files {
        insert(&mut tree, &file.path.split('/').collect::<Vec<_>>(), file)?;
    }
    anyhow::ensure!(
        tree_hash(&tree, source.commit.len())? == source.tree,
        "source paths/modes/blobs do not match tree identity"
    );
    anyhow::ensure!(names <= 32768, "source paths exceed 32 KiB");
    Ok(())
}
pub(super) fn summary(source: &Source) -> Value {
    json!({"id":source.id,"task":source.task,"attempt":source.attempt,"worktree":source.worktree,
        "revision":source.revision,"commit":source.commit,"tree":source.tree,"created_ms":source.created_ms,
        "created_generation":source.created_generation,"file_count":source.files.len(),
        "bytes":source.files.iter().map(|file| file.bytes.len()).sum::<usize>()})
}
fn view(store: &Store, id: &str) -> Result<Value> {
    let source = store
        .journal()
        .sources
        .get(id)
        .context("source not found")?;
    let files: Vec<_> = source
        .files
        .iter()
        .map(|file| {
            json!({"path":file.path,"executable":file.executable,
        "blob":file.blob,"bytes":file.bytes.len()})
        })
        .collect();
    Ok(json!({"generation":store.journal().generation,"source":summary(source),"files":files}))
}
pub fn inspect(root: &Path, id: &str) -> Result<Value> {
    view(&Store::open(root)?, id)
}
pub fn file(root: &Path, id: &str, path: &str) -> Result<Value> {
    let store = Store::open(root)?;
    let source = store
        .journal()
        .sources
        .get(id)
        .context("source not found")?;
    let file = source
        .files
        .iter()
        .find(|file| file.path == path)
        .context("source file not found")?;
    Ok(json!({"generation":store.journal().generation,"source":id,"file":file}))
}
fn metadata(bytes: &[u8]) -> Result<Vec<(String, bool, String, usize)>> {
    anyhow::ensure!(
        bytes.is_empty() || bytes.last() == Some(&0),
        "unterminated source tree"
    );
    let mut files = Vec::new();
    let mut total = 0usize;
    for entry in bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let split = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .context("source entry lacks path")?;
        let fields: Vec<_> = std::str::from_utf8(entry.get(..split).context("source metadata")?)?
            .split_ascii_whitespace()
            .collect();
        anyhow::ensure!(
            fields.len() == 4 && fields.get(1) == Some(&"blob"),
            "source supports regular files only; submodules are unsupported"
        );
        let mode = *fields.first().context("source mode")?;
        anyhow::ensure!(
            matches!(mode, "100644" | "100755"),
            "source symlinks and special modes are unsupported"
        );
        let blob = *fields.get(2).context("source blob")?;
        let size = fields.get(3).context("source size")?.parse::<usize>()?;
        let name = std::str::from_utf8(entry.get(split + 1..).context("source path")?)?;
        total = total.checked_add(size).context("source size overflow")?;
        anyhow::ensure!(
            path(name) && oid(blob) && size <= 65536 && total <= 262144 && files.len() < 256,
            "source path, file size, total size or count exceeds supported bounds"
        );
        files.push((name.into(), mode == "100755", blob.into(), size));
    }
    Ok(files)
}
pub fn collect(root: &Path, task_id: &str, id: &str, requested: String) -> Result<Value> {
    anyhow::ensure!(
        super::model::id(id) && revision(&requested),
        "invalid source ID or revision"
    );
    let mut store = Store::open(root)?;
    if let Some(source) = store.journal().sources.get(id) {
        anyhow::ensure!(
            source.task == task_id && source.revision == requested,
            "source ID already has different intent"
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
    let tree = store
        .journal()
        .launches
        .get(task_id)
        .and_then(|launch| launch.worktree.clone())
        .context("source collection needs a managed task with an owned worktree")?;
    let directory = super::worktree::launch_path(root, &mut store, &tree)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let commit = String::from_utf8(command(
        &directory,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{requested}^{{commit}}"),
        ],
        deadline,
    )?)?;
    let commit = commit.trim_end_matches('\n').to_owned();
    anyhow::ensure!(oid(&commit), "invalid resolved source commit");
    let tree_oid = String::from_utf8(command(
        &directory,
        &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
        deadline,
    )?)?;
    let mut source = Source {
        commit_bytes: command(&directory, &["cat-file", "commit", &commit], deadline)?,
        id: id.into(),
        task: task_id.into(),
        attempt: task.attempt,
        worktree: tree,
        revision: requested,
        commit: commit.clone(),
        tree: tree_oid.trim_end_matches('\n').into(),
        created_ms: super::now_ms()?,
        created_generation: store
            .journal()
            .generation
            .checked_add(1)
            .context("journal generation exhausted")?,
        files: Vec::new(),
    };
    for (path, executable, blob, size) in metadata(&command(
        &directory,
        &["ls-tree", "-r", "-z", "-l", "--full-tree", &commit],
        deadline,
    )?)? {
        let bytes = command(&directory, &["cat-file", "blob", &blob], deadline)?;
        anyhow::ensure!(bytes.len() == size, "source blob size changed");
        source.files.push(SourceFile {
            path,
            executable,
            blob,
            bytes,
        });
    }
    validate(&source)?;
    let identity = store
        .journal()
        .worktrees
        .get(&source.worktree)
        .and_then(|tree| tree.checkout_identity)
        .context("source worktree identity missing")?;
    let current = fs::symlink_metadata(&directory)?;
    anyhow::ensure!(
        current.is_dir() && (current.dev(), current.ino()) == identity,
        "source worktree identity changed during collection"
    );
    store.transaction(|journal| {
        journal.sources.insert(id.into(), source);
        Ok(())
    })?;
    view(&store, id)
}
pub(super) fn materialize(root: &Path, directory: &Path, source: &Source) -> Result<fs::File> {
    anyhow::ensure!(
        directory.parent() == Some(fs::canonicalize(root)?.as_path())
            && valid_check_directory(directory),
        "check source directory is outside its state root"
    );
    validate(source)?;
    fs::DirBuilder::new().mode(0o700).create(directory)?;
    for file in &source.files {
        let destination = directory.join(&file.path);
        let parent = destination.parent().context("source parent")?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&destination)?;
        output.write_all(&file.bytes)?;
        output.set_permissions(fs::Permissions::from_mode(if file.executable {
            0o700
        } else {
            0o600
        }))?;
    }
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK)
        .open(directory)?)
}
