//! Paired worktree workload; herdr checks remain external harness evidence.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    local::{Root, until},
    process::{self, Guard, OwnedProcess},
    service,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    io::{BufRead, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
fn digest(p: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(p)?)))
}
fn git(root: &Root, path: &Path, args: &[&str]) -> Result<()> {
    let mut c = root.command(Path::new("/usr/bin/git"));
    c.arg("-C").arg(path).args(args);
    let r = process::output(c, Duration::from_secs(5), 1024 * 1024)?;
    ensure!(
        r.status.success(),
        "fixture git: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    Ok(())
}
pub fn worker() -> Result<()> {
    // Original fixture-wide bound also applies to a stuck stdin reader.
    unsafe {
        libc::alarm(90);
    }
    let cwd = std::env::current_dir()?;
    fs::write(
        format!("{}.pid", cwd.display()),
        std::process::id().to_string(),
    )?;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let text = line.trim();
        if std::env::var("WORKFLOW_LEAD").as_deref() == Ok("1") {
            println!("HANDOFF_RECEIVED {text}");
            std::io::stdout().flush()?;
            continue;
        }
        fs::write("output.txt", text)?;
        for args in [
            &["add", "output.txt"][..],
            &["commit", "--allow-empty", "-m", "worker output"][..],
        ] {
            let mut c = Command::new("/usr/bin/git");
            c.args(args);
            let r = process::output(c, Duration::from_secs(5), 1024 * 1024)?;
            std::io::stdout().write_all(&r.stdout)?;
            std::io::stderr().write_all(&r.stderr)?;
            ensure!(r.status.success(), "worker git failed");
        }
        println!("RESPONSE {text}");
        std::io::stdout().flush()?;
    }
    Ok(())
}
fn herdr(binary: &Path) -> Result<Value> {
    let mut root = Root::new("workflow-pair-rs-", &["/bin/cat".into()])?;
    root.env.insert(
        "XDG_CACHE_HOME".into(),
        root.path().join("cache").to_str().context("cache")?.into(),
    );
    let config = root.path().join("config");
    let repo = root.path().join("repo");
    fs::create_dir(&repo)?;
    git(&root, &repo, &["init", "-b", "main"])?;
    git(&root, &repo, &["config", "user.name", "Fixture"])?;
    git(
        &root,
        &repo,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    fs::write(repo.join("base"), "shared baseline")?;
    git(&root, &repo, &["add", "."])?;
    git(&root, &repo, &["commit", "-m", "baseline"])?;
    let worker = root.path().join("claude");
    let exe = std::env::current_exe()?;
    let quoted = format!(
        "'{}'",
        exe.to_str()
            .context("fixture executable")?
            .replace('\'', "'\\''")
    );
    fs::write(
        &worker,
        format!("#!/bin/sh\nexec {quoted} workflow-capture-worker\n"),
    )?;
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))?;
    for name in ["herdr", "herdr-dev"] {
        let path = config.join(name).join("config.toml");
        fs::create_dir_all(path.parent().context("config parent")?)?;
        fs::write(
            path,
            format!(
                "onboarding = false\n[terminal]\ndefault_shell = {}\nshell_mode = \"non_login\"\n[update]\nversion_check = false\nmanifest_check = false\n",
                serde_json::to_string(&worker)?
            ),
        )?;
    }
    let log = tempfile::tempfile()?;
    let mut c = root.command(binary);
    c.arg("server")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut server = Guard(c.spawn()?);
    let mut workers = Vec::new();
    let transcript = RefCell::new(Vec::new());
    let result = (|| -> Result<Value> {
        let endpoint = until(Duration::from_secs(8), || {
            for name in ["herdr", "herdr-dev"] {
                let p = config.join(name).join("herdr.sock");
                if p.exists() {
                    return Ok(Some(p));
                }
            }
            Ok(None)
        })?;
        let call = |method: &str, params: Value, ok: bool| -> Result<Value> {
            let reply = service::rpc(
                &endpoint,
                &json!({"id":"workflow","method":method,"params":params}),
                Duration::from_secs(3),
            )?;
            transcript
                .borrow_mut()
                .push(json!({"method":method,"params":params,"reply":reply}));
            ensure!(
                reply.get("error").is_none() == ok,
                "herdr {method}: {reply}"
            );
            Ok(reply.get("result").cloned().unwrap_or(reply))
        };
        let pid = |directory: &Path| -> Result<i32> {
            let marker = PathBuf::from(format!("{}.pid", directory.display()));
            until(Duration::from_secs(8), || {
                let value = fs::read_to_string(&marker)
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok());
                Ok(value)
            })
        };
        let lead_dir = root.path().join("lead");
        fs::create_dir(&lead_dir)?;
        let lead = call(
            "workspace.create",
            json!({"cwd":lead_dir,"focus":true,"env":{"WORKFLOW_LEAD":"1"}}),
            true,
        )?;
        let lead_pane = lead["root_pane"]["pane_id"].clone();
        workers.push(pid(&lead_dir)?);
        let mut trees = BTreeMap::new();
        let mut panes = BTreeMap::new();
        let mut workspaces = BTreeMap::new();
        for name in ["alpha", "beta"] {
            let tree = root.path().join(name);
            call(
                "worktree.create",
                json!({"cwd":repo,"branch":name,"path":tree,"trust_repository":true,"focus":true}),
                true,
            )?;
            trees.insert(name, tree);
        }
        let listing = call("pane.list", json!({}), true)?;
        for (name, tree) in &trees {
            workers.push(pid(tree)?);
            let resolved = tree.canonicalize()?;
            let matching = listing["panes"]
                .as_array()
                .context("panes")?
                .iter()
                .filter(|p| p["cwd"] == json!(tree) || p["cwd"] == json!(resolved))
                .collect::<Vec<_>>();
            ensure!(matching.len() == 1, "pane cwd mismatch {name}: {listing}");
            panes.insert(*name, matching[0]["pane_id"].clone());
            workspaces.insert(*name, matching[0]["workspace_id"].clone());
        }
        ensure!(
            trees["alpha"] != trees["beta"] && !repo.join("output.txt").exists(),
            "isolated worktrees"
        );
        let response = |name: &str, text: &str| -> Result<()> {
            until(Duration::from_secs(8), || {
                let r = call(
                    "pane.read",
                    json!({"pane_id":panes[name],"source":"visible","strip_ansi":true}),
                    true,
                )?;
                Ok(r.to_string()
                    .contains(&format!("RESPONSE {text}"))
                    .then_some(()))
            })
        };
        for (name, text) in [("alpha", "result-alpha"), ("beta", "incorrect-beta")] {
            call(
                "pane.send_input",
                json!({"pane_id":panes[name],"text":format!("{text}\n")}),
                true,
            )?;
        }
        response("alpha", "result-alpha")?;
        response("beta", "incorrect-beta")?;
        let mut checks = Vec::new();
        for (name, tree) in &trees {
            let actual = fs::read_to_string(tree.join("output.txt"))?;
            let expected = format!("result-{name}");
            checks.push(json!({"worker":name,"expected":expected,"actual":actual,"passed":actual==expected}));
        }
        ensure!(
            checks[0]["passed"] == true && checks[1]["passed"] == false,
            "failed dependency fixture"
        );
        call(
            "pane.send_input",
            json!({"pane_id":panes["beta"],"text":"result-beta\n"}),
            true,
        )?;
        response("beta", "result-beta")?;
        ensure!(
            fs::read_to_string(trees["beta"].join("output.txt"))? == "result-beta",
            "repair output"
        );
        let mut collected = serde_json::Map::new();
        for (name, tree) in &trees {
            collected.insert(
                (*name).into(),
                fs::read_to_string(tree.join("output.txt"))?.into(),
            );
        }
        ensure!(
            Value::Object(collected.clone())
                == json!({"alpha":"result-alpha","beta":"result-beta"}),
            "collected output"
        );
        let unsupported = call("task.verify", json!({"task":"alpha"}), false)?;
        ensure!(
            unsupported["error"]["code"] == "invalid_request"
                && unsupported["error"]["message"]
                    .as_str()
                    .context("unsupported error")?
                    .contains("unknown variant `task.verify`"),
            "unrelated error used as verification evidence"
        );
        call(
            "pane.send_input",
            json!({"pane_id":lead_pane,"text":"result-alpha result-beta\n"}),
            true,
        )?;
        let handoff = until(Duration::from_secs(8), || {
            let v = call(
                "pane.read",
                json!({"pane_id":lead_pane,"source":"visible","strip_ansi":true}),
                true,
            )?;
            Ok(v.to_string()
                .contains("HANDOFF_RECEIVED result-alpha result-beta")
                .then_some(v))
        })?;
        let mut dirty = serde_json::Map::new();
        for (name, tree) in &trees {
            fs::write(tree.join("untracked"), "preserve unless explicitly forced")?;
            dirty.insert(
                (*name).into(),
                call(
                    "worktree.remove",
                    json!({"workspace_id":workspaces[name],"force":false,"trust_repository":true}),
                    false,
                )?,
            );
            ensure!(tree.join("untracked").exists(), "dirty data removed");
            call(
                "worktree.remove",
                json!({"workspace_id":workspaces[name],"force":true,"trust_repository":true}),
                true,
            )?;
            ensure!(!tree.exists(), "owned tree not removed");
        }
        ensure!(
            fs::read_to_string(repo.join("base"))? == "shared baseline"
                && !repo.join("output.txt").exists(),
            "main repo changed"
        );
        Ok(
            json!({"external_checks":checks,"external_collected":collected,"verification_api":unsupported,"dirty_errors":dirty,"main_preserved":true,"owned_trees_removed":true,"external_handoff_capture":handoff,"worker_count":workers.len()}),
        )
    })();
    let cleanup = (|| -> Result<()> {
        server.0.terminate()?;
        let status = process::wait(&mut server.0, Duration::from_secs(10))?;
        ensure!(status.success(), "herdr server exit {status}");
        Ok(())
    })();
    if cleanup.is_err() && server.0.try_wait()?.is_none() {
        server.0.kill()?;
        process::wait(&mut server.0, Duration::from_secs(5))?;
    }
    let gone = until(Duration::from_secs(8), || {
        for pid in &workers {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), None) {
                Err(nix::errno::Errno::ESRCH) => {}
                Ok(()) => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Some(()))
    });
    cleanup?;
    gone?;
    let mut result = result?;
    result["transcript"] = json!(transcript.into_inner());
    result["server_exit"] = json!(0);
    result["worker_exit_confirmed"] = json!(true);
    let encoded = serde_json::to_string(&result)?
        .replace(
            root.path().canonicalize()?.to_str().context("root")?,
            "<RUN_ROOT>",
        )
        .replace(root.path().to_str().context("root")?, "<RUN_ROOT>");
    Ok(serde_json::from_str(&encoded)?)
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len().is_multiple_of(2),
        "usage: capture-workflow --fux PATH --zor PATH --herdr PATH --herdr-provenance JSON --output NEW_JSON"
    );
    let mut flags = BTreeMap::new();
    for p in args.chunks_exact(2) {
        ensure!(
            [
                "--fux",
                "--zor",
                "--herdr",
                "--herdr-provenance",
                "--output"
            ]
            .contains(&p[0].as_str()),
            "unknown argument"
        );
        flags.insert(p[0].as_str(), p[1].as_str());
    }
    let output = Path::new(flags.get("--output").context("missing output")?);
    ensure!(!output.exists(), "use new output file");
    let mut binaries = BTreeMap::new();
    let mut provenance = json!({"harness_kind":"rust-paired-workflow","binaries":{},"sources":{},"herdr_build":serde_json::from_slice::<Value>(&fs::read(flags.get("--herdr-provenance").context("missing herdr provenance")?)?)?});
    for name in ["fux", "zor", "herdr"] {
        let key = format!("--{name}");
        let p = Path::new(flags.get(key.as_str()).context("missing binary")?).canonicalize()?;
        provenance["binaries"][name] = json!({"path":p,"sha256":digest(&p)?});
        binaries.insert(name, p);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("root")?;
    for p in [
        "tools/xtask/src/workflow_capture.rs",
        "tools/xtask/src/scenarios/zor_workflow.rs",
        "tools/xtask/src/scenarios/mod.rs",
        "tools/xtask/src/support/contention.rs",
    ] {
        provenance["sources"][p] = digest(&root.join(p))?.into();
    }
    ensure!(
        provenance["herdr_build"]["binary_sha256"] == provenance["binaries"]["herdr"]["sha256"],
        "herdr build mismatch"
    );
    let mut c = Command::new(std::env::current_exe()?);
    c.args(["scenario", "zor-workflow"])
        .arg(&binaries["fux"])
        .arg(&binaries["zor"]);
    let zor = process::output(c, Duration::from_secs(180), 4 * 1024 * 1024)?;
    ensure!(
        zor.status.success(),
        "zor workflow: {} {}",
        String::from_utf8_lossy(&zor.stdout),
        String::from_utf8_lossy(&zor.stderr)
    );
    println!("zor workflow passed");
    std::io::stdout().flush()?;
    let herdr = herdr(&binaries["herdr"])?;
    println!("herdr workflow passed");
    let evidence = json!({"recorded_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"provenance":provenance,"zor":{"exit_code":zor.status.code(),"stdout":String::from_utf8(zor.stdout)?,"stderr":String::from_utf8(zor.stderr)?},"herdr":herdr});
    let mut bytes = serde_json::to_vec_pretty(&evidence)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?
        .write_all(&bytes)?;
    Ok(())
}
