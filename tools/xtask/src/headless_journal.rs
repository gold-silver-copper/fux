//! Account-free public-CLI journal sample, not an internal lock-time profiler.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    local::{Root, completed, until},
    process,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};
fn digest(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
fn cpu_us() -> Result<i128> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    ensure!(
        unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, usage.as_mut_ptr()) } == 0,
        "child CPU usage: {}",
        std::io::Error::last_os_error()
    );
    // getrusage initialized the complete result on success.
    let usage = unsafe { usage.assume_init() };
    Ok(
        (i128::from(usage.ru_utime.tv_sec) + i128::from(usage.ru_stime.tv_sec)) * 1_000_000
            + i128::from(usage.ru_utime.tv_usec)
            + i128::from(usage.ru_stime.tv_usec),
    )
}
pub fn run(args: Vec<String>) -> Result<()> {
    let mut flags = BTreeMap::new();
    ensure!(
        args.len().is_multiple_of(2),
        "usage: headless-journal --fux PATH --zor PATH --output NEW_PATH"
    );
    for pair in args.chunks_exact(2) {
        ensure!(
            ["--fux", "--zor", "--output"].contains(&pair[0].as_str()),
            "unknown argument {}",
            pair[0]
        );
        flags.insert(pair[0].as_str(), pair[1].as_str());
    }
    let fux = Path::new(flags.get("--fux").context("missing --fux")?).canonicalize()?;
    let zor = Path::new(flags.get("--zor").context("missing --zor")?).canonicalize()?;
    let output = Path::new(flags.get("--output").context("missing --output")?);
    ensure!(!output.exists(), "output already exists");
    let mut evidence = json!({"schema":1,"scope":"Public CLI, journal activity, fixed binaries; no model calls","provenance":{"fux_sha256":digest(&fux)?,"zor_sha256":digest(&zor)?,"harness_sha256":format!("{:x}",Sha256::digest(include_bytes!("headless_journal.rs"))),"harness_kind":"rust-public-cli","process_support_sha256":format!("{:x}",Sha256::digest(include_bytes!("support/process.rs")))},"runs":[]});
    for repetition in 1..=3 {
        let mut root = Root::new("hjournal-rs-", &["/bin/cat".into()])?;
        root.env.insert("PATH".into(), "/usr/bin:/bin".into());
        root.env.remove("SHELL");
        let mut server = root.server(&fux)?;
        let mut worker = None;
        let measured = (|| -> Result<Value> {
            let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
            let pane = listing
                .pointer("/workspaces/0/tabs/0/panes/0")
                .context("pane")?;
            worker = Some(i32::try_from(pane["pid"].as_u64().context("worker pid")?)?);
            let instance = listing["instance"].as_str().context("instance")?;
            let pane_id = pane["id"].to_string();
            let journal = root.path().join("state/zor/journal.json");
            let mut samples = Vec::new();
            for kind in ["adopt", "inspect", "idempotent-adopt"] {
                for index in 0..32 {
                    let before = match fs::metadata(&journal) {
                        Ok(m) => Some(m),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                        Err(e) => return Err(e.into()),
                    };
                    let mut command = root.command(&zor);
                    command.arg("task");
                    if kind == "inspect" {
                        command.args(["inspect", "t0"]);
                    } else {
                        command.args([
                            "adopt",
                            &format!("t{index}"),
                            "--title",
                            "journal workload",
                            "--instance",
                            instance,
                            "--workspace",
                            "default",
                            "--pane",
                            &pane_id,
                        ]);
                    }
                    let cpu = cpu_us()?;
                    let start = Instant::now();
                    let result =
                        process::output(command, Duration::from_secs(10), 4 * 1024 * 1024)?;
                    let elapsed = start.elapsed().as_secs_f64() * 1000.;
                    let after_cpu = cpu_us()?;
                    ensure!(
                        result.status.success(),
                        "journal {kind}: {}",
                        String::from_utf8_lossy(&result.stderr)
                    );
                    let _: Value = serde_json::from_slice(&result.stdout)?;
                    let after = fs::metadata(&journal)?;
                    let changed = before.is_none_or(|b| b.ino() != after.ino());
                    ensure!(
                        changed == (kind == "adopt"),
                        "unexpected journal replacement for {kind}"
                    );
                    samples.push(json!({"kind":kind,"elapsed_ms":elapsed,"child_cpu_ms":(after_cpu-cpu) as f64/1000.,"journal_bytes":after.len(),"committed_replacement":changed,"logical_committed_bytes":if changed{after.len()}else{0}}));
                }
            }
            let state: Value = serde_json::from_slice(&fs::read(journal)?)?;
            ensure!(state["generation"] == 32, "journal generation");
            Ok(
                json!({"repetition":repetition,"samples":samples,"final_generation":state["generation"]}),
            )
        })();
        // Finish even when measurement failed; do not publish incomplete evidence.
        let cleanup = server.finish();
        let worker_cleanup = if let Some(pid) = worker {
            until(Duration::from_secs(5), || {
                match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                    Err(nix::errno::Errno::ESRCH) => Ok(Some(())),
                    Ok(()) => Ok(None),
                    Err(e) => Err(e.into()),
                }
            })
        } else {
            Ok(())
        };
        cleanup?;
        worker_cleanup?;
        let mut record = measured?;
        ensure!(worker.is_some(), "missing worker identity");
        record["server_exit"] = json!(0);
        record["worker_exit_confirmed"] = json!(true);
        evidence["runs"]
            .as_array_mut()
            .context("runs")?
            .push(record);
        println!("journal repetition {repetition} passed");
        std::io::stdout().flush()?;
    }
    evidence["limits"] = json!(
        "CPU includes CLI startup; logical committed bytes count atomic journal replacements, not physical filesystem writes. Inode checks observe these serial operations only. No internal lock hold time, allocation count or durability timing isolation. Inspection/replay make zero replacements. The journal implementation is unchanged by the fux sharing optimization. Rust elapsed measurements include the bounded subprocess runner's 10 ms completion polling; do not compare them directly to historical Python harness latency."
    );
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(&evidence)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?
        .write_all(&bytes)?;
    Ok(())
}
