//! Retained git change evidence in a disposable owned task checkout.
use crate::support::{
    local::{Root, completed},
    process,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

fn changes(value: &Value) -> Result<BTreeMap<Vec<u8>, String>> {
    value
        .as_array()
        .context("change array")?
        .iter()
        .map(|item| {
            Ok((
                serde_json::from_value(item["path"].clone())?,
                item["status"].as_str().context("change status")?.into(),
            ))
        })
        .collect()
}
pub(super) fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new("zchanges-rs-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let listing = completed(&root.control(), json!({"id":1,"command":"list"}))?;
    let instance = listing["instance"].as_str().context("instance")?;
    let cli = |args: &[&str], ok: bool| -> Result<Value> {
        let mut command = root.command(zor);
        command.args(args);
        let result = process::output(command, Duration::from_secs(12), 1024 * 1024)?;
        ensure!(
            result.status.success() == ok,
            "CLI {args:?}: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        if ok {
            Ok(serde_json::from_slice(&result.stdout)?)
        } else {
            Ok(Value::Null)
        }
    };
    let git = |path: &Path, args: &[&str]| -> Result<String> {
        let mut command = root.command(Path::new("/usr/bin/git"));
        command.arg("-C").arg(path).args(args);
        let result = process::output(command, Duration::from_secs(5), 1024 * 1024)?;
        ensure!(
            result.status.success(),
            "fixture git {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(String::from_utf8(result.stdout)?)
    };
    let repo = root.path().join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["config", "user.name", "Fixture"])?;
    git(&repo, &["config", "user.email", "fixture@example.invalid"])?;
    fs::write(repo.join("source"), "baseline\n")?;
    fs::write(repo.join(".gitignore"), "ignored\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "baseline"])?;
    let base = git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
    let tree = cli(
        &[
            "worktree",
            "create",
            "tree",
            "--repo",
            repo.to_str().context("repo")?,
            "--branch",
            "worker",
        ],
        true,
    )?;
    let cwd = Path::new(tree["worktree"]["path"].as_str().context("worktree path")?);
    cli(
        &[
            "task",
            "start",
            "worker",
            "--title",
            "changes",
            "--instance",
            instance,
            "--workspace",
            "default",
            "--worktree",
            "tree",
            "--",
            "/bin/cat",
        ],
        true,
    )?;
    let clean = cli(&["task", "changes-collect", "worker", "clean"], true)?["changes"].clone();
    ensure!(
        clean["head"] == base && clean["committed"] == json!([]) && clean["working"] == json!([]),
        "clean evidence: {clean}"
    );
    fs::rename(cwd.join("source"), cwd.join("renamed"))?;
    git(cwd, &["add", "-A"])?;
    git(cwd, &["commit", "-m", "rename"])?;
    let head = git(cwd, &["rev-parse", "HEAD"])?.trim().to_owned();
    fs::write(cwd.join("renamed"), "modified\n")?;
    fs::write(cwd.join("staged"), "staged\n")?;
    git(cwd, &["add", "staged"])?;
    fs::write(cwd.join("staged"), "modified after stage\n")?;
    let raw_name = "raw-\t\nname";
    fs::write(cwd.join(raw_name), "raw filename\n")?;
    fs::write(cwd.join("ignored"), "excluded\n")?;
    let marker = root.path().join("must-not-run");
    let helper = root.path().join("helper");
    fs::write(
        &helper,
        format!("#!/bin/sh\ntouch \"{}\"\n", marker.display()),
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
    git(
        cwd,
        &[
            "config",
            "core.fsmonitor",
            helper.to_str().context("helper")?,
        ],
    )?;
    git(
        cwd,
        &[
            "config",
            "diff.external",
            helper.to_str().context("helper")?,
        ],
    )?;
    git(cwd, &["replace", &base, &head])?;
    let observed = cli(&["task", "changes-collect", "worker", "edited"], true)?["changes"].clone();
    ensure!(!marker.exists(), "metadata invoked fsmonitor/external diff");
    ensure!(
        observed["base_commit"] == base && observed["head"] == head,
        "wrong commit evidence"
    );
    ensure!(
        changes(&observed["committed"])?
            == BTreeMap::from([
                (b"source".to_vec(), "D".into()),
                (b"renamed".to_vec(), "A".into())
            ]),
        "committed evidence: {observed}"
    );
    ensure!(
        changes(&observed["working"])?
            == BTreeMap::from([
                (b"renamed".to_vec(), " M".into()),
                (b"staged".to_vec(), "AM".into()),
                (raw_name.as_bytes().to_vec(), "??".into())
            ]),
        "working evidence: {observed}"
    );
    fs::write(cwd.join("renamed"), "newer contents\n")?;
    ensure!(
        cli(&["task", "changes-collect", "worker", "edited"], true)?["changes"] == observed,
        "replay changed evidence"
    );
    let result = cli(&["task", "result", "worker"], true)?;
    ensure!(
        result["changed_files"]["evidence"] == observed
            && result["changed_files"]["atomic_snapshot"] == false
            && result["verification"]["status"] == "unverified",
        "result misrepresented evidence"
    );
    let journal = root.path().join("state/zor/journal.json");
    let before = fs::read(&journal)?;
    for index in 0..257 {
        fs::write(cwd.join(format!("excess-{index}")), b"")?;
    }
    cli(&["task", "changes-collect", "worker", "too-many"], false)?;
    ensure!(
        fs::read(&journal)? == before,
        "oversized evidence changed journal"
    );
    for index in 0..257 {
        fs::remove_file(cwd.join(format!("excess-{index}")))?;
    }
    cli(&["task", "cancel", "worker"], true)?;
    ensure!(
        cli(&["task", "changes-inspect", "edited"], true)?["changes"] == observed,
        "cancel lost evidence"
    );
    ensure!(
        cli(&["task", "changes-collect", "worker", "edited"], true)?["changes"] == observed,
        "cancel rejected retained replay"
    );
    cli(
        &["task", "changes-collect", "worker", "after-cancel"],
        false,
    )?;
    server.finish()?;
    println!(
        "PASS retained git changes, raw filenames, bounds, replacement-ref isolation and disabled fsmonitor/external diff"
    );
    Ok(())
}
