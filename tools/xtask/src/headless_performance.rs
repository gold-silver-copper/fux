//! Headless viewer baseline: synthetic workload, private local sockets, owned resources.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    attachment,
    local::{self, Root},
    process::{self, Guard, OwnedProcess},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[derive(Default)]
struct Counters {
    received: u64,
    decoded: u64,
    frames: u64,
    high_water: usize,
    markers: BTreeMap<String, BTreeSet<String>>,
}
impl Counters {
    fn value(&self) -> Value {
        json!({"received_bytes":self.received,"decoded_frame_bytes":self.decoded,"frames":self.frames,"pending_high_water_bytes":self.high_water})
    }
}
#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    counters: Mutex<Counters>,
    errors: Mutex<Vec<String>>,
}
struct Viewer {
    state: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
fn read_viewer(mut peer: UnixStream, slow: bool, state: &Shared) -> Result<()> {
    let mut pending = Vec::new();
    while !state.stop.load(Ordering::Acquire) {
        let mut fds = [nix::poll::PollFd::new(
            peer.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        match nix::poll::poll(&mut fds, 100u16) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        let mut chunk = [0u8; 65536];
        let n = match peer.read(&mut chunk) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        ensure!(n > 0, "viewer disconnected");
        pending.extend_from_slice(&chunk[..n]);
        {
            let mut c = state.counters.lock().unwrap();
            c.received += n as u64;
            c.high_water = c.high_water.max(pending.len());
        }
        while pending.len() >= 4 {
            let length = u32::from_be_bytes(pending[..4].try_into()?) as usize;
            ensure!(
                length > 0 && length <= 16 * 1024 * 1024,
                "viewer frame length"
            );
            if pending.len() < length + 4 {
                break;
            }
            let value: Value = serde_json::from_slice(&pending[4..4 + length])?;
            pending.drain(..4 + length);
            {
                let mut c = state.counters.lock().unwrap();
                c.decoded += (length + 4) as u64;
                if let Some(frame) = value.get("state") {
                    c.frames += 1;
                    // Each delta carries complete changed rows. Remember each pane's observed
                    // phase marker, even when a later metadata-only delta is decoded first.
                    let panes = frame
                        .pointer("/state/panes")
                        .and_then(Value::as_object)
                        .context("frame panes")?;
                    for (id, pane) in panes {
                        let mut text = String::new();
                        if let Some(cells) = pane.get("cells") {
                            for cell in cells.as_array().context("cells")? {
                                if let Some(value) = cell.get("text") {
                                    text.push_str(value.as_str().context("cell text")?);
                                }
                            }
                        }
                        for marker in ["READY", "DONE_B01", "DONE_S01"] {
                            if text.contains(marker) {
                                c.markers
                                    .entry(marker.into())
                                    .or_default()
                                    .insert(id.clone());
                            }
                        }
                    }
                } else {
                    ensure!(
                        value == json!({"hello":{}})
                            || (value.as_object().is_some_and(|object| object.len() == 1)
                                && value
                                    .pointer("/bindings/bindings/bindings")
                                    .is_some_and(Value::is_object)),
                        "unexpected attachment message: {value}"
                    );
                }
            }
            if slow && !state.stop.load(Ordering::Acquire) {
                thread::park_timeout(Duration::from_millis(25));
            }
        }
    }
    Ok(())
}
impl Viewer {
    fn start(path: &Path, slow: bool) -> Result<Self> {
        let mut peer = local::connect(path, Instant::now() + Duration::from_secs(3))?;
        peer.set_write_timeout(Some(Duration::from_secs(3)))?;
        attachment::send(&mut peer, &json!({"type":"hello","rows":24,"columns":80}))?;
        peer.set_nonblocking(true)?;
        let state = Arc::new(Shared::default());
        let shared = state.clone();
        let thread = thread::spawn(move || {
            if let Err(e) = read_viewer(peer, slow, &shared)
                && !shared.stop.load(Ordering::Acquire)
            {
                shared.errors.lock().unwrap().push(format!("{e:#}"));
            }
        });
        Ok(Self {
            state,
            thread: Some(thread),
        })
    }
    fn counters(&self) -> Value {
        self.state.counters.lock().unwrap().value()
    }
    fn healthy(&self) -> Result<()> {
        let errors = self.state.errors.lock().unwrap();
        ensure!(errors.is_empty(), "viewer: {}", errors.join("; "));
        Ok(())
    }
    fn visible(&self, marker: &str, count: usize) -> Result<bool> {
        let c = self.state.counters.lock().unwrap();
        Ok(c.markers
            .get(marker)
            .is_some_and(|panes| panes.len() == count))
    }

    fn close(&mut self) -> Result<()> {
        self.state.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let end = Instant::now() + Duration::from_secs(3);
            while !t.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(10));
            }
            ensure!(t.is_finished(), "viewer reader did not stop");
            ensure!(t.join().is_ok(), "viewer panic");
        }
        self.healthy()
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        if self.thread.is_some()
            && let Err(e) = self.close()
        {
            eprintln!("viewer cleanup: {e:#}");
        }
    }
}
fn until(
    owners: &mut [(&str, Guard)],
    readers: &[Viewer],
    mut check: impl FnMut() -> Result<bool>,
) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(12);
    loop {
        for (name, owner) in owners.iter_mut() {
            ensure!(owner.0.try_wait()?.is_none(), "{name} exited");
        }
        for r in readers {
            r.healthy()?;
        }
        if check()? {
            return Ok(());
        }
        ensure!(Instant::now() < end, "performance fixture observation");
        thread::sleep(Duration::from_millis(10));
    }
}
fn sample(
    owners: &[(&str, Guard)],
    readers: &[Viewer],
    sampler: Option<&Path>,
    epoch: Instant,
) -> Result<Value> {
    let mut values = serde_json::Map::new();
    for (name, owner) in owners {
        let value = if let Some(sampler) = sampler {
            let mut c = Command::new(sampler);
            c.arg(owner.0.id().to_string());
            let r = process::output(c, Duration::from_secs(3), 1048576)?;
            ensure!(
                r.status.success(),
                "resource sample: {}",
                String::from_utf8_lossy(&r.stderr)
            );
            serde_json::from_slice(&r.stdout)?
        } else {
            Value::Null
        };
        values.insert((*name).into(), value);
    }
    Ok(
        json!({"monotonic":epoch.elapsed().as_secs_f64(),"owners":values,"viewers":readers.iter().map(Viewer::counters).collect::<Vec<_>>()}),
    )
}
fn case(
    fux: &Path,
    zor: &Path,
    worker: &Path,
    sampler: Option<&Path>,
    panes: usize,
    viewers: usize,
    slow: bool,
) -> Result<Value> {
    let mut root = Root::new("hperf-rs-", &[worker.to_str().context("worker")?.into()])?;
    root.env.insert("PATH".into(), "/usr/bin:/bin".into());
    root.env.remove("SHELL");
    let mut owners: Vec<(&str, Guard)> = Vec::new();
    let mut readers = Vec::new();
    let mut worker_pids = Vec::new();
    let epoch = Instant::now();
    let mut row =
        json!({"panes":panes,"viewers":viewers,"slow":slow,"geometry":[80,24],"phases":[]});
    let measured = (|| -> Result<()> {
        let mut c = root.command(fux);
        c.arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        owners.push(("fux", Guard(c.spawn()?)));
        let control = root.control();
        until(&mut owners, &readers, || Ok(control.exists()))?;
        for _ in 1..panes {
            local::rpc(
                &control,
                json!({"command":"split","id":1,"axis":"horizontal","argv":[worker],"final_retain_ms":60000}),
            )?;
        }
        for index in 0..viewers {
            readers.push(Viewer::start(
                &root.path().join("fux/default.attach.sock"),
                slow && index == viewers - 1,
            )?);
        }
        until(&mut owners, &readers, || {
            readers
                .iter()
                .map(|r| r.visible("READY", panes))
                .try_fold(true, |ok, v| Ok(ok && v?))
        })?;
        let listing = local::completed(&control, json!({"command":"list","id":1}))?;
        let mut pane_ids = Vec::new();
        let mut rects = Vec::new();
        for w in listing["workspaces"].as_array().context("workspaces")? {
            for t in w["tabs"].as_array().context("tabs")? {
                for p in t["panes"].as_array().context("panes")? {
                    pane_ids.push(p.get("id").context("pane ID")?.clone());
                    let pid = i32::try_from(p["pid"].as_u64().context("worker PID")?)?;
                    ensure!(pid > 1, "worker PID");
                    worker_pids.push(pid);
                    rects.push(p.get("geometry").context("geometry")?.clone());
                }
            }
        }
        row["pane_rects"] = json!(rects);
        let mut c = root.command(zor);
        c.arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        owners.push(("zor", Guard(c.spawn()?)));
        until(&mut owners, &readers, || {
            Ok(root.path().join("zor/control.sock").exists())
        })?;
        until(&mut owners, &readers, || {
            let mut c = root.command(zor);
            c.arg("status");
            let r = process::output(c, Duration::from_secs(3), 1048576)?;
            if !r.status.success() {
                return Ok(false);
            }
            let v: Value = serde_json::from_slice(&r.stdout)?;
            Ok(v.pointer("/snapshot/observations")
                .and_then(Value::as_array)
                .is_some_and(|a| a.len() == panes)
                && v.get("stale") == Some(&json!(false)))
        })?;
        thread::sleep(Duration::from_millis(300));
        for label in ["idle", "B01", "S01"] {
            let before = sample(&owners, &readers, sampler, epoch)?;
            let start = Instant::now();
            if label == "idle" {
                thread::sleep(Duration::from_secs(1));
            } else {
                for pane in &pane_ids {
                    local::rpc(
                        &control,
                        json!({"command":"send-keys","id":1,"pane":pane,"keys":format!("{label}\\n")}),
                    )?;
                }
                until(&mut owners, &readers, || {
                    readers
                        .iter()
                        .map(|r| r.visible(&format!("DONE_{label}"), panes))
                        .try_fold(true, |ok, v| Ok(ok && v?))
                })?;
            }
            let elapsed = start.elapsed().as_secs_f64() * 1000.;
            thread::sleep(Duration::from_millis(100));
            row["phases"].as_array_mut().unwrap().push(json!({"name":label,"visible_ms":if label=="idle"{Value::Null}else{json!(elapsed)},"workload_ms":elapsed,"before":before,"after":sample(&owners,&readers,sampler,epoch)?}));
        }
        row["passed"] = json!(true);
        Ok(())
    })();
    let mut failures = Vec::new();
    for r in &mut readers {
        if let Err(e) = r.close() {
            failures.push(format!("{e:#}"));
        }
    }
    let mut exits = serde_json::Map::new();
    for (name, owner) in owners.iter_mut().rev() {
        let result = (|| -> Result<()> {
            owner.0.terminate()?;
            let status = process::wait(&mut owner.0, Duration::from_secs(10))?;
            exits.insert((*name).into(), json!(status.code()));
            ensure!(status.success(), "{name}: {status}");
            Ok(())
        })();
        if let Err(e) = result {
            failures.push(format!("{e:#}"));
            if owner.0.try_wait()?.is_none() {
                if let Err(e) = owner.0.kill() {
                    failures.push(e.to_string());
                }
                if let Err(e) = process::wait(&mut owner.0, Duration::from_secs(3)) {
                    failures.push(e.to_string());
                }
            }
        }
    }
    row["owner_exits"] = Value::Object(exits);
    let gone = local::until(Duration::from_secs(5), || {
        for pid in &worker_pids {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), None) {
                Err(nix::errno::Errno::ESRCH) => {}
                Ok(()) => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Some(()))
    });
    row["worker_exit_confirmed"] = json!(gone.is_ok());
    if let Err(e) = gone {
        failures.push(format!("{e:#}"));
    }
    if !failures.is_empty() {
        eprintln!("cleanup: {}", failures.join("; "));
    }
    measured?;
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(row)
}
fn digest(p: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(p)?)))
}
fn compile(source: &Path, binary: &Path) -> Result<()> {
    let mut c = Command::new("/usr/bin/cc");
    c.arg(source).arg("-o").arg(binary);
    let r = process::output(c, Duration::from_secs(30), 1048576)?;
    ensure!(
        r.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    Ok(())
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len().is_multiple_of(2),
        "usage: headless-performance --fux PATH --zor PATH --output NEW_JSON [--repetitions 1..5]"
    );
    let mut flags = BTreeMap::new();
    for p in args.chunks_exact(2) {
        ensure!(
            ["--fux", "--zor", "--output", "--repetitions"].contains(&p[0].as_str()),
            "unknown argument"
        );
        flags.insert(p[0].as_str(), p[1].as_str());
    }
    let repetitions = flags.get("--repetitions").unwrap_or(&"3").parse::<u64>()?;
    ensure!((1..=5).contains(&repetitions), "repetition bounds");
    let output = Path::new(flags.get("--output").context("output")?);
    ensure!(!output.exists(), "use new output file");
    let fux = Path::new(flags.get("--fux").context("fux")?).canonicalize()?;
    let zor = Path::new(flags.get("--zor").context("zor")?).canonicalize()?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("repository")?;
    let mut c = Command::new("/usr/bin/uname");
    c.arg("-a");
    let r = process::output(c, Duration::from_secs(5), 65536)?;
    ensure!(r.status.success(), "platform");
    let mut report = json!({"schema":1,"geometry":[80,24],"platform":String::from_utf8(r.stdout)?.trim(),"synthetic":true,"provenance":{"harness_kind":"rust-headless-performance","harness_sha256":digest(&repository.join("tools/xtask/src/headless_performance.rs"))?,"fux_sha256":digest(&fux)?,"zor_sha256":digest(&zor)?,"sources":{}},"results":[]});
    for p in [
        "tools/xtask/src/headless_performance.rs",
        "tools/xtask/src/performance-worker.c",
        "tools/xtask/src/support/local.rs",
        "tools/xtask/src/support/attachment.rs",
        "tools/xtask/src/support/process.rs",
    ] {
        report["provenance"]["sources"][p] = json!(digest(&repository.join(p))?);
    }
    let root = tempfile::Builder::new()
        .prefix("hperf-tools-rs-")
        .tempdir_in("/tmp")?;
    let source = root.path().join("worker.c");
    fs::write(&source, include_bytes!("performance-worker.c"))?;
    let worker = root.path().join("worker");
    compile(&source, &worker)?;
    let sampler: Option<PathBuf> = if cfg!(target_os = "macos") {
        let sampler = root.path().join("sample");
        let source = repository.join("tools/comparisons/resource_sampler.c");
        compile(&source, &sampler)?;
        let mut c = Command::new(&sampler);
        c.arg("--calibrate");
        let r = process::output(c, Duration::from_secs(3), 1048576)?;
        ensure!(r.status.success(), "sampler calibration");
        report["calibration"] = serde_json::from_slice(&r.stdout)?;
        report["provenance"]["sampler_sha256"] = json!(digest(&source)?);
        Some(sampler)
    } else {
        None
    };
    report["repetitions"] = json!(repetitions);
    report["resource_limit"] = json!(
        "Owned-process CPU/RSS only; unavailable on this harness platform if null. No allocation or internal queue counters."
    );
    for repetition in 1..=repetitions {
        for (panes, viewers, slow) in [(1, 1, false), (1, 4, false), (4, 4, false), (4, 4, true)] {
            let mut row = case(
                &fux,
                &zor,
                &worker,
                sampler.as_deref(),
                panes,
                viewers,
                slow,
            )?;
            row["repetition"] = json!(repetition);
            report["results"].as_array_mut().unwrap().push(row);
            println!("{panes} {viewers} {slow} {repetition} passed");
            std::io::stdout().flush()?;
        }
    }
    crate::headless_evidence::validate(repository, &report)?;
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(&report)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?
        .write_all(&bytes)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reader_decodes_fragmented_frames_and_counts_received_bytes() -> Result<()> {
        let (client, mut server) = UnixStream::pair()?;
        client.set_nonblocking(true)?;
        let state = Arc::new(Shared::default());
        let shared = state.clone();
        let handle = thread::spawn(move || read_viewer(client, false, &shared));
        let mut bytes = Vec::new();
        for v in [
            json!({"hello":{}}),
            json!({"bindings":{"bindings":{"bindings":{},"prefix":1}}}),
            json!({"state":{"state":{"panes":{"1":{"cells":[{}, {"text":"DONE_B01"}]}}}}}),
            json!({"state":{"state":{"panes":{"2":{"cells":[{"text":"DONE_B01"}]}}}}}),
            json!({"state":{"state":{"panes":{}}}}),
        ] {
            let b = serde_json::to_vec(&v)?;
            bytes.extend(u32::try_from(b.len())?.to_be_bytes());
            bytes.extend(b);
        }
        for chunk in bytes.chunks(3) {
            server.write_all(chunk)?;
        }
        let observed = local::until(Duration::from_secs(2), || {
            let c = state.counters.lock().unwrap();
            Ok((c.frames == 3
                && c.markers
                    .get("DONE_B01")
                    .is_some_and(|panes| panes == &BTreeSet::from(["1".into(), "2".into()]))
                && c.decoded == bytes.len() as u64)
                .then_some(c.value()))
        });
        state.stop.store(true, Ordering::Release);
        handle.thread().unpark();
        let result = handle.join().map_err(|_| anyhow::anyhow!("reader panic"))?;
        let c = observed?;
        result?;
        ensure!(
            c["received_bytes"] == bytes.len() && c["decoded_frame_bytes"] == bytes.len(),
            "wire accounting"
        );
        Ok(())
    }
}
