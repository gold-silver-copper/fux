//! Worktree ownership and live process directory refusal through the shared API.
use super::service_fixture::*;
use crate::support::{local::until, process};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink, path::PathBuf, time::Duration};
pub struct Managed {
    pub launch: Value,
    pub managed: Value,
    pub tree: PathBuf,
    pub tree_request: Value,
}
pub fn run(h: &Harness<'_>) -> Result<Managed> {
    macro_rules! api {
        ($v:expr) => {
            h.api($v, true)?
        };
        ($v:expr,false) => {
            h.api($v, false)?
        };
    }
    let root = h.root.path();
    let repo = root.join("worktree-repo");
    fs::create_dir(&repo)?;
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
    ] {
        git(h, &repo, &args)?;
    }
    fs::write(repo.join("file.txt"), b"baseline")?;
    git(h, &repo, &["add", "."])?;
    git(h, &repo, &["commit", "-m", "fixture"])?;
    let tree_request = json!({"action":"worktree-create","id":"api-tree","repo":repo,"branch":"api-tree","base":"HEAD"});
    let tree = api!(tree_request.clone());
    ensure!(
        tree["worktree"]["phase"] == "ready"
            && api!(tree_request.clone()) == tree
            && api!(json!({"action":"worktree-inspect","id":"api-tree"})) == tree
            && api!(json!({"action":"worktree-reconcile","id":"api-tree"})) == tree
            && api!(json!({"action":"worktree-list"}))["worktrees"][0]["id"] == "api-tree",
        "worktree replay"
    );
    api!(json!({"action":"worktree-list","unexpected":true}), false);
    ensure!(
        api!(json!({"action":"list"}))["tasks"] == json!([]),
        "initial tasks"
    );
    api!(json!({"action":"list","unknown":true}), false);
    h.api_instance(json!({"action":"list"}), false, &json!("stale"))?;
    let launch = json!({"action":"start","id":"api-managed","title":"managed API fixture","instance":h.handle["instance"],"workspace":h.handle["workspace"],"worktree":"api-tree","argv":["/bin/sh","-c","pwd -P > worker-cwd; exec /bin/cat"]});
    api!(altered(&launch, "cwd", json!(root)), false);
    api!(without(&launch, "worktree")?, false);
    api!(altered(&launch, "worktree", json!("missing")), false);
    ensure!(
        api!(json!({"action":"list"}))["tasks"] == json!([]),
        "invalid launch created task"
    );
    let original = fs::read(&h.journal)?;
    let mut incomplete: Value = serde_json::from_slice(&original)?;
    incomplete["worktrees"]["api-tree"]["phase"] = json!("creating");
    h.write(&incomplete)?;
    let rejected = api!(launch.clone(), false);
    ensure!(
        contains(&rejected, "not ready")
            && serde_json::from_slice::<Value>(&fs::read(&h.journal)?)? == incomplete,
        "incomplete tree launch"
    );
    fs::write(&h.journal, &original)?;
    let tree_path = PathBuf::from(s(&tree["worktree"]["path"])?);
    let saved = tree_path.with_file_name("saved");
    fs::rename(&tree_path, &saved)?;
    symlink(&repo, &tree_path)?;
    api!(launch.clone(), false);
    ensure!(
        api!(json!({"action":"list"}))["tasks"] == json!([]),
        "symlink launch created task"
    );
    fs::remove_file(&tree_path)?;
    fs::rename(&saved, &tree_path)?;
    let pane = h.fux(
        "split",
        json!({"axis":"horizontal","cwd":tree_path,"argv":["/bin/cat"]}),
    )?["pane"]
        .clone();
    api!(json!({"action":"worktree-reconcile","id":"api-tree"}));
    let before = fs::read(&h.journal)?;
    refuse(h, &tree_path, "active session", Some(&before))?;
    let manager = root.join("fux/manager.sock");
    let parked = root.join("fux/parked-manager");
    fs::rename(&manager, &parked)?;
    let r = api!(
        json!({"action":"worktree-remove","id":"api-tree","force":true}),
        false
    );
    ensure!(
        contains(&r, "manager unavailable")
            && tree_path.exists()
            && fs::read(&h.journal)? == before,
        "unavailable manager removed tree"
    );
    fs::rename(&parked, &manager)?;
    let adopted = api!(h.adopt("tree-adopted", "adopted tree worker", &pane));
    ensure!(
        api!(json!({"action":"adapter-status","id":"tree-adopted"}))["availability"]
            == "not-configured",
        "adopted adapter"
    );
    api!(
        json!({"action":"adapter-status","id":"tree-adopted","unknown":true}),
        false
    );
    refuse(h, &tree_path, "active session", None)?;
    let original = fs::read(&h.journal)?;
    let mut unreachable: Value = serde_json::from_slice(&original)?;
    unreachable["sessions"][s(&adopted["session"]["id"])?]["target"]["runtime"] =
        json!(root.join("missing-runtime"));
    h.write(&unreachable)?;
    let r = api!(
        json!({"action":"worktree-remove","id":"api-tree","force":true}),
        false
    );
    ensure!(
        contains(&r, "use is uncertain") && tree_path.exists(),
        "unreachable target removed tree"
    );
    fs::write(&h.journal, &original)?;
    api!(json!({"action":"forget","id":"tree-adopted"}));
    h.fux("kill", json!({"pane":pane}))?;
    h.closed(&pane)?;
    let marker = root.join("moved-cwd");
    let moved_argv = json!([
        "/bin/sh",
        "-c",
        "cd \"$1\" && pwd -P > \"$2\" && exec /bin/cat",
        "fixture",
        tree_path,
        marker
    ]);
    let moved = api!(
        json!({"action":"start","id":"moved-managed","title":"changed cwd","instance":h.handle["instance"],"workspace":h.handle["workspace"],"cwd":root,"argv":moved_argv})
    );
    marked(&marker, path(&tree_path)?)?;
    ensure!(
        moved["launch"]["cwd"] == json!(root.canonicalize()?),
        "creation cwd"
    );
    refuse(
        h,
        &tree_path,
        "current directory of an active session",
        None,
    )?;
    h.stop_task("moved-managed", 42)?;
    let vanished = root.join("vanished-cwd");
    fs::create_dir(&vanished)?;
    fs::remove_file(&marker)?;
    let mut argv = moved_argv.clone();
    argv[4] = json!(vanished);
    api!(
        json!({"action":"start","id":"vanished-cwd","title":"unlinked cwd","instance":h.handle["instance"],"workspace":h.handle["workspace"],"cwd":root,"argv":argv})
    );
    marked(&marker, path(&vanished.canonicalize()?)?)?;
    fs::remove_dir(&vanished)?;
    let r = api!(
        json!({"action":"worktree-remove","id":"api-tree","force":true}),
        false
    );
    ensure!(
        contains(&r, "current session cwd") && tree_path.exists(),
        "unlinked cwd evidence"
    );
    h.stop_task("vanished-cwd", 43)?;
    fs::remove_file(&marker)?;
    let pane = h.fux(
        "split",
        json!({"axis":"horizontal","cwd":root,"argv":moved_argv}),
    )?["pane"]
        .clone();
    marked(&marker, path(&tree_path)?)?;
    let before = fs::read(&h.journal)?;
    refuse(
        h,
        &tree_path,
        "current directory of an active session",
        Some(&before),
    )?;
    api!(h.adopt("moved-adopted", "changed cwd", &pane));
    refuse(
        h,
        &tree_path,
        "current directory of an active session",
        None,
    )?;
    api!(json!({"action":"forget","id":"moved-adopted"}));
    h.fux("kill", json!({"pane":pane}))?;
    h.closed(&pane)?;
    let descendant = root.join("descendant-ready");
    let pane=h.fux("split",json!({"axis":"horizontal","cwd":root,"argv":[std::env::current_exe()?,"fixture-worker","service-descendant",tree_path,descendant,"1"]}))?["pane"].clone();
    marked(&descendant, path(&root.canonicalize()?)?)?;
    let before = fs::read(&h.journal)?;
    refuse(
        h,
        &tree_path,
        "current directory of an active session",
        Some(&before),
    )?;
    api!(h.adopt("descendant-adopted", "grandchild cwd", &pane));
    refuse(
        h,
        &tree_path,
        "current directory of an active session",
        None,
    )?;
    api!(json!({"action":"forget","id":"descendant-adopted"}));
    h.fux("kill", json!({"pane":pane}))?;
    h.closed(&pane)?;
    let before: Value = serde_json::from_slice(&fs::read(&h.journal)?)?;
    let control = h.root.control();
    let parked = root.join("fux/parked-control");
    fs::rename(&control, &parked)?;
    api!(launch.clone(), false);
    let pending: Value = serde_json::from_slice(&fs::read(&h.journal)?)?;
    ensure!(
        pending["worktrees"]["api-tree"] == before["worktrees"]["api-tree"]
            && pending["launches"] == before["launches"]
            && tree_path.exists()
            && !tree_path.join("worker-cwd").exists(),
        "failed launch changed durable tree"
    );
    fs::rename(&parked, &control)?;
    let managed = api!(launch.clone());
    ensure!(
        managed["launch"]["phase"] == "attached"
            && managed["session"]["ownership"] == "managed"
            && managed["launch"]["worktree"] == "api-tree"
            && managed["launch"]["cwd"] == json!(tree_path)
            && managed["session"]["launch"] == managed["launch"]["id"]
            && managed["attempt"]["session"] == managed["session"]["id"],
        "managed launch associations"
    );
    marked(&tree_path.join("worker-cwd"), path(&tree_path)?)?;
    ensure!(
        !repo.join("worker-cwd").exists() && api!(launch.clone()) == managed,
        "launch replay"
    );
    refuse(h, &tree_path, "active or unresolved", None)?;
    fs::rename(&tree_path, &saved)?;
    let tasks = root.join("tasks");
    let mut args = vec![
        "--state-directory",
        path(&tasks)?,
        "task",
        "start",
        "api-managed",
        "--title",
        s(&launch["title"])?,
        "--instance",
        s(&h.handle["instance"])?,
        "--workspace",
        s(&h.handle["workspace"])?,
        "--worktree",
        "api-tree",
        "--",
    ];
    for arg in items(&launch["argv"])? {
        args.push(s(arg)?);
    }
    ensure!(
        h.cli(&args, true, Duration::from_secs(5))? == managed && api!(launch.clone()) == managed,
        "missing checkout retained replay"
    );
    fs::rename(&saved, &tree_path)?;
    api!(altered(&launch, "worktree", json!("missing")), false);
    api!(
        altered(&without(&launch, "worktree")?, "cwd", json!(tree_path)),
        false
    );
    let original = fs::read(&h.journal)?;
    for (field, value) in [("worktree", json!("missing")), ("cwd", json!(root))] {
        let mut v: Value = serde_json::from_slice(&original)?;
        v["launches"]["api-managed"][field] = value;
        let encoded = h.write(&v)?;
        api!(json!({"action":"inspect","id":"api-managed"}), false);
        ensure!(fs::read(&h.journal)? == encoded, "corrupt launch repaired");
        fs::write(&h.journal, &original)?;
    }
    Ok(Managed {
        launch,
        managed,
        tree: tree_path,
        tree_request,
    })
}
fn git(h: &Harness<'_>, repo: &std::path::Path, args: &[&str]) -> Result<()> {
    let mut c = h.root.command(std::path::Path::new("/usr/bin/git"));
    c.arg("-C").arg(repo).args(args);
    let r = process::output(c, Duration::from_secs(5), 1048576)?;
    ensure!(
        r.status.success(),
        "git: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    Ok(())
}
fn marked(marker: &std::path::Path, text: &str) -> Result<()> {
    until(Duration::from_secs(12), || {
        Ok((marker.exists() && fs::read_to_string(marker)?.trim() == text).then_some(()))
    })
}
fn refuse(
    h: &Harness<'_>,
    tree: &std::path::Path,
    text: &str,
    before: Option<&[u8]>,
) -> Result<()> {
    for force in [false, true] {
        let r = h.api(
            json!({"action":"worktree-remove","id":"api-tree","force":force}),
            false,
        )?;
        ensure!(contains(&r, text) && tree.exists(), "removal refusal: {r}");
        if let Some(before) = before {
            ensure!(
                fs::read(&h.journal)? == before,
                "removal refusal changed journal"
            );
        }
    }
    Ok(())
}
