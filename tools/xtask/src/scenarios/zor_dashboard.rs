//! Custom-state dashboard summaries, notifications, terminal restoration and service loss.
use crate::support::{
    contention::{RealClock, cli_busy, retry_busy},
    local::{Root, completed, until},
    logged::Logged,
    process::{self, Guard, OwnedProcess},
    pty,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

struct Ui {
    child: Option<Guard>,
    pty: nix::pty::OpenptyResult,
    output: Vec<u8>,
}
impl Ui {
    fn new() -> Result<Self> {
        let pty = pty::open(24, 100)?;
        nix::unistd::write(&pty.slave, b"x")?;
        let mut byte = [0];
        ensure!(
            nix::unistd::read(&pty.master, &mut byte)? == 1 && byte == *b"x",
            "PTY history initialization"
        );
        nix::fcntl::fcntl(
            &pty.master,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )?;
        Ok(Self {
            child: None,
            pty,
            output: Vec::new(),
        })
    }
    fn attributes(&self) -> Result<libc::termios> {
        let value: libc::termios = nix::sys::termios::tcgetattr(&self.pty.slave)?.into();
        #[cfg(target_os = "macos")]
        let value = {
            let mut value = value;
            value.c_lflag &= !libc::PENDIN;
            value
        };
        Ok(value)
    }
    fn flags(&self) -> Result<i32> {
        Ok(nix::fcntl::fcntl(
            &self.pty.slave,
            nix::fcntl::FcntlArg::F_GETFL,
        )?)
    }
    fn start(&mut self, root: &Root, zor: &Path, args: &[&str]) -> Result<()> {
        if let Some(child) = self.child.as_mut() {
            ensure!(child.0.try_wait()?.is_some(), "UI still running");
        }
        self.pump()?;
        self.output.clear();
        self.child = Some(Guard(
            root.command(zor)
                .args(args)
                .stdin(Stdio::from(self.pty.slave.try_clone()?))
                .stdout(Stdio::from(self.pty.slave.try_clone()?))
                .stderr(Stdio::from(self.pty.slave.try_clone()?))
                .spawn()?,
        ));
        Ok(())
    }
    fn pump(&mut self) -> Result<()> {
        let mut bytes = [0; 65536];
        loop {
            match nix::unistd::read(&self.pty.master, &mut bytes) {
                Ok(0) | Err(nix::errno::Errno::EAGAIN | nix::errno::Errno::EIO) => return Ok(()),
                Ok(n) => {
                    self.output.extend_from_slice(&bytes[..n]);
                    ensure!(
                        self.output.len() <= 16 * 1024 * 1024,
                        "dashboard output bound"
                    );
                }
                Err(nix::errno::Errno::EINTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    fn contains(&self, text: &str) -> bool {
        self.output
            .windows(text.len())
            .any(|w| w == text.as_bytes())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self
            .child
            .as_mut()
            .context("UI child")?
            .0
            .try_wait()?
            .is_none())
    }
    fn wait_text(&mut self, text: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.pump()?;
            if self.contains(text) {
                return Ok(());
            }
            ensure!(
                self.alive()? && Instant::now() < deadline,
                "missing {text}: {}",
                String::from_utf8_lossy(&self.output[self.output.len().saturating_sub(3000)..])
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn observe(&mut self, duration: Duration) -> Result<()> {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            ensure!(self.alive()?, "UI exited while observing");
            self.pump()?;
            std::thread::sleep(Duration::from_millis(20));
        }
        self.pump()
    }
    fn send(&self, text: &[u8]) -> Result<()> {
        ensure!(
            nix::unistd::write(&self.pty.master, text)? == text.len(),
            "short UI input"
        );
        Ok(())
    }
    fn wait_exit(&mut self, timeout: Duration) -> Result<()> {
        until(timeout, || {
            self.pump()?;
            Ok(self.child.as_mut().context("UI")?.0.try_wait()?)
        })
        .and_then(|s| {
            ensure!(s.success(), "UI exit {s}");
            self.pump()
        })
    }
    fn quit(&mut self, timeout: Duration) -> Result<()> {
        self.send(b"q")?;
        self.wait_exit(timeout)
    }
    fn resize(&self, rows: u16, columns: u16) -> Result<()> {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // This fixture owns the slave descriptor and initialized winsize.
        if unsafe { libc::ioctl(self.pty.slave.as_raw_fd(), libc::TIOCSWINSZ, &size) } == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        if let Some(child) = self.child.as_mut() {
            child.0.terminate()?;
        }
        if self.child.is_some() {
            self.wait_exit(Duration::from_secs(10))?;
        }
        Ok(())
    }
}
fn records(path: &Path) -> Result<Vec<Value>> {
    match fs::read_to_string(path) {
        Ok(text) => {
            ensure!(text.len() <= 1024 * 1024, "notification record bound");
            text.lines()
                .map(|line| Ok(serde_json::from_str(line)?))
                .collect()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}
fn reaped(record: &Value) -> Result<()> {
    let pid = i32::try_from(record["pid"].as_u64().context("notifier pid")?)?;
    ensure!(pid > 1, "invalid notifier pid");
    ensure!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None)
            == Err(nix::errno::Errno::ESRCH),
        "notifier was not reaped"
    );
    Ok(())
}
pub(super) fn run(fux: &Path, zor: &Path, notifier_binary: &Path) -> Result<()> {
    let root = Root::new("zdash-rs-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let instance = completed(&root.control(), json!({"id":1,"command":"list"}))?["instance"]
        .as_str()
        .context("instance")?
        .to_owned();
    let cli = |args: &[&str], ok: bool| -> Result<Value> {
        let reply = retry_busy(
            |remaining| {
                let mut command = root.command(zor);
                command.args(args);
                process::output(command, remaining.min(Duration::from_secs(12)), 1024 * 1024)
            },
            |r| Ok(cli_busy(r.status.code(), &r.stderr)),
            Instant::now() + Duration::from_secs(12),
            args == ["dashboard", "--once"],
            &RealClock,
        )?;
        ensure!(
            reply.status.success() == ok,
            "CLI {args:?}: {} {}",
            String::from_utf8_lossy(&reply.stdout),
            String::from_utf8_lossy(&reply.stderr)
        );
        if ok {
            Ok(serde_json::from_slice(&reply.stdout)?)
        } else {
            Ok(String::from_utf8(reply.stderr)?.into())
        }
    };
    let task_root = root.path().join("custom-tasks");
    let state = task_root.to_str().context("state root")?;
    let task = |args: &[&str]| {
        let mut command = vec!["--state-directory", state, "task"];
        command.extend_from_slice(args);
        cli(&command, true)
    };
    let git = |path: &Path, args: &[&str]| -> Result<()> {
        let mut command = root.command(Path::new("/usr/bin/git"));
        command.arg("-C").arg(path).args(args);
        let reply = process::output(command, Duration::from_secs(5), 1024 * 1024)?;
        ensure!(
            reply.status.success(),
            "fixture git: {}",
            String::from_utf8_lossy(&reply.stderr)
        );
        Ok(())
    };
    let rules = root.path().join("rules");
    fs::create_dir(&rules)?;
    fs::write(
        rules.join("test.toml"),
        "id='test'\n[[rules]]\nid='attention'\nstate='blocked'\nregion='title'\ncontains=['ATTENTION']\nvisible_blocker=true\n",
    )?;
    task(&[
        "start",
        "worker",
        "--title",
        "Review 界 changes",
        "--instance",
        &instance,
        "--workspace",
        "default",
        "--cwd",
        root.path().to_str().context("root")?,
        "--",
        "/bin/sh",
        "-c",
        "printf '\\033]2;ATTENTION\\007'; exec /bin/cat",
    ])?;
    task(&["require-check", "worker", "fail", "--", "/usr/bin/false"])?;
    ensure!(
        task(&[
            "check",
            "worker",
            "failed-check",
            "--requirement",
            "fail",
            "--",
            "/usr/bin/false"
        ])?["passed"]
            == false,
        "failed check passed"
    );
    let repo = root.path().join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["config", "user.name", "Fixture"])?;
    git(&repo, &["config", "user.email", "fixture@example.invalid"])?;
    fs::write(repo.join("source"), "input")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "input"])?;
    cli(
        &[
            "--state-directory",
            state,
            "worktree",
            "create",
            "artifact-tree",
            "--repo",
            repo.to_str().context("repo")?,
            "--branch",
            "artifact",
        ],
        true,
    )?;
    task(&[
        "start",
        "artifact",
        "--title",
        "Missing output",
        "--instance",
        &instance,
        "--workspace",
        "default",
        "--worktree",
        "artifact-tree",
        "--",
        "/bin/cat",
    ])?;
    task(&["require-artifact", "artifact", "report", "missing"])?;
    task(&["source-collect", "artifact", "artifact-source"])?;
    let capture = task(&[
        "check",
        "artifact",
        "diagnostic",
        "--source",
        "artifact-source",
        "--artifact",
        "report=missing-report",
        "--",
        "/usr/bin/true",
    ])?;
    ensure!(
        capture["passed"] == true
            && !capture["check"]["artifact_problems"]
                .as_object()
                .context("artifact problems")?
                .is_empty(),
        "missing artifact evidence"
    );
    let mut service_command = root.command(zor);
    service_command.args([
        "--state-directory",
        state,
        "--rules",
        rules.to_str().context("rules")?,
        "--agent",
        "test",
        "serve",
    ]);
    let mut service = Logged::spawn(service_command)?;
    let mut ui = Ui::new()?;
    let scenario = (|| -> Result<()> {
        until(Duration::from_secs(8), || {
            ensure!(service.child.0.try_wait()?.is_none(), "service exited");
            Ok(root.path().join("zor/control.sock").exists().then_some(()))
        })?;
        let view = until(Duration::from_secs(10), || {
            let view = cli(&["dashboard", "--once"], true)?;
            let blocked = view["rows"]
                .as_array()
                .context("rows")?
                .iter()
                .any(|row| row["status"] == "blocked");
            Ok(blocked.then_some(view))
        })?;
        ensure!(
            view["state_directory"] == state
                && !root.path().join("state/zor/journal.json").exists(),
            "dashboard ignored service state directory"
        );
        let rows = view["rows"].as_array().context("rows")?;
        let row = rows
            .iter()
            .find(|r| r["key"] == "task:worker")
            .context("worker row")?;
        ensure!(
            row["status"] == "checks-failed"
                && row["attention"] == true
                && row["task_outcome"] == "open"
                && row["label"].as_str().context("label")?.contains('界'),
            "worker summary"
        );
        let artifact = rows
            .iter()
            .find(|r| r["key"] == "task:artifact")
            .context("artifact row")?;
        ensure!(
            artifact["attention"] == true && artifact["status"] == "artifact-failed",
            "artifact summary"
        );
        let before = fs::read(task_root.join("journal.json"))?;
        cli(&["dashboard", "--once"], true)?;
        ensure!(
            fs::read(task_root.join("journal.json"))? == before,
            "dashboard wrote journal"
        );
        let original = ui.attributes()?;
        let flags = ui.flags()?;
        let notifier = root.path().join("notifier");
        fs::copy(notifier_binary, &notifier)?;
        fs::set_permissions(&notifier, fs::Permissions::from_mode(0o700))?;
        let notices = root.path().join("notices.jsonl");
        let mode = root.path().join("notification-mode");
        fs::write(&mode, "ok")?;
        let notifier = notifier.to_str().context("notifier")?;
        let notify = ["dashboard", "--notify", "--notification-command", notifier];
        cli(&["dashboard", "--once", "--notify"], false)?;
        cli(&["dashboard", "--notification-command", notifier], false)?;
        ui.start(
            &root,
            zor,
            &[
                "dashboard",
                "--bell",
                "--notify",
                "--notification-command",
                notifier,
            ],
        )?;
        ui.wait_text("checks-failed")?;
        ui.wait_text("blocked")?;
        ensure!(
            !ui.contains("界") && ui.contains("\x07"),
            "terminal width sanitization or bell missing"
        );
        ui.send(b"\r")?;
        ui.wait_text("Focused default pane")?;
        ui.send(b"a")?;
        ui.wait_text("attention only")?;
        ui.observe(Duration::from_millis(1200))?;
        ensure!(
            ui.output.iter().filter(|b| **b == 7).count() == 1,
            "unchanged attention repeated bell"
        );
        let first = until(Duration::from_secs(5), || {
            let r = records(&notices)?;
            Ok((!r.is_empty()).then_some(r))
        })?;
        ensure!(
            first.len() == 1
                && first[0]["args"].as_array().context("notice args")?.len() == 2
                && first[0]["args"][0] == "Zor needs attention"
                && first[0]["args"][1]
                    .as_str()
                    .context("body")?
                    .ends_with("Open zor dashboard for details."),
            "notification shape"
        );
        let encoded = serde_json::to_string(&first)?;
        ensure!(
            !encoded.contains("Review") && !encoded.contains("Missing output"),
            "notification disclosed task details"
        );
        ui.observe(Duration::from_millis(5200))?;
        ensure!(
            records(&notices)?.len() == 1
                && !ui.contains("NOTIFIER_STDOUT")
                && !ui.contains("NOTIFIER_STDERR"),
            "repeat notification or leaked notifier output"
        );
        ui.resize(3, 20)?;
        ui.observe(Duration::from_millis(150))?;
        ui.resize(24, 100)?;
        ui.quit(Duration::from_secs(10))?;
        ensure!(
            ui.contains("\x1b[?1049h")
                && ui.contains("\x1b[?1049l")
                && ui.contains("\x1b[?25h")
                && ui.attributes()? == original
                && ui.flags()? == flags,
            "terminal restoration failed: before={original:?} after={:?}; flags {flags} -> {}; enter={} leave={} cursor={}",
            ui.attributes()?,
            ui.flags()?,
            ui.contains("\x1b[?1049h"),
            ui.contains("\x1b[?1049l"),
            ui.contains("\x1b[?25h")
        );
        for (failure, message) in [
            ("fail", "notification command exited"),
            ("stall", "notification command timed out"),
        ] {
            fs::write(&mode, failure)?;
            ui.start(&root, zor, &notify)?;
            ui.wait_text(message)?;
            reaped(records(&notices)?.last().context("notice")?)?;
            ui.quit(Duration::from_secs(3))?;
            ensure!(ui.attributes()? == original, "failure terminal restoration");
        }
        let previous = records(&notices)?.len();
        ui.start(&root, zor, &notify)?;
        ui.wait_text("checks-failed")?;
        let latest = until(Duration::from_secs(5), || {
            let r = records(&notices)?;
            Ok((r.len() > previous).then_some(r))
        })?;
        ui.quit(Duration::from_secs(3))?;
        ensure!(ui.attributes()? == original, "quit terminal restoration");
        reaped(latest.last().context("in-flight notifier")?)?;
        ui.start(
            &root,
            zor,
            &[
                "dashboard",
                "--notify",
                "--notification-command",
                root.path()
                    .join("missing-notifier")
                    .to_str()
                    .context("missing notifier")?,
            ],
        )?;
        ui.wait_text("start notification command")?;
        ui.quit(Duration::from_secs(3))?;
        ensure!(ui.attributes()? == original, "spawn failure restoration");
        ui.start(&root, zor, &["dashboard"])?;
        ui.wait_text("checks-failed")?;
        ui.stop()?;
        ensure!(ui.attributes()? == original, "signal restoration");
        ensure!(
            task(&["launch-reconcile", "worker"])?["launch"]["phase"] == "attached",
            "UI stopped worker"
        );
        ui.start(&root, zor, &["dashboard"])?;
        ui.wait_text("checks-failed")?;
        service.child.0.terminate()?;
        ensure!(
            process::wait(&mut service.child.0, Duration::from_secs(10))?.success(),
            "service shutdown"
        );
        ui.output.clear();
        ui.wait_text("STALE")?;
        ui.wait_text("No matching tasks or observed panes")?;
        ui.quit(Duration::from_secs(10))?;
        ensure!(ui.attributes()? == original, "outage restoration");
        Ok(())
    })();
    let ui_stopped = ui.stop();
    let service_stopped = (|| -> Result<()> {
        service.child.0.terminate()?;
        ensure!(
            process::wait(&mut service.child.0, Duration::from_secs(10))?.success(),
            "service exit"
        );
        Ok(())
    })();
    let server_stopped = server.finish();
    let errors: Vec<_> = [scenario, ui_stopped, service_stopped, server_stopped]
        .into_iter()
        .filter_map(Result::err)
        .map(|e| format!("{e:#}"))
        .collect();
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS dashboard attention, bounded notifications, focus, terminal restoration and service loss"
    );
    Ok(())
}
