//! Owned worktree intent/recovery in a disposable repository, independent of fux.
use crate::support::{
    local::until,
    process::{self, Guard},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn dirs(path: &Path, prefix: &str) -> Result<BTreeSet<PathBuf>> {
    fs::read_dir(path)?
        .filter_map(|e| match e {
            Ok(e) if e.file_name().to_string_lossy().starts_with(prefix) => Some(Ok(e.path())),
            Ok(_) => None,
            Err(e) => Some(Err(e.into())),
        })
        .collect()
}
fn path(v: &Value, key: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(
        v[key].as_str().with_context(|| format!("missing {key}"))?,
    ))
}
fn error_contains(v: Value, text: &str) -> Result<()> {
    ensure!(
        v.as_str().context("error")?.contains(text),
        "expected {text}: {v}"
    );
    Ok(())
}
pub(super) fn filter(root: &Path) -> Result<()> {
    fs::write(
        root.join("filter-started"),
        nix::unistd::getpgrp().to_string(),
    )?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("filter-release").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "filter input bound");
    std::io::stdout().write_all(&bytes)?;
    fs::write(root.join("filter-finished"), "done")?;
    Ok(())
}
pub(super) fn run(zor: &Path) -> Result<()> {
    let temp = tempfile::Builder::new()
        .prefix("zwt-rs-")
        .tempdir_in("/tmp")?;
    let root = temp.path();
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    let command = |exe: &Path| {
        let mut c = Command::new(exe);
        c.env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", root)
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        c
    };
    let git = |cwd: &Path, args: &[&str]| -> Result<Vec<u8>> {
        let mut c = command(Path::new("/usr/bin/git"));
        c.arg("-C").arg(cwd).args(args);
        let r = process::output(c, Duration::from_secs(8), 1024 * 1024)?;
        ensure!(
            r.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        Ok(r.stdout)
    };
    let worktree = |args: &[&str], ok: bool| -> Result<Value> {
        let mut c = command(zor);
        c.arg("worktree").args(args);
        let r = process::output(c, Duration::from_secs(18), 1024 * 1024)?;
        ensure!(
            r.status.success() == ok,
            "worktree {args:?}: {} {}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
        if ok {
            Ok(serde_json::from_slice(&r.stdout)?)
        } else {
            Ok(String::from_utf8(r.stderr)?.into())
        }
    };
    let repo_s = repo.to_str().context("repo")?;
    let create = |name: &str, branch: &str| -> Vec<String> {
        ["create", name, "--repo", repo_s, "--branch", branch]
            .map(String::from)
            .to_vec()
    };
    let new = |name: &str, branch: &str, ok| {
        worktree(
            &create(name, branch)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ok,
        )
    };
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["config", "user.name", "Fixture"])?;
    git(&repo, &["config", "user.email", "fixture@example.invalid"])?;
    fs::write(repo.join("hello.txt"), "tracked baseline\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "fixture"])?;
    let head = git(&repo, &["rev-parse", "HEAD"])?;
    let first = new("one", "one", true)?;
    let record = &first["worktree"];
    let checkout = path(record, "path")?;
    ensure!(
        record["phase"] == "ready"
            && fs::read_to_string(checkout.join("hello.txt"))? == "tracked baseline\n",
        "ready checkout"
    );
    ensure!(
        fs::metadata(checkout.parent().context("parent")?)?
            .permissions()
            .mode()
            & 0o777
            == 0o700,
        "private parent"
    );
    ensure!(new("one", "one", true)? == first, "creation replay");
    new("one", "different", false)?;
    git(&repo, &["switch", "-c", "previous"])?;
    git(&repo, &["switch", "main"])?;
    git(&repo, &["branch", "-D", "previous"])?;
    let before = worktree(&["list"], true)?;
    new("shorthand", "@{-1}", false)?;
    ensure!(
        worktree(&["list"], true)? == before
            && git(&repo, &["branch", "--list", "previous"])?.is_empty(),
        "shorthand mutated state"
    );
    ensure!(
        git(&repo, &["rev-parse", "HEAD"])? == head
            && git(&repo, &["branch", "--show-current"])? == b"main\n"
            && git(&repo, &["status", "--porcelain"])?.is_empty(),
        "main repo changed"
    );
    new("collision", "one", false)?;
    let collision = worktree(&["inspect", "collision"], true)?["worktree"].clone();
    ensure!(collision["phase"] == "uncertain", "collision phase");
    let state_dir = root.join("state/zor").canonicalize()?;
    let parents = dirs(&state_dir, "worktree-")?;
    new("collision", "one", false)?;
    ensure!(
        dirs(&state_dir, "worktree-")? == parents && !path(&collision, "path")?.exists(),
        "collision retried allocation"
    );
    let parked = checkout.with_file_name("parked");
    fs::rename(&checkout, &parked)?;
    symlink(&repo, &checkout)?;
    worktree(&["reconcile", "one"], false)?;
    ensure!(
        !worktree(&["inspect", "one"], true)?["worktree"]["problem"].is_null(),
        "replacement problem absent"
    );
    fs::remove_file(&checkout)?;
    fs::rename(&parked, &checkout)?;
    ensure!(
        worktree(&["reconcile", "one"], true)?["worktree"]["problem"].is_null(),
        "recovery problem"
    );
    fs::write(checkout.join("hello.txt"), "worker edit\n")?;
    git(&checkout, &["add", "."])?;
    git(&checkout, &["commit", "-m", "worker"])?;
    ensure!(
        worktree(&["reconcile", "one"], true)?["worktree"]["phase"] == "ready"
            && git(&repo, &["rev-parse", "HEAD"])? == head,
        "worker commit recovery"
    );
    let executable = std::env::current_exe()?;
    // Git runs this configuration through its shell; quote both owned paths literally.
    let quote = |p: &Path| -> Result<String> {
        Ok(format!(
            "'{}'",
            p.to_str().context("filter path")?.replace('\'', "'\\''")
        ))
    };
    git(
        &repo,
        &[
            "config",
            "filter.fixture.smudge",
            &format!(
                "{} fixture-worker worktree-filter {}",
                quote(&executable)?,
                quote(root)?
            ),
        ],
    )?;
    fs::write(repo.join(".gitattributes"), "hello.txt filter=fixture\n")?;
    git(&repo, &["add", ".gitattributes"])?;
    git(&repo, &["commit", "-m", "filter fixture"])?;
    let marker = root.join("filter-started");
    let release = root.join("filter-release");
    let finished = root.join("filter-finished");
    let mut c = command(zor);
    c.arg("worktree")
        .args(create("crash", "crash"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut caller = Guard(c.spawn()?);
    let crash = (|| -> Result<()> {
        until(Duration::from_secs(5), || {
            ensure!(
                caller.0.try_wait()?.is_none(),
                "caller exited before filter"
            );
            Ok(marker.exists().then_some(()))
        })?;
        caller.0.kill()?;
        process::wait(&mut caller.0, Duration::from_secs(3))?;
        let pending = worktree(&["inspect", "crash"], true)?["worktree"].clone();
        ensure!(pending["phase"] == "creating", "durable creating absent");
        worktree(&["reconcile", "crash"], false)?;
        ensure!(
            worktree(&["inspect", "crash"], true)?["worktree"]["phase"] == "uncertain",
            "uncertain boundary"
        );
        fs::write(&release, "")?;
        until(Duration::from_secs(6), || {
            Ok(finished.exists().then_some(()))
        })?;
        let parents = dirs(&state_dir, "worktree-")?;
        let mut retry = command(zor);
        retry.arg("worktree").args(create("crash", "crash"));
        process::output(retry, Duration::from_secs(18), 1024 * 1024)?;
        let after = worktree(&["inspect", "crash"], true)?["worktree"].clone();
        ensure!(
            matches!(after["phase"].as_str(), Some("ready" | "uncertain"))
                && after["path"] == pending["path"]
                && dirs(&state_dir, "worktree-")? == parents,
            "interrupted add replay"
        );
        ensure!(
            String::from_utf8(git(&repo, &["worktree", "list", "--porcelain"])?)?
                .matches("branch refs/heads/crash\n")
                .count()
                <= 1,
            "duplicate checkout"
        );
        Ok(())
    })();
    // Always release and reap the caller, then verify the owned filter group has gone.
    fs::write(&release, "")?;
    if caller.0.try_wait()?.is_none() {
        caller.0.kill()?;
        process::wait(&mut caller.0, Duration::from_secs(3))?;
    }
    let cleanup = (|| -> Result<()> {
        let end = Instant::now() + Duration::from_secs(6);
        while marker.exists() && !finished.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        if marker.exists() {
            let group = fs::read_to_string(&marker)?.parse::<i32>()?;
            ensure!(
                group > 1 && group != nix::unistd::getpgrp().as_raw(),
                "foreign group"
            );
            loop {
                match nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None) {
                    Err(nix::errno::Errno::ESRCH) => break,
                    Ok(()) => {}
                    Err(e) => return Err(e.into()),
                }
                ensure!(
                    Instant::now() < end,
                    "fixture git group outlived bounded filter"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        Ok(())
    })();
    cleanup?;
    crash?;
    let completed = new("post-git", "post-git", true)?;
    let journal = state_dir.join("journal.json");
    let read = || -> Result<Value> { Ok(serde_json::from_slice(&fs::read(&journal)?)?) };
    let write = |s: &Value| -> Result<()> { Ok(fs::write(&journal, serde_json::to_vec(s)?)?) };
    let mut state = read()?;
    state["worktrees"]["post-git"]["phase"] = json!("creating");
    write(&state)?;
    let restored = worktree(&["reconcile", "post-git"], true)?;
    ensure!(
        restored["worktree"] == completed["worktree"]
            && new("post-git", "post-git", true)? == restored,
        "post-git recovery"
    );
    let saved = fs::read(&journal)?;
    let template = state["worktrees"]["post-git"].clone();
    state = read()?;
    for i in 0..128 - state["worktrees"].as_object().context("worktrees")?.len() {
        let id = format!("reserved-{i}");
        let mut item = template.clone();
        item["id"] = json!(id);
        item["phase"] = json!("allocating");
        item["parent_dev"] = json!(0);
        item["parent_ino"] = json!(0);
        let parent = state_dir.join(format!("worktree-{i:032x}"));
        item["parent"] = json!(parent);
        item["path"] = json!(parent.join("tree"));
        state["worktrees"][id] = item;
    }
    write(&state)?;
    let directories = dirs(&state_dir, "")?;
    let capacity = (|| -> Result<()> {
        new("over-limit", "over-limit", false)?;
        ensure!(
            dirs(&state_dir, "")? == directories,
            "full journal allocated directory"
        );
        Ok(())
    })();
    fs::write(&journal, &saved)?;
    capacity?;
    state = serde_json::from_slice(&saved)?;
    let mut item = template;
    for (k, v) in [
        ("id", json!("allocate")),
        ("branch", json!("allocate")),
        ("phase", json!("allocating")),
        ("parent_dev", json!(0)),
        ("parent_ino", json!(0)),
        ("checkout_identity", Value::Null),
    ] {
        item[k] = v;
    }
    let parent = state_dir.join("worktree-ffffffffffffffffffffffffffffffff");
    item["parent"] = json!(parent);
    item["path"] = json!(parent.join("tree"));
    state["worktrees"]["allocate"] = item;
    write(&state)?;
    fs::create_dir(&parent)?;
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
    fs::write(parent.join("unrelated"), "preserve")?;
    worktree(&["reconcile", "allocate"], false)?;
    ensure!(
        fs::read_to_string(parent.join("unrelated"))? == "preserve",
        "unrelated allocation data"
    );
    fs::remove_file(parent.join("unrelated"))?;
    ensure!(
        worktree(&["reconcile", "allocate"], true)?["worktree"]["phase"] == "prepared"
            && new("allocate", "allocate", true)?["worktree"]["phase"] == "ready",
        "allocation recovery"
    );
    let removal_head = git(&repo, &["rev-parse", "HEAD"])?;
    let clean = new("remove-clean", "remove-clean", true)?;
    let clean_path = path(&clean["worktree"], "path")?;
    let sibling = clean_path.parent().context("parent")?.join("sibling");
    fs::write(&sibling, "preserve")?;
    let removed = worktree(&["remove", "remove-clean"], true)?;
    ensure!(
        removed["worktree"]["phase"] == "removed"
            && !clean_path.exists()
            && fs::read_to_string(&sibling)? == "preserve"
            && String::from_utf8(git(&repo, &["branch", "--list", "remove-clean"])?)?.trim()
                == "remove-clean",
        "clean remove authority"
    );
    ensure!(
        worktree(&["remove", "remove-clean"], true)? == removed
            && new("remove-clean", "remove-clean", true)? == removed,
        "removal replay"
    );
    worktree(&["remove", "remove-clean", "--force"], false)?;
    ensure!(
        worktree(&["inspect", "remove-clean"], true)? == removed,
        "force changed intent"
    );
    let dirty = new("remove-dirty", "remove-dirty", true)?;
    let dirty_path = path(&dirty["worktree"], "path")?;
    fs::write(dirty_path.join("hello.txt"), "modified")?;
    let before = fs::read(&journal)?;
    error_contains(worktree(&["remove", "remove-dirty"], false)?, "dirty")?;
    ensure!(
        fs::read(&journal)? == before
            && fs::read_to_string(dirty_path.join("hello.txt"))? == "modified",
        "dirty remove changed data"
    );
    git(&dirty_path, &["restore", "hello.txt"])?;
    fs::write(dirty_path.join("untracked"), "preserve")?;
    worktree(&["remove", "remove-dirty"], false)?;
    fs::remove_file(dirty_path.join("untracked"))?;
    fs::write(repo.join(".git/info/exclude"), "ignored\n")?;
    fs::write(dirty_path.join("ignored"), "preserve")?;
    worktree(&["remove", "remove-dirty"], false)?;
    ensure!(
        worktree(&["remove", "remove-dirty", "--force"], true)?["worktree"]["phase"] == "removed"
            && !dirty_path.exists(),
        "force remove failed"
    );
    let guarded = new("remove-guard", "remove-guard", true)?;
    let guarded_path = path(&guarded["worktree"], "path")?;
    let parked = guarded_path.with_file_name("saved");
    fs::rename(&guarded_path, &parked)?;
    symlink(&repo, &guarded_path)?;
    let replaced = (|| -> Result<()> {
        worktree(&["remove", "remove-guard", "--force"], false)?;
        ensure!(
            fs::read_to_string(repo.join("hello.txt"))? == "tracked baseline\n",
            "symlink redirected remove"
        );
        Ok(())
    })();
    fs::remove_file(&guarded_path)?;
    fs::rename(&parked, &guarded_path)?;
    replaced?;
    fs::rename(&guarded_path, &parked)?;
    fs::create_dir(&guarded_path)?;
    fs::copy(parked.join(".git"), guarded_path.join(".git"))?;
    fs::write(guarded_path.join("unrelated"), "replacement data")?;
    let replaced = (|| -> Result<()> {
        error_contains(
            worktree(&["remove", "remove-guard", "--force"], false)?,
            "directory changed",
        )?;
        ensure!(
            fs::read_to_string(guarded_path.join("unrelated"))? == "replacement data"
                && parked.join("hello.txt").exists(),
            "replacement data removed"
        );
        Ok(())
    })();
    fs::remove_file(guarded_path.join(".git"))?;
    fs::remove_file(guarded_path.join("unrelated"))?;
    fs::remove_dir(&guarded_path)?;
    fs::rename(&parked, &guarded_path)?;
    replaced?;
    state = read()?;
    state["worktrees"]["remove-guard"]["phase"] = json!("removing");
    state["worktrees"]["remove-guard"]["remove_force"] = json!(false);
    state["worktrees"]["remove-guard"]["problem"] = Value::Null;
    write(&state)?;
    error_contains(
        worktree(&["remove", "remove-guard"], false)?,
        "no remove was repeated",
    )?;
    ensure!(
        guarded_path.join("hello.txt").exists(),
        "destructive replay"
    );
    new("remove-guard", "remove-guard", false)?;
    ensure!(guarded_path.exists(), "create removed checkout");
    git(
        &repo,
        &[
            "worktree",
            "remove",
            "--",
            guarded_path.to_str().context("guarded path")?,
        ],
    )?;
    let recovered = worktree(&["reconcile", "remove-guard"], true)?;
    ensure!(
        recovered["worktree"]["phase"] == "removed"
            && worktree(&["remove", "remove-guard"], true)? == recovered
            && git(&repo, &["rev-parse", "HEAD"])? == removal_head,
        "remove recovery"
    );
    println!(
        "PASS owned worktree creation, guarded removal, dirty/force policy and interrupted intent recovery"
    );
    Ok(())
}
