//! Controlled effect-before-reply recovery samples through the public zor CLI.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    launch_proxy::Proxy,
    local::{Root, completed},
    process,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};

fn task(root: &Root, zor: &Path, args: &[&str], success: bool) -> Result<Value> {
    let mut command = root.command(zor);
    command.arg("task").args(args);
    let result = process::output(command, Duration::from_secs(15), 1024 * 1024)?;
    ensure!(
        result.status.success() == success,
        "task {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    if success {
        Ok(serde_json::from_slice(&result.stdout)?)
    } else {
        Ok(Value::Null)
    }
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(args.len() == 2, "measure-recovery FUX_BINARY ZOR_BINARY");
    let fux = Path::new(&args[0]).canonicalize()?;
    let zor = Path::new(&args[1]).canonicalize()?;
    let root = Root::new("recovery-perf-", &["/bin/cat".into()])?;
    let mut server = root.server(&fux)?;
    let result = (|| -> Result<Value> {
        let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
        let instance = listing["instance"].as_str().context("instance")?;
        let runtime = root.path().join("proxy");
        std::fs::create_dir(&runtime)?;
        let mut proxy = Proxy::start(
            &runtime,
            &root.control(),
            &root.control().with_file_name("manager.sock"),
            json!(instance),
        )?;
        let runtime_arg = runtime.to_str().context("runtime path")?;
        let cwd = root.path().to_str().context("cwd path")?;
        let journal = root.path().join("state/zor/journal.json");
        let mut samples = Vec::new();
        for index in 0..12 {
            let name = format!("recover-{index}");
            task(
                &root,
                &zor,
                &[
                    "start",
                    &name,
                    "--title",
                    "recovery benchmark",
                    "--instance",
                    instance,
                    "--workspace",
                    "default",
                    "--runtime",
                    runtime_arg,
                    "--cwd",
                    cwd,
                    "--",
                    "/bin/cat",
                ],
                false,
            )?;
            let uncertain = task(&root, &zor, &["inspect", &name], true)?;
            ensure!(
                uncertain["launch"]["phase"] == "uncertain",
                "lost creation reply must preserve uncertainty"
            );
            let creates = proxy.faults().creates;
            let before = std::fs::metadata(&journal)?;
            let server_cpu = crate::measure::cpu(server.child.id())?;
            let child_cpu = crate::headless_journal::cpu_us()?;
            let start = Instant::now();
            let recovered = task(&root, &zor, &["launch-reconcile", &name], true)?;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
            let child_cpu_ms = (crate::headless_journal::cpu_us()? - child_cpu) as f64 / 1000.;
            let cpu_s = crate::measure::cpu(server.child.id())? - server_cpu;
            let after = std::fs::metadata(&journal)?;
            ensure!(
                recovered["launch"]["phase"] == "attached" && proxy.faults().creates == creates,
                "reconciliation must attach without duplicate creation"
            );
            let target = recovered
                .pointer("/session/target")
                .context("session target")?;
            let current = completed(&root.control(), json!({"command":"list","id":2}))?;
            let panes = current
                .pointer("/workspaces/0/tabs/0/panes")
                .and_then(Value::as_array)
                .context("panes")?;
            ensure!(
                panes.iter().any(|p| p["id"] == target["pane"]
                    && p["pid"] == target["pid"]
                    && p["fixed_workspace"] == false),
                "recovered exact process and released pin"
            );
            ensure!(
                before.ino() != after.ino(),
                "recovery must commit attachment"
            );
            let retry_cpu = crate::headless_journal::cpu_us()?;
            proxy.begin_timing();
            let start = Instant::now();
            let retry = task(&root, &zor, &["launch-reconcile", &name], true)?;
            let retry_ms = start.elapsed().as_secs_f64() * 1000.;
            let retry_proxy_timings = proxy.finish_timing(start);
            let retry_cpu_ms = (crate::headless_journal::cpu_us()? - retry_cpu) as f64 / 1000.;
            ensure!(
                retry == recovered && proxy.faults().creates == creates,
                "recovery retry changed identity"
            );
            samples.push(json!({"recovery_ms":elapsed_ms,"idempotent_recovery_ms":retry_ms,"retry_proxy_timings":retry_proxy_timings,"recovery_child_cpu_ms":child_cpu_ms,"retry_child_cpu_ms":retry_cpu_ms,"server_cpu_s":cpu_s,"server_rss_kib":crate::measure::rss(server.child.id())?,"journal_bytes":after.len(),"committed_attachment":true,"creates":creates}));
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let mut command = root.command(&zor);
                command.args(["task", "stop", &name]);
                let stopped = process::output(command, Duration::from_secs(8), 1024 * 1024)?;
                let state = task(&root, &zor, &["inspect", &name], true)?;
                ensure!(
                    state["launch"]["stop_requested"] == true
                        && state["task"]["outcome"] == "cancelled",
                    "cleanup stop authority"
                );
                if stopped.status.success() && state["launch"]["phase"] == "closed" {
                    break;
                }
                ensure!(Instant::now() < deadline, "cleanup release not confirmed");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        proxy.close()?;
        Ok(
            json!({"fux":fux,"zor":zor,"samples":samples,"limits":"Recovery of a real pane after a controlled lost create reply. Elapsed includes CLI startup and 10 ms subprocess observation polling. Proxy accept wakes on socket readiness, with a 10 ms maximum cancellation-check interval. Child CPU measures reaped zor CLI user+system time including startup; server CPU and sampled RSS are separate and exclude descendants. Journal bytes are logical size, not physical writes; no fsync timing isolation."}),
        )
    })();
    let cleanup = server.finish();
    let value = result?;
    cleanup?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
