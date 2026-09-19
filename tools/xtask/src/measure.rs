//! Release-binary comparison on one host. No build or source inference occurs here.
//! Legacy and ECS protocols are adapted only at CLI/endpoint setup; timed input and
//! rendered output always traverse the shipped viewer, real controlling PTY and shell.
mod metrics;
mod terminal;

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use terminal::{Terminal, drive};

type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;
const COMMAND_BOUND: u64 = 16 * 1024 * 1024;
const HELP: &str = "measure --baseline-fux PATH --baseline-zor PATH --candidate-fux PATH --candidate-zor PATH --output NEW_DIR [--samples 5 --latency-samples 20 --window-ms 3000 --settle-ms 1000 --viewers 8 --lines 20000 --rows 24 --cols 80 --timeout-ms 30000]";
const FUX_CONFIG: &str = "default-command = { argv = [\"/bin/sh\", \"-i\"] }\n[history]\nscrollback-lines = 10000\n[final]\nretain-ms = 60000\n";

static CANCELLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
extern "C" fn cancel(_signal: nix::libc::c_int) {
    CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed);
}
pub(super) fn check_cancelled() -> Result<()> {
    if CANCELLED.load(std::sync::atomic::Ordering::Relaxed) {
        Err("measurement interrupted; partial artifacts retained".into())
    } else {
        Ok(())
    }
}
struct SignalGuard(Vec<(nix::sys::signal::Signal, nix::sys::signal::SigAction)>);
impl SignalGuard {
    fn install() -> Result<Self> {
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
        CANCELLED.store(false, std::sync::atomic::Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        let action = SigAction::new(
            SigHandler::Handler(cancel),
            SaFlags::empty(),
            SigSet::empty(),
        );
        for signal in [Signal::SIGINT, Signal::SIGTERM] {
            // SAFETY: handler only stores to a lock-free atomic and remains installed until drop.
            guard
                .0
                .push((signal, unsafe { sigaction(signal, &action) }?));
        }
        Ok(guard)
    }
}
impl Drop for SignalGuard {
    fn drop(&mut self) {
        for (signal, previous) in self.0.iter().rev() {
            // SAFETY: restoring the valid action returned by sigaction.
            let _ = unsafe { nix::sys::signal::sigaction(*signal, previous) };
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Protocol {
    Legacy,
    Rewrite,
}
#[derive(Clone, Serialize)]
struct Binaries {
    fux: PathBuf,
    zor: PathBuf,
    protocol: Protocol,
}
#[derive(Serialize)]
struct Config {
    baseline: Binaries,
    candidate: Binaries,
    output: PathBuf,
    samples: usize,
    latency_samples: usize,
    window_ms: u64,
    settle_ms: u64,
    viewers: usize,
    lines: u64,
    rows: u16,
    cols: u16,
    timeout_ms: u64,
}
impl Config {
    fn parse(args: Vec<String>) -> Result<Self> {
        if !args.len().is_multiple_of(2) {
            return Err(format!("missing option value; {HELP}").into());
        }
        let mut options = BTreeMap::new();
        for pair in args.chunks_exact(2) {
            if options.insert(pair[0].clone(), pair[1].clone()).is_some() {
                return Err(format!("duplicate option {}", pair[0]).into());
            }
        }
        let mut required = |key: &str| -> Result<PathBuf> {
            Ok(PathBuf::from(
                options
                    .remove(key)
                    .ok_or_else(|| format!("missing {key}; {HELP}"))?,
            ))
        };
        let baseline = Binaries {
            fux: required("--baseline-fux")?.canonicalize()?,
            zor: required("--baseline-zor")?.canonicalize()?,
            protocol: Protocol::Legacy,
        };
        let candidate = Binaries {
            fux: required("--candidate-fux")?.canonicalize()?,
            zor: required("--candidate-zor")?.canonicalize()?,
            protocol: Protocol::Rewrite,
        };
        let output = required("--output")?;
        let mut number = |key: &str, default: u64, min: u64, max: u64| -> Result<u64> {
            let n = options
                .remove(key)
                .map(|s| s.parse::<u64>())
                .transpose()?
                .unwrap_or(default);
            if !(min..=max).contains(&n) {
                return Err(format!("{key} must be {min}..={max}").into());
            }
            Ok(n)
        };
        let config = Self {
            baseline,
            candidate,
            output,
            samples: number("--samples", 5, 1, 100)? as usize,
            latency_samples: number("--latency-samples", 20, 1, 1000)? as usize,
            window_ms: number("--window-ms", 3000, 100, 60000)?,
            settle_ms: number("--settle-ms", 1000, 100, 30000)?,
            viewers: number("--viewers", 8, 2, 64)? as usize,
            lines: number("--lines", 20000, 1, 1_000_000)?,
            rows: number("--rows", 24, 10, 100)? as u16,
            cols: number("--cols", 80, 40, 300)? as u16,
            timeout_ms: number("--timeout-ms", 30000, 1000, 300000)?,
        };
        if !options.is_empty() {
            return Err(format!("unknown options: {:?}; {HELP}", options.keys()).into());
        }
        for bin in [
            &config.baseline.fux,
            &config.baseline.zor,
            &config.candidate.fux,
            &config.candidate.zor,
        ] {
            let meta = fs::metadata(bin)?;
            if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
                return Err(format!("not executable: {}", bin.display()).into());
            }
        }
        Ok(config)
    }
    fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

/// Main wires `measure::run(std::env::args().skip(2).collect())`; no other API or build step.
pub fn run(args: Vec<String>) -> Result<()> {
    if args == ["--help"] {
        println!("{HELP}");
        return Ok(());
    }
    let config = Config::parse(args)?;
    let _signals = SignalGuard::install()?;
    fs::create_dir(&config.output)?; // Refuse overwrite of previous evidence.
    fs::set_permissions(&config.output, fs::Permissions::from_mode(0o700))?;
    let mut provenance = provenance(&config)?;
    save(&config.output.join("provenance.json"), &provenance)?;
    let mut raw = File::create(config.output.join("samples.jsonl"))?;
    let mut records = Vec::new();
    let mut failures = 0usize;
    for sample in 0..config.samples {
        // Alternate order to expose, rather than systematically favor, cache/thermal drift.
        let order = if sample % 2 == 0 {
            ["baseline", "candidate"]
        } else {
            ["candidate", "baseline"]
        };
        for label in order {
            let bins = if label == "baseline" {
                &config.baseline
            } else {
                &config.candidate
            };
            let artifact = config.output.join(format!("{sample:03}-{label}"));
            fs::create_dir(&artifact)?;
            let begin = Instant::now();
            let result = sample_run(&config, bins, &artifact);
            let record = match result {
                Ok(measurements) => {
                    json!({"sample":sample, "label":label, "status":"measured", "elapsed_s":begin.elapsed().as_secs_f64(), "measurements":measurements, "artifact":artifact})
                }
                Err(error) => {
                    failures += 1;
                    json!({"sample":sample, "label":label, "status":"failed", "error":error.to_string(), "elapsed_s":begin.elapsed().as_secs_f64(), "artifact":artifact})
                }
            };
            serde_json::to_writer(&mut raw, &record)?;
            raw.write_all(b"\n")?;
            raw.sync_data()?;
            println!("measure sample {sample} {label}: {}", record["status"]);
            records.push(record);
            save(&config.output.join("summary.json"), &summary(&records))?;
            check_cancelled()?;
        }
    }
    let final_hashes = binary_provenance(&config)?;
    if provenance["binaries"] != final_hashes {
        failures += 1;
        provenance["binary_stability"] =
            json!({"status":"failed", "reason":"binary changed during run", "after":final_hashes});
    } else {
        provenance["binary_stability"] = json!({"status":"verified"});
    }
    save(&config.output.join("provenance.json"), &provenance)?;
    if failures != 0 {
        return Err(format!(
            "{failures} failed samples/provenance checks; partial raw evidence retained at {}",
            config.output.display()
        )
        .into());
    }
    println!(
        "Raw samples and distributions: {} (unavailable metrics are not successes)",
        config.output.display()
    );
    Ok(())
}

pub(super) struct Process(Child);
impl Process {
    fn alive(&mut self) -> Result<()> {
        if let Some(status) = self.0.try_wait()? {
            return Err(format!("owned process {} exited {status}", self.0.id()).into());
        }
        Ok(())
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
    fn signal(&self, signal: nix::sys::signal::Signal) {
        if let Ok(pid) = i32::try_from(self.pid()) {
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), signal);
        }
    }
    fn stop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        self.signal(nix::sys::signal::Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.signal(nix::sys::signal::Signal::SIGKILL);
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Bounded finite utility invocation. Files avoid pipe deadlock and leaked reader threads.
pub(super) fn output(mut command: Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut child = Process(command.spawn()?);
    let started = Instant::now();
    let status = loop {
        if stdout.metadata()?.len() > COMMAND_BOUND || stderr.metadata()?.len() > COMMAND_BOUND {
            return Err("utility output bound exceeded".into());
        }
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            return Err("utility deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    use std::io::{Seek, SeekFrom};
    stdout.seek(SeekFrom::Start(0))?;
    stderr.seek(SeekFrom::Start(0))?;
    let mut out = Vec::new();
    stdout.take(COMMAND_BOUND + 1).read_to_end(&mut out)?;
    if out.len() as u64 > COMMAND_BOUND {
        return Err("utility stdout bound exceeded".into());
    }
    if !status.success() {
        let mut error = String::new();
        stderr.take(65536).read_to_string(&mut error)?;
        return Err(format!("utility exited {status}: {error}").into());
    }
    Ok(out)
}

struct Stack {
    root: tempfile::TempDir,
    bins: Binaries,
    processes: Vec<Process>,
    terminals: Vec<Terminal>,
    known: Vec<metrics::Counter>,
    artifact: PathBuf,
    cleaned: bool,
}
impl Stack {
    fn new(bins: &Binaries, artifact: &Path) -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("fux-measure-")
            .tempdir_in("/tmp")?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        for dir in [
            "run",
            "home",
            "tmp",
            "config/fux",
            "state",
            "cache",
            "data",
            "bin",
        ] {
            fs::create_dir_all(root.path().join(dir))?;
        }
        symlink(&bins.fux, root.path().join("bin/fux"))?;
        symlink(&bins.zor, root.path().join("bin/zor"))?;
        let name = match bins.protocol {
            Protocol::Legacy => "config.toml",
            Protocol::Rewrite => "fux.toml",
        };
        fs::write(root.path().join("config/fux").join(name), FUX_CONFIG)?;
        Ok(Self {
            root,
            bins: bins.clone(),
            processes: Vec::new(),
            terminals: Vec::new(),
            known: Vec::new(),
            artifact: artifact.into(),
            cleaned: false,
        })
    }
    fn command(&self, bin: &Path) -> Command {
        let mut command = Command::new(bin);
        command
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                    self.root.path().join("bin").display()
                ),
            )
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("PS1", "BENCHREADY ")
            .env("PS2", "")
            .env("ENV", "/dev/null");
        for (name, dir) in [
            ("HOME", "home"),
            ("TMPDIR", "tmp"),
            ("XDG_RUNTIME_DIR", "run"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
        ] {
            command.env(name, self.root.path().join(dir));
        }
        command.current_dir(self.root.path().join("home"));
        command
    }
    fn spawn(&mut self, mut command: Command, log: &str) -> Result<(usize, Instant)> {
        let file = File::create(self.artifact.join(log))?;
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file);
        let index = self.processes.len();
        let started = Instant::now();
        self.processes.push(Process(command.spawn()?));
        Ok((index, started))
    }
    fn roots(&self) -> Vec<u32> {
        self.processes
            .iter()
            .map(Process::pid)
            .chain(self.terminals.iter().map(|t| t.process.pid()))
            .collect()
    }
    fn snapshot(&mut self) -> Result<Vec<metrics::Counter>> {
        let values = metrics::snapshot(&self.roots(), &self.known)?;
        self.known = values.clone();
        Ok(values)
    }
    fn healthy(&mut self) -> Result<()> {
        check_cancelled()?;
        for p in &mut self.processes {
            p.alive()?;
        }
        for t in &mut self.terminals {
            t.process.alive()?;
        }
        for name in ["fux.log", "zor.log", "active-zor-run.log"] {
            if fs::metadata(self.artifact.join(name)).is_ok_and(|m| m.len() > COMMAND_BOUND) {
                return Err(format!("product log bound exceeded: {name}").into());
            }
        }
        Ok(())
    }
    fn wait_file(&mut self, path: &Path, timeout: Duration, descriptor: bool) -> Result<()> {
        let start = Instant::now();
        loop {
            self.healthy()?;
            if path.exists() {
                if !descriptor {
                    return Ok(());
                }
                if let Ok(bytes) = fs::read(path)
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                    && value["http"]["port"].as_u64().is_some_and(|p| p != 0)
                    && (path
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|s| s != "fux")
                        || !value["attach"].is_null())
                {
                    return Ok(());
                }
            }
            if start.elapsed() >= timeout {
                return Err(format!("endpoint publication deadline: {}", path.display()).into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn viewer(&mut self, config: &Config) -> Result<()> {
        // No arguments is the supported default-workspace attachment CLI on both stacks.
        let command = self.command(&self.bins.fux);
        self.terminals
            .push(Terminal::spawn(command, config.rows, config.cols)?);
        Ok(())
    }
    fn mark(&mut self, marker: &str, config: &Config) -> Result<Vec<f64>> {
        let command = format!("printf '\\033[2J\\033[H{}\\n'\n", octal(marker));
        let begin = Instant::now();
        self.terminals[0].send(command.as_bytes(), config.timeout())?;
        let sent = begin.elapsed().as_secs_f64();
        let times = drive(&mut self.terminals, config.timeout(), Some(marker))?;
        Ok(times.into_iter().map(|time| time + sent).collect())
    }
    fn window(&mut self, config: &Config) -> Result<Value> {
        drive(
            &mut self.terminals,
            Duration::from_millis(config.settle_ms),
            None,
        )?;
        self.healthy()?;
        let before_started = Instant::now();
        let before = self.snapshot()?;
        let before_cost = before_started.elapsed();
        let started = Instant::now();
        drive(
            &mut self.terminals,
            Duration::from_millis(config.window_ms),
            None,
        )?;
        let actual = started.elapsed();
        let after_started = Instant::now();
        let after = self.snapshot()?;
        let after_cost = after_started.elapsed();
        self.healthy()?;
        let mut delta = metrics::delta(&before, &after, actual + (before_cost + after_cost) / 2);
        delta["observation_window_s"] = json!(actual.as_secs_f64());
        delta["snapshot_before_s"] = json!(before_cost.as_secs_f64());
        delta["snapshot_after_s"] = json!(after_cost.as_secs_f64());
        delta["counter_window_semantics"] = json!(
            "midpoint-to-midpoint approximation; counter sampling skew bounded by sum of snapshot durations"
        );
        Ok(delta)
    }
    fn persist_terminals(&self) -> Result<()> {
        for (i, terminal) in self.terminals.iter().enumerate() {
            fs::write(
                self.artifact.join(format!("viewer-{i}.ansi")),
                &terminal.raw,
            )?;
            fs::write(
                self.artifact.join(format!("viewer-{i}.screen.txt")),
                terminal.screen.text(),
            )?;
        }
        Ok(())
    }
    fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        self.cleaned = true;
        // Capture descendants before any parent is stopped; native birth counters guard PID reuse.
        let capture_error = match metrics::snapshot(&self.roots(), &self.known) {
            Ok(current) => {
                self.known = current;
                None
            }
            Err(error) => Some(error.to_string()),
        };
        for process in self.processes.iter_mut().rev() {
            process.stop();
        }
        for terminal in &mut self.terminals {
            terminal.process.stop();
        }
        for counter in &self.known {
            if metrics::same_process(counter) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(i32::try_from(counter.pid)?),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let remaining: Vec<_> = self
                .known
                .iter()
                .filter(|c| metrics::same_process(c))
                .map(|c| c.pid)
                .collect();
            if remaining.is_empty() {
                return match capture_error {
                    Some(error) => {
                        Err(format!("cleanup descendant enumeration incomplete: {error}").into())
                    }
                    None => Ok(()),
                };
            }
            if Instant::now() >= deadline {
                return Err(format!("owned processes remain after cleanup: {remaining:?}").into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Stack {
    fn drop(&mut self) {
        // Error paths must clean up as well; the explicit success path reports cleanup failure.
        let _ = self.cleanup();
    }
}

fn sample_run(config: &Config, bins: &Binaries, artifact: &Path) -> Result<Value> {
    let mut stack = Stack::new(bins, artifact)?;
    let mut measurements = json!({});
    let result = (|| -> Result<()> {
        let mut fux = stack.command(&bins.fux);
        fux.args(["serve", "--name", "default"]);
        if matches!(bins.protocol, Protocol::Rewrite) {
            fux.args(["--restore", "none"]);
        }
        let (_, start) = stack.spawn(fux, "fux.log")?;
        let endpoint = stack.root.path().join(match bins.protocol {
            Protocol::Legacy => "run/fux/default.attach.sock",
            Protocol::Rewrite => "run/fux/default.brp.json",
        });
        stack.wait_file(
            &endpoint,
            config.timeout(),
            matches!(bins.protocol, Protocol::Rewrite),
        )?;
        measurements["fux_endpoint_startup_s"] = json!(start.elapsed().as_secs_f64());
        save(&artifact.join("partial.json"), &measurements)?;
        stack.viewer(config)?;
        let start = stack.terminals[0].spawned_at;
        drive(&mut stack.terminals, config.timeout(), Some("BENCHREADY"))?;
        measurements["viewer_startup_to_shell_prompt_s"] = json!(start.elapsed().as_secs_f64());
        // Shell echo cannot satisfy later probes: the exact marker appears only after printf
        // decodes octal bytes, and echo is then disabled for the steady-state workload.
        stack.terminals[0].send(b"stty -echo; PS1=''\n", config.timeout())?;
        stack.mark("SETUPDONE", config)?;
        let mut zor = stack.command(&bins.zor);
        zor.arg("serve");
        if matches!(bins.protocol, Protocol::Rewrite) {
            zor.args(["--name", "default"]);
        }
        let (_, start) = stack.spawn(zor, "zor.log")?;
        let endpoint = stack.root.path().join(match bins.protocol {
            Protocol::Legacy => "run/zor/control.sock",
            Protocol::Rewrite => "run/zor/default.brp.json",
        });
        stack.wait_file(
            &endpoint,
            config.timeout(),
            matches!(bins.protocol, Protocol::Rewrite),
        )?;
        measurements["zor_endpoint_startup_s"] = json!(start.elapsed().as_secs_f64());
        measurements["quiescent"] = stack.window(config)?;
        save(&artifact.join("partial.json"), &measurements)?;
        let mut latency = Vec::new();
        for i in 0..config.latency_samples {
            latency.push(stack.mark(&format!("LAT{i:06}END"), config)?[0]);
        }
        measurements["input_to_visible_s"] = json!(latency);
        let burst = format!(
            "i=0; while [ $i -lt {} ]; do printf '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\\n'; i=$((i+1)); done; printf '{}\\n'\n",
            config.lines,
            octal("BURSTFINISHED")
        );
        let before_bytes: u64 = stack.terminals.iter().map(|t| t.bytes).sum();
        let start = Instant::now();
        stack.terminals[0].send(burst.as_bytes(), config.timeout())?;
        drive(
            &mut stack.terminals,
            config.timeout(),
            Some("BURSTFINISHED"),
        )?;
        let elapsed = start.elapsed().as_secs_f64();
        measurements["sustained_output"] = json!({"seconds":elapsed, "generated_lines":config.lines, "generated_bytes":config.lines * 65, "generated_bytes_per_second":config.lines as f64 * 65.0 / elapsed,
            "viewer_terminal_bytes":stack.terminals.iter().map(|t| t.bytes).sum::<u64>() - before_bytes,
            "completion":"final marker rendered; intermediate output may be coalesced by product, not every line painted"});
        measurements["post_output_processes"] = json!(stack.snapshot()?);
        save(&artifact.join("partial.json"), &measurements)?;
        // Real zor CLI supervision of a live process, with no provider fixtures/network.
        // Legacy timeout is milliseconds; rewrite timeout is seconds. A FIFO blocks the
        // same real shell until release, without periodic fixture wakeups or child churn.
        let worker = stack.root.path().join("home/worker.sh");
        let fifo = stack.root.path().join("home/active-release");
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )?;
        let mut release = fs::OpenOptions::new().read(true).write(true).open(&fifo)?;
        fs::write(
            &worker,
            "#!/bin/sh\nprintf ready > active-ready\nIFS= read -r release < active-release\nprintf 'SUPERVISIONFINISHED\\n'\n",
        )?;
        let timeout_secs =
            (config.timeout_ms + config.window_ms + config.settle_ms).div_ceil(1000) + 10;
        let timeout_value = match bins.protocol {
            Protocol::Legacy => timeout_secs * 1000,
            Protocol::Rewrite => timeout_secs,
        };
        let mut run = stack.command(&bins.zor);
        run.args([
            "run",
            "--workspace",
            "measure-active",
            "--timeout",
            &timeout_value.to_string(),
            "--",
            "/bin/sh",
        ])
        .arg(&worker);
        let (active, _) = stack.spawn(run, "active-zor-run.log")?;
        let ready = stack.root.path().join("home/active-ready");
        stack.wait_file(&ready, config.timeout(), false)?;
        measurements["active_zor_supervision"] = stack.window(config)?;
        release.write_all(b"release\n")?;
        let start = Instant::now();
        loop {
            if let Some(status) = stack.processes[active].0.try_wait()? {
                if !status.success() {
                    return Err(format!("zor supervised run exited {status}").into());
                }
                break;
            }
            if start.elapsed() >= config.timeout() {
                return Err("zor supervised completion deadline".into());
            }
            drive(&mut stack.terminals, Duration::from_millis(10), None)?;
        }
        let log = fs::read_to_string(artifact.join("active-zor-run.log"))?;
        if !log.contains("SUPERVISIONFINISHED") {
            return Err("zor did not return supervised final evidence".into());
        }
        stack.processes.remove(active);
        stack.known.retain(metrics::same_process);
        measurements["active_supervision_final_evidence"] = json!("SUPERVISIONFINISHED");
        for _ in 1..config.viewers {
            stack.viewer(config)?;
        }
        stack.mark("MANYVIEWERSREADY", config)?;
        measurements["many_viewers"] = stack.window(config)?;
        let mut many_latency = Vec::new();
        for i in 0..config.latency_samples {
            many_latency.push(stack.mark(&format!("MANY{i:06}END"), config)?);
        }
        measurements["many_viewers_input_to_visible_s"] = json!(many_latency);
        measurements["terminal_artifacts"] = json!(stack.terminals.iter().enumerate().map(|(i,t)| json!({"viewer":i,"observed_bytes":t.bytes,"retained_bytes":t.raw.len(),"truncated":t.bytes > t.raw.len() as u64})).collect::<Vec<_>>());
        save(&artifact.join("partial.json"), &measurements)?;
        Ok(())
    })();
    let terminal_artifacts = stack.persist_terminals();
    let cleanup = stack.cleanup();
    save(
        &artifact.join("cleanup.json"),
        &match &cleanup {
            Ok(()) => json!({"status":"verified"}),
            Err(e) => json!({"status":"failed","error":e.to_string()}),
        },
    )?;
    result?;
    terminal_artifacts?;
    cleanup?;
    Ok(measurements)
}

fn octal(text: &str) -> String {
    text.bytes().map(|b| format!("\\{b:03o}")).collect()
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    fs::rename(temporary, path)?;
    Ok(())
}
fn binary_provenance(config: &Config) -> Result<Value> {
    let mut result = json!({});
    for (label, bins) in [
        ("baseline", &config.baseline),
        ("candidate", &config.candidate),
    ] {
        for (app, path) in [("fux", &bins.fux), ("zor", &bins.zor)] {
            let mut hash = if cfg!(target_os = "macos") {
                let mut c = Command::new("/usr/bin/shasum");
                c.args(["-a", "256"]);
                c
            } else {
                Command::new("sha256sum")
            };
            hash.arg(path);
            let hash = String::from_utf8(output(hash, Duration::from_secs(60))?)?;
            let sha256 = hash
                .split_whitespace()
                .next()
                .ok_or("empty SHA256 output")?;
            if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid SHA256 output".into());
            }
            let mut version = Command::new(path);
            version
                .arg("--version")
                .env_clear()
                .env("PATH", "/usr/bin:/bin");
            let version = match output(version, Duration::from_secs(5)) {
                Ok(bytes) => {
                    json!({"status":"reported", "text":String::from_utf8_lossy(&bytes).trim()})
                }
                Err(e) => json!({"status":"unavailable", "reason":e.to_string()}),
            };
            result[label][app] = json!({"path":path,"bytes":fs::metadata(path)?.len(),"sha256":sha256,"version":version,"build_mode":"caller-supplied release binary; optimization flags not inferable from executable"});
        }
    }
    Ok(result)
}
fn provenance(config: &Config) -> Result<Value> {
    let mut uname = Command::new("/usr/bin/uname");
    uname.arg("-a");
    let hardware = if cfg!(target_os = "macos") {
        let mut c = Command::new("/usr/sbin/sysctl");
        c.args([
            "hw.model",
            "hw.memsize",
            "hw.ncpu",
            "machdep.cpu.brand_string",
        ]);
        output(c, Duration::from_secs(5)).map(|b| String::from_utf8_lossy(&b).into_owned())
    } else {
        fs::read_to_string("/proc/cpuinfo").map_err(Into::into)
    };
    Ok(json!({
        "schema":1,"unix_time_s":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        "config":config,"binaries":binary_provenance(config)?,
        "host":String::from_utf8_lossy(&output(uname, Duration::from_secs(5))?),
        "hardware":match hardware { Ok(text)=>json!(text), Err(e)=>json!({"status":"unavailable","reason":e.to_string()}) },
        "settings":{"term":"xterm-256color","locale":"C","shell":"/bin/sh -i","xdg":"fresh private root per sample","order":"alternating baseline/candidate; no concurrent benchmark cases","cpu_affinity":"not pinned","thermal_power_state":"not controlled; caller must hold hardware/power/background workload constant","cache":"warm filesystem cache uncontrolled; fresh application state, not cold disk","startup_observation_quantum_ms":1},
        "fux_config_toml":FUX_CONFIG,
        "measurement_contract":{
            "startup":"foreground process launch through endpoint publication; separately viewer launch to rendered real shell prompt; XDG, log and PTY allocation setup excluded",
            "latency":"write first input byte to complete ASCII marker in viewer terminal cell grid; includes shell execution and harness decode/scheduling, excludes CLI/RPC setup; not physical photons",
            "quiescent":"fux+zor servers, one viewer, one idle shell; no measurement RPC during window",
            "active":"same stack plus real zor run supervising /bin/sh blocked on a release FIFO; includes runner CLI, server and shell descendants; product default supervision intervals retained",
            "memory":"all owned live descendants including viewer, server, shells, supervisor CLI; boundary RSS sum not peak/PSS, shared pages double-counted",
            "cpu":"birth-identity-matched processes alive at both boundaries; transient descendants entirely between snapshots cannot be accounted, membership changes explicitly unavailable",
            "wakeups":"macOS proc_pid_rusage interrupt wakeups only; Linux per-thread voluntary context-switch proxy only; total scheduler wakeups explicitly unavailable without tracing",
            "observation_overhead":"blocking poll, no fixed latency polling period; process counters only at window boundaries; raw terminal bytes retained bounded in memory and flushed outside timed paths",
            "comparison":"no superiority claim; ratio distributions are descriptive only; failed and unavailable samples counted separately"
        },
        "protocol_adapters":{"baseline":"legacy fux config.toml + Unix attachment endpoint; zor control.sock and run timeout milliseconds","candidate":"ECS fux.toml + BRP descriptors; zor run timeout seconds; both use their own real no-argument fux viewer CLI"}
    }))
}

fn distribution(mut values: Vec<f64>) -> Value {
    values.retain(|x| x.is_finite());
    if values.is_empty() {
        return json!({"status":"unavailable","n":0});
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    let median = if n.is_multiple_of(2) {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    };
    let stddev = if n > 1 {
        Some((values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt())
    } else {
        None
    };
    let mut deviations: Vec<f64> = values.iter().map(|v| (v - median).abs()).collect();
    deviations.sort_by(f64::total_cmp);
    let mad = if n.is_multiple_of(2) {
        (deviations[n / 2 - 1] + deviations[n / 2]) / 2.0
    } else {
        deviations[n / 2]
    };
    json!({"status":"measured","n":n,"min":values[0],"max":values[n-1],"mean":mean,"median":median,"p95_nearest_rank":values[(n as f64*0.95).ceil() as usize-1],"sample_stddev":stddev,"median_absolute_deviation":mad,"coefficient_of_variation":stddev.filter(|_| mean!=0.0).map(|s|s/mean),"sorted_samples":values})
}
fn flatten(value: &Value, path: &str, result: &mut BTreeMap<String, Vec<f64>>) {
    match value {
        Value::Number(n) => {
            if let Some(n) = n.as_f64() {
                result.entry(path.into()).or_default().push(n);
            }
        }
        Value::Array(array) if path.ends_with("input_to_visible_s") => {
            for v in array {
                if let Some(n) = v.as_f64() {
                    result.entry(path.into()).or_default().push(n);
                } else if let Some(row) = v.as_array() {
                    // Many-viewer consumer latency is worst viewer per input, not pooled viewers.
                    if let Some(max) = row.iter().filter_map(Value::as_f64).max_by(f64::total_cmp) {
                        result.entry(path.into()).or_default().push(max);
                    }
                }
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if !matches!(
                    key.as_str(),
                    "before" | "after" | "post_output_processes" | "terminal_artifacts"
                ) {
                    flatten(value, &format!("{path}/{key}"), result);
                }
            }
        }
        _ => {}
    }
}
fn unavailable(value: &Value, path: &str, result: &mut BTreeMap<String, Vec<String>>) {
    if let Value::Object(object) = value {
        if value["status"] == "unavailable" {
            result
                .entry(path.into())
                .or_default()
                .push(value["reason"].as_str().unwrap_or("unspecified").into());
        } else {
            for (key, value) in object {
                unavailable(value, &format!("{path}/{key}"), result);
            }
        }
    }
}

fn summary(records: &[Value]) -> Value {
    let mut report = json!({"schema":1,"claims":"descriptive distributions only; no significance or superiority inferred","labels":{},"candidate_over_baseline_median":{}});
    let mut by_label = BTreeMap::new();
    for label in ["baseline", "candidate"] {
        let mut values = BTreeMap::new();
        let mut missing = BTreeMap::new();
        let mut measured = 0;
        let mut failed = 0;
        for record in records.iter().filter(|r| r["label"] == label) {
            if record["status"] == "measured" {
                measured += 1;
                flatten(&record["measurements"], "", &mut values);
                unavailable(&record["measurements"], "", &mut missing);
            } else {
                failed += 1;
            }
        }
        let distributions: BTreeMap<_, _> = values
            .into_iter()
            .map(|(key, values)| (key, distribution(values)))
            .collect();
        report["labels"][label] = json!({"measured_samples":measured,"failed_samples":failed,"distributions":distributions,"unavailable_per_sample_reasons":missing,"total_scheduler_wakeups":{"status":"unavailable","reason":"no scheduler tracing; supported proxy counters remain named as such"}});
        by_label.insert(label, distributions);
    }
    for (metric, candidate) in &by_label["candidate"] {
        if let Some(baseline) = by_label["baseline"].get(metric)
            && let Some((b, c)) = baseline["median"]
                .as_f64()
                .zip(candidate["median"].as_f64())
                .filter(|(b, _)| *b != 0.0)
        {
            report["candidate_over_baseline_median"][metric] = json!(c / b);
        }
    }
    report
}
