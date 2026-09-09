//! Durable task evidence, overload admission and bounded service results.
use super::{service_fixture::*, service_worktrees::Managed};
use crate::support::{contention, local::until, process, service};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
pub fn run(h: &Harness<'_>, m: &Managed) -> Result<Value> {
    macro_rules! api {
        ($v:expr) => {
            h.api($v, true)?
        };
        ($v:expr,false) => {
            h.api($v, false)?
        };
    }
    let requirement = json!({"action":"require-artifact","id":"api-managed","artifact":"report","path":"result.bin"});
    let policy = api!(requirement.clone());
    ensure!(
        policy["artifact_policy"] == json!({"sealed":false,"required_count":1,"collected_count":0})
            && api!(requirement.clone()) == policy,
        "artifact policy replay"
    );
    api!(altered(&requirement, "path", json!("changed")), false);
    ensure!(
        items(&api!(json!({"action":"result","id":"api-managed"}))["blockers"])?
            .contains(&json!("required-artifacts-missing")),
        "missing artifact blocker"
    );
    fs::write(m.tree.join("result.bin"), b"api\0\xff")?;
    let request = json!({"action":"artifact-collect","id":"api-managed","artifact":"api-output","path":"result.bin","requirement":"report"});
    let artifact = api!(request.clone());
    ensure!(
        artifact["artifact"]["bytes"] == json!([97, 112, 105, 0, 255]),
        "binary artifact"
    );
    api!(altered(&request, "requirement", Value::Null), false);
    api!(altered(&requirement, "artifact", json!("late")), false);
    ensure!(
        api!(json!({"action":"inspect","id":"api-managed"}))["artifact_policy"]
            == json!({"sealed":true,"required_count":1,"collected_count":1}),
        "artifact sealing"
    );
    fs::remove_file(m.tree.join("result.bin"))?;
    ensure!(
        api!(request.clone()) == artifact
            && api!(json!({"action":"artifact-inspect","artifact":"api-output"})) == artifact,
        "artifact retained replay"
    );
    api!(altered(&request, "path", json!("different")), false);
    let check = json!({"action":"check","id":"api-managed","check":"api-check","timeout_ms":1000,"requirement":"unit","argv":["/bin/sh","-c","printf check-output; printf check-error >&2; exit 7"]});
    let requirement =
        json!({"action":"require-check","id":"api-managed","check":"unit","argv":check["argv"]});
    let required = api!(requirement.clone());
    ensure!(
        required["task"]["required_checks"]["unit"] == check["argv"]
            && api!(requirement.clone()) == required,
        "check requirement replay"
    );
    api!(altered(&check, "argv", json!(["/bin/true"])), false);
    let checked = api!(check.clone());
    ensure!(
        checked["passed"] == false
            && checked["check"]["exit_code"] == 7
            && checked["check"]["stdout"] == "check-output"
            && checked["check"]["stderr"] == "check-error"
            && api!(check.clone()) == checked
            && api!(json!({"action":"check-inspect","check":"api-check"})) == checked,
        "check result replay"
    );
    api!(altered(&requirement, "check", json!("too-late")), false);
    ensure!(
        api!(json!({"action":"inspect","id":"api-managed"}))["task"]["outcome"] == "open",
        "check changed task outcome"
    );
    let before = fs::read(&h.journal)?;
    let result = api!(json!({"action":"result","id":"api-managed"}));
    ensure!(
        fs::read(&h.journal)? == before
            && result["task_outcome"] == "open"
            && result["verification"]["status"] == "unverified"
            && result["required_checks"]
                == json!([{"name":"unit","status":"failed","execution":"api-check"}])
            && result["artifacts"][0]["id"] == "api-output"
            && result["required_artifacts"]
                == json!([{"name":"report","path":"result.bin","status":"collected","artifact":"api-output"}]),
        "readonly result evidence: {result}"
    );
    ensure!(
        !items(&result["blockers"])?.contains(&json!("required-artifacts-missing"))
            && items(&result["blockers"])?.contains(&json!("required-checks-not-passed"))
            && api!(json!({"action":"result","id":"api-managed"})) == result,
        "result blockers"
    );
    let source_request = json!({"action":"source-collect","id":"api-managed","source":"api-source","revision":"HEAD"});
    let source = api!(source_request.clone());
    ensure!(
        source["source"]["file_count"] == 1
            && api!(source_request.clone()) == source
            && api!(json!({"action":"source-inspect","source":"api-source"})) == source
            && api!(json!({"action":"source-file","source":"api-source","path":"file.txt"}))["file"]
                ["bytes"]
                == json!(b"baseline".to_vec()),
        "source bytes replay"
    );
    let source_check = json!({"action":"check","id":"api-managed","check":"api-source-check","source":"api-source","timeout_ms":1000,"argv":["/bin/cat","file.txt"]});
    let checked = api!(source_check.clone());
    ensure!(
        checked["passed"] == true
            && checked["check"]["stdout"] == "baseline"
            && std::path::Path::new(s(&checked["check"]["cwd"])?).parent()
                == Some(h.root.path().join("tasks").canonicalize()?.as_path())
            && api!(source_check.clone()) == checked,
        "source check replay"
    );
    api!(altered(&source_check, "source", Value::Null), false);
    let result = api!(json!({"action":"result","id":"api-managed"}));
    ensure!(
        result["sources"] == json!([source["source"]])
            && result["verification"]["status"] == "unverified",
        "source result"
    );
    let before = fs::read(&h.journal)?;
    api!(
        json!({"action":"verify","id":"api-managed","source":"api-source"}),
        false
    );
    ensure!(
        fs::read(&h.journal)? == before,
        "failed verify changed journal"
    );
    let mut capture = source_check.clone();
    capture["check"] = json!("api-capture");
    capture["artifacts"] = json!({"report":"api-captured-output"});
    capture["argv"] = json!(["/bin/sh", "-c", "printf bound-output > result.bin"]);
    let bound = api!(capture.clone());
    ensure!(
        bound["passed"] == true && bound["check"]["artifact_problems"] == json!({}),
        "bound capture"
    );
    let artifact =
        api!(json!({"action":"artifact-inspect","artifact":"api-captured-output"}))["artifact"]
            .clone();
    ensure!(
        artifact["bytes"] == json!(b"bound-output".to_vec())
            && artifact["source"] == "api-source"
            && artifact["check"] == "api-capture"
            && artifact["created_generation"] == bound["generation"]
            && api!(capture.clone()) == bound,
        "atomic bound artifact replay"
    );
    api!(altered(&capture, "artifacts", json!({})), false);
    api!(
        altered(&capture, "check", json!("api-capture-conflict")),
        false
    );
    ensure!(
        api!(json!({"action":"result","id":"api-managed"}))["artifact_captures"][0]["status"]
            == "collected",
        "artifact capture result"
    );
    let changes_request =
        json!({"action":"changes-collect","id":"api-managed","changes":"api-changes"});
    let changes = api!(changes_request.clone());
    ensure!(
        changes["changes"]["task"] == "api-managed"
            && changes["changes"]["worktree"] == "api-tree"
            && api!(changes_request) == changes
            && api!(json!({"action":"changes-inspect","changes":"api-changes"})) == changes
            && api!(json!({"action":"result","id":"api-managed"}))["changed_files"]["evidence"]
                == changes["changes"],
        "changes evidence"
    );
    let observe = h.adopt(
        "api-observed",
        "observation only",
        &m.managed["session"]["target"]["pane"],
    );
    let observed = api!(observe.clone());
    api!(
        json!({"action":"require-artifact","id":"api-observed","artifact":"report","path":"result.bin"}),
        false
    );
    ensure!(
        observed["session"]["ownership"] == "adopted" && observed["session"]["launch"].is_null(),
        "adoption authority"
    );
    let mut conflict = observe;
    conflict["id"] = json!("api-managed");
    conflict["title"] = json!("managed API fixture");
    api!(conflict, false);
    api!(json!({"action":"stop","id":"api-observed"}), false);
    api!(altered(
        &h.adopt("api-task", "API fixture", &h.handle["pane"]),
        "agent",
        json!("test")
    ));
    let prepared = api!(
        json!({"action":"prepare","id":"api-task","operation":"api-op","text":"x".repeat(5000),"timeout_ms":60000})
    );
    ensure!(prepared["delivery"] == "prepared", "API preparation");
    api!(json!({"action":"submit","operation":"api-op"}));
    let delivered = h.delivered("api-op")?;
    api!(
        json!({"action":"report","operation":"api-op","token":prepared["report_token"],"producer":"api-fixture","sequence":1,"input_operation":delivered["receipt"]["operation"],"kind":"response-observed"})
    );
    ensure!(
        api!(json!({"action":"wait","operation":"api-op"}))["wait"] == "response-observed",
        "API report"
    );
    let claimed = api!(json!({"action":"result","id":"api-task"}));
    ensure!(
        claimed["verification"]["status"] == "unverified"
            && claimed["prompt_evidence"]["latest"][0]["claim"]["producer"] == "api-fixture"
            && !contains(&claimed, s(&prepared["report_token"])?)
            && claimed["output"]["available"] == false,
        "adopted claim is not output or verification"
    );
    api!(
        json!({"action":"prepare","id":"api-task","operation":"api-lost","text":"lost reply input","timeout_ms":60000})
    );
    drop(service::Peer::send(
        &h.endpoint,
        &json!({"v":1,"id":42,"op":"task","service_instance":h.instance,"task":{"action":"submit","operation":"api-lost"}}),
        Duration::from_secs(3),
    )?);
    until(Duration::from_secs(12), || {
        Ok(
            (h.api(json!({"action":"inspect","id":"api-task"}), true)?["prompts"][0]["delivery"]
                != "prepared")
                .then_some(()),
        )
    })?;
    let lost = h.delivered("api-lost")?;
    ensure!(
        api!(json!({"action":"submit","operation":"api-lost"})) == lost,
        "lost API reply replay"
    );
    ensure!(
        h.cli(
            &[
                "--state-directory",
                path(&h.root.path().join("tasks"))?,
                "task",
                "inspect",
                "api-task"
            ],
            true,
            Duration::from_secs(5)
        )? == api!(json!({"action":"inspect","id":"api-task"})),
        "CLI and API state differ"
    );
    let cancelled = api!(json!({"action":"cancel","id":"api-task"}));
    ensure!(
        cancelled["task"]["outcome"] == "cancelled",
        "API cancellation"
    );
    api!(json!({"action":"submit","operation":"api-lost"}), false);
    let mut disposable = m.tree_request.clone();
    disposable["id"] = json!("api-remove");
    disposable["branch"] = json!("api-remove");
    let disposable = api!(disposable);
    let p = std::path::Path::new(s(&disposable["worktree"]["path"])?);
    fs::write(p.join("untracked"), "fixture")?;
    api!(json!({"action":"worktree-remove","id":"api-remove"}), false);
    ensure!(
        api!(json!({"action":"worktree-remove","id":"api-remove","force":true}))["worktree"]["phase"]
            == "removed"
            && !p.exists(),
        "forced owned tree removal"
    );
    overload(h)?;
    large(h)?;
    let cancelled = api!(json!({"action":"inspect","id":"api-task"}));
    let committed = fs::read(&h.journal)?;
    fs::write(&h.journal, b"corrupt fixture")?;
    ensure!(
        api!(json!({"action":"list"}), false)["error"] == "task-failed"
            && h.snapshot()?["status"] == "completed"
            && fs::read(&h.journal)? == b"corrupt fixture",
        "task storage failure stopped observations or repaired journal"
    );
    fs::write(&h.journal, &committed)?;
    Ok(cancelled)
}
fn overload(h: &Harness<'_>) -> Result<()> {
    let proxy_root = h.root.path().join("proxy");
    fs::create_dir(&proxy_root)?;
    let mut proxy = Proxy::new(
        &proxy_root.join(format!("{}.sock", s(&h.handle["workspace"])?)),
        h.root.control(),
    )?;
    let result = (|| -> Result<()> {
        h.cli(
            &[
                "--state-directory",
                path(&h.root.path().join("tasks"))?,
                "task",
                "adopt",
                "slow-task",
                "--title",
                "slow fixture",
                "--instance",
                s(&h.handle["instance"])?,
                "--workspace",
                s(&h.handle["workspace"])?,
                "--pane",
                &h.handle["pane"].to_string(),
                "--runtime",
                path(&proxy_root)?,
            ],
            true,
            Duration::from_secs(5),
        )?;
        h.api(json!({"action":"prepare","id":"slow-task","operation":"slow-op","text":"unsent","timeout_ms":60000}),true)?;
        let enqueue = |payload: Value, id: u64| {
            service::Peer::send(
                &h.endpoint,
                &json!({"v":1,"id":id,"op":"task","service_instance":h.instance,"task":payload}),
                Duration::from_secs(5),
            )
        };
        proxy.block.store(true, Ordering::SeqCst);
        let mut pending = vec![enqueue(
            json!({"action":"reserve","operation":"slow-op"}),
            90,
        )?];
        until(Duration::from_secs(3), || {
            Ok(proxy.blocked.load(Ordering::SeqCst).then_some(()))
        })?;
        for i in 0..12 {
            pending.push(enqueue(
                h.adopt(&format!("queued-{i}"), "queue fixture", &h.handle["pane"]),
                100 + i,
            )?);
        }
        let began = Instant::now();
        ensure!(
            h.snapshot()?["status"] == "completed"
                && h.exchange(&json!({"v":1,"id":9,"op":"ping"}))?["status"] == "completed"
                && began.elapsed() < Duration::from_millis(1500),
            "task worker stalled observation/control"
        );
        std::thread::sleep(Duration::from_millis(200));
        proxy.release.store(true, Ordering::SeqCst);
        let mut overloads = Vec::new();
        for peer in pending {
            let v = peer.receive()?;
            if v["error"] == "task-overloaded" {
                overloads.push(v);
            }
        }
        ensure!(!overloads.is_empty(), "queue did not reject overload");
        let tasks = h.api(json!({"action":"list"}), true)?;
        for reply in overloads {
            let id = reply["id"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("overload id"))?;
            ensure!(
                id >= 100
                    && !items(&tasks["tasks"])?
                        .iter()
                        .any(|t| t["id"] == format!("queued-{}", id - 100)),
                "overloaded task was admitted"
            );
        }
        h.api(json!({"action":"cancel","id":"slow-task"}), true)?;
        Ok(())
    })();
    let closed = proxy.close();
    result?;
    closed?;
    Ok(())
}
fn large(h: &Harness<'_>) -> Result<()> {
    h.api(
        h.adopt("large-result", "large fixture", &h.handle["pane"]),
        true,
    )?;
    for (prefix, count, text) in [
        ("large", 9, "x".repeat(64000)),
        ("compact", 32, "short".into()),
    ] {
        for i in 0..count {
            let op = format!("{prefix}-{i}");
            h.api(json!({"action":"prepare","id":"large-result","operation":op,"text":text,"timeout_ms":60000}),true)?;
            h.api(json!({"action":"abandon","operation":op}), true)?;
        }
    }
    let retained = fs::read(&h.journal)?;
    let mut v: Value = serde_json::from_slice(&retained)?;
    v["prompts"]["large-0"]["released"] = json!(false);
    v["prompts"]["large-0"]["wait"] = json!("pending");
    let before = h.write(&v)?;
    let compact = h.api(json!({"action":"result","id":"large-result"}), true)?;
    ensure!(
        fs::read(&h.journal)? == before
            && items(&compact["prompt_evidence"]["latest"])?.len() == 32
            && compact["prompt_evidence"]["omitted_count"] == 9
            && compact["prompt_evidence"]["unresolved_count"] == 1
            && !items(&compact["prompt_evidence"]["latest"])?
                .iter()
                .any(|v| v["operation"] == "large-0")
            && items(&compact["blockers"])?.contains(&json!("prompt-unresolved"))
            && compact.to_string().len() < 32768
            && !contains(&compact, &"x".repeat(5000)),
        "compact result evidence"
    );
    fs::write(&h.journal, &retained)?;
    ensure!(
        h.api(json!({"action":"cancel","id":"large-result"}), false)?["error"]
            == "response-too-large"
            && h.api(json!({"action":"result","id":"large-result"}), true)?["task_outcome"]
                == "cancelled",
        "bounded reply lost committed mutation"
    );
    let end = Instant::now() + Duration::from_secs(12);
    let large = until(Duration::from_secs(12), || {
        let left = end
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow::anyhow!("large result inspection deadline"))?;
        let c = h.command(&[
            "--state-directory",
            path(&h.root.path().join("tasks"))?,
            "task",
            "inspect",
            "large-result",
        ]);
        let r = process::output(c, left.min(Duration::from_secs(5)), 2097152)?;
        if contention::cli_busy(r.status.code(), &r.stderr) {
            return Ok(None);
        }
        ensure!(
            r.status.success(),
            "large inspect: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        Ok(Some(serde_json::from_slice::<Value>(&r.stdout)?))
    })?;
    ensure!(
        large["task"]["outcome"] == "cancelled"
            && h.api(json!({"action":"forget","id":"large-result"}), true)?["forgotten"] == true,
        "large result reconciliation"
    );
    Ok(())
}
