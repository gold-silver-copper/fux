//! One disposable fux+zor owner: private XDG directories under a temporary root, a `fux serve`
//! and a `zor serve` whose descriptors are found the way the product finds them, and CLI
//! invocations inheriting exactly that environment. Nothing here touches the user's own
//! servers: every location is under the root, `FUX_BRP`/`ZOR_BRP` are cleared.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::brp::Descriptor;
use super::{Binaries, Result, err, until};

/// How long a server gets to publish a usable descriptor.
const START: Duration = Duration::from_secs(20);
/// Whole-call deadline for one CLI invocation.
const CLI: Duration = Duration::from_secs(30);
const OUTPUT_BOUND: usize = 4 * 1024 * 1024;

/// A server is terminated gracefully so its owned PTYs/process groups are cleaned up before
/// its private runtime directory disappears; a bounded deadline still reaps a wedged server.
pub struct Guard(pub Child);

impl Drop for Guard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            if let Ok(pid) = i32::try_from(self.0.id()) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(5) {
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub struct Stack {
    pub label: &'static str,
    root: tempfile::TempDir,
    bins: Binaries,
    zor: Option<Guard>,
    fux: Option<Guard>,
}

impl Stack {
    pub fn start(label: &'static str, bins: &Binaries) -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix(&format!("zor-scn-{}-", label.to_ascii_lowercase()))
            .tempdir_in("/tmp")?;
        for dir in ["run", "config", "state", "home", "tmp"] {
            std::fs::create_dir_all(root.path().join(dir))?;
        }
        let mut stack = Self {
            label,
            root,
            bins: bins.clone(),
            zor: None,
            fux: None,
        };
        stack.start_fux()?;
        stack.start_zor()?;
        Ok(stack)
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.path().join("run")
    }

    pub fn config_dir(&self) -> PathBuf {
        self.path().join("config")
    }

    pub fn fux_descriptor_path(&self) -> PathBuf {
        self.runtime_dir().join("fux/default.brp.json")
    }

    pub fn zor_descriptor_path(&self) -> PathBuf {
        self.runtime_dir().join("zor/default.brp.json")
    }

    pub fn fux(&self) -> Result<Descriptor> {
        Descriptor::read(&self.fux_descriptor_path())
    }

    pub fn zor(&self) -> Result<Descriptor> {
        Descriptor::read(&self.zor_descriptor_path())
    }

    /// A command with this stack's private environment and the built binaries first on PATH.
    pub fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command.env_clear();
        for name in [
            "PATH",
            "LANG",
            "LC_ALL",
            "SHELL",
            "USER",
            "LOGNAME",
            "RUST_BACKTRACE",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![self.bins.dir.clone()];
        paths.extend(std::env::split_paths(&path));
        if let Ok(joined) = std::env::join_paths(paths) {
            command.env("PATH", joined);
        }
        command
            .env("HOME", self.path().join("home"))
            .env("TMPDIR", self.path().join("tmp"))
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env("XDG_CONFIG_HOME", self.config_dir())
            .env("XDG_STATE_HOME", self.path().join("state"))
            .env("TERM", "xterm-256color")
            .env_remove("FUX_BRP")
            .env_remove("ZOR_BRP")
            .current_dir(self.path().join("home"));
        command
    }

    fn start_fux(&mut self) -> Result<()> {
        let log = std::fs::File::create(self.path().join("fux-serve.log"))?;
        let child = self
            .command(&self.bins.fux)
            .args(["serve", "--name", "default", "--restore", "none"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        self.fux = Some(Guard(child));
        let path = self.fux_descriptor_path();
        until(START, "fux descriptor", || {
            self.check_alive()?;
            if !path.is_file() {
                return Ok(None);
            }
            let descriptor = Descriptor::read(&path)?;
            let ready = descriptor.raw.get("attach").is_some_and(|a| !a.is_null())
                && descriptor.alive("fux");
            Ok(ready.then_some(()))
        })
    }

    pub fn start_zor(&mut self) -> Result<()> {
        let log = std::fs::File::options()
            .create(true)
            .append(true)
            .open(self.path().join("zor-serve.log"))?;
        let child = self
            .command(&self.bins.zor)
            .args(["serve", "--name", "default"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        self.zor = Some(Guard(child));
        let path = self.zor_descriptor_path();
        until(START, "zor descriptor", || {
            self.check_alive()?;
            if !path.is_file() {
                return Ok(None);
            }
            Ok(Descriptor::read(&path)?.alive("zor").then_some(()))
        })
    }

    /// `SIGKILL` to the owned zor; its descriptor may linger (no destructor ran).
    pub fn kill_zor(&mut self) -> Result<u32> {
        let mut guard = self.zor.take().ok_or_else(|| err("zor not running"))?;
        let pid = guard.0.id();
        guard.0.kill()?;
        guard.0.wait()?;
        Ok(pid)
    }

    /// Signal only our child and observe its real exit; `Guard` still cleans up on timeout.
    pub fn terminate(
        &mut self,
        prefix: &str,
        deadline: Duration,
    ) -> Result<(u32, std::process::ExitStatus, Duration)> {
        let child = match prefix {
            "fux" => &mut self.fux,
            "zor" => &mut self.zor,
            _ => return Err(err("unknown owned server")),
        };
        let guard = child
            .as_mut()
            .ok_or_else(|| err(format!("{prefix} not running")))?;
        if let Some(status) = guard.0.try_wait()? {
            return Err(err(format!("{prefix} exited before SIGTERM: {status}")));
        }
        let pid = guard.0.id();
        let started = Instant::now();
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(pid)?),
            nix::sys::signal::Signal::SIGTERM,
        )?;
        let status = until(deadline, &format!("{prefix} SIGTERM exit"), || {
            guard.0.try_wait().map_err(Into::into)
        })?;
        let elapsed = started.elapsed();
        child.take();
        Ok((pid, status, elapsed))
    }

    pub fn check_alive(&mut self) -> Result<()> {
        if let Some(fux) = &mut self.fux
            && fux.0.try_wait()?.is_some()
        {
            return Err(format!(
                "{}: owned fux exited: {}",
                self.label,
                self.log("fux-serve.log")
            )
            .into());
        }
        if let Some(zor) = &mut self.zor
            && zor.0.try_wait()?.is_some()
        {
            return Err(format!(
                "{}: owned zor exited: {}",
                self.label,
                self.log("zor-serve.log")
            )
            .into());
        }
        Ok(())
    }

    pub fn log(&self, name: &str) -> String {
        use std::io::{Seek, SeekFrom};
        let Ok(mut file) = std::fs::File::open(self.path().join(name)) else {
            return String::new();
        };
        let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let _ = file.seek(SeekFrom::Start(length.saturating_sub(64 * 1024)));
        let mut bytes = Vec::new();
        let _ = file.take(64 * 1024).read_to_end(&mut bytes);
        let text = String::from_utf8_lossy(&bytes);
        let tail: Vec<&str> = text.lines().rev().take(12).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    /// Runs one CLI invocation to completion under the deadline.
    pub(super) fn invoke(&self, binary: &Path, args: &[&str]) -> Result<Output> {
        let mut child = self
            .command(binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| err("stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| err("stderr"))?;
        let out = std::thread::spawn(move || read_bounded(stdout));
        let err_ = std::thread::spawn(move || read_bounded(stderr));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() > CLI {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    format!("{} {args:?}: no exit within {CLI:?}", binary.display()).into(),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let stdout = out.join().map_err(|_| err("stdout reader"))??;
        let stderr = err_.join().map_err(|_| err("stderr reader"))??;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    /// A successful CLI call's stdout.
    pub fn run(&self, binary: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let output = self.invoke(binary, args)?;
        if !output.status.success() {
            return Err(format!(
                "{}: {} {args:?} exited {}: {}",
                self.label,
                binary.file_name().unwrap_or_default().to_string_lossy(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        Ok(output.stdout)
    }

    /// A CLI call that must fail; its stderr.
    pub fn fail(&self, binary: &Path, args: &[&str]) -> Result<String> {
        let output = self.invoke(binary, args)?;
        if output.status.success() {
            return Err(format!(
                "{}: {args:?} unexpectedly succeeded: {}",
                self.label,
                String::from_utf8_lossy(&output.stdout).trim()
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&output.stderr).into_owned())
    }

    pub fn zor_json(&self, args: &[&str]) -> Result<Value> {
        let out = self.run(&self.bins.zor, args)?;
        serde_json::from_slice(&out).map_err(|e| {
            format!(
                "zor {args:?}: not JSON ({e}): {}",
                String::from_utf8_lossy(&out)
            )
            .into()
        })
    }

    pub fn zor_run(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.run(&self.bins.zor, args)
    }

    pub fn zor_fail(&self, args: &[&str]) -> Result<String> {
        self.fail(&self.bins.zor, args)
    }

    pub fn zor_binary(&self) -> &Path {
        &self.bins.zor
    }

    pub fn fux_binary(&self) -> &Path {
        &self.bins.fux
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // Field declaration order would remove the TempDir first. Controllers must stop
        // before fux, and fux must get a chance to reap its PTY children before root removal.
        drop(self.zor.take());
        drop(self.fux.take());
    }
}

fn read_bounded(mut reader: impl std::io::Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(OUTPUT_BOUND as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > OUTPUT_BOUND {
        return Err(err("CLI output bound exceeded"));
    }
    Ok(bytes)
}
