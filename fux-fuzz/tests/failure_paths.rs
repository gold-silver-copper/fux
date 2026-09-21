use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fux-fuzz-fixture-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn spawn(&self, script: &str, seconds: u64) -> Result<Child> {
        let binary = self.0.join("fixture");
        fs::write(&binary, script)?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
        Ok(Command::new(env!("CARGO_BIN_EXE_fux-fuzz"))
            .args([
                "--scenario",
                "startup",
                "--seconds",
                &seconds.to_string(),
                "--fux",
            ])
            .arg(binary)
            .arg("--output")
            .arg(self.0.join("runs"))
            .stdout(fs::File::create(self.0.join("harness.stdout"))?)
            .stderr(fs::File::create(self.0.join("harness.stderr"))?)
            .stdin(Stdio::null())
            .spawn()?)
    }
    fn bundle(&self) -> Result<PathBuf> {
        fs::read_dir(self.0.join("runs"))?
            .next()
            .ok_or("no bundle")?
            .map(|entry| entry.path())
            .map_err(Into::into)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn finish(child: &mut Child) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait()? {
            assert!(!status.success());
            return Ok(());
        }
        if Instant::now() >= end {
            child.kill()?;
            return Err("harness exceeded fixture safety deadline".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
fn verify_bundle(path: &Path) -> Result<()> {
    let summary: Value = serde_json::from_slice(&fs::read(path.join("summary.json"))?)?;
    assert!(
        summary
            .get("failures")
            .and_then(Value::as_u64)
            .is_some_and(|n| n > 0)
    );
    assert!(path.join("trace.json").is_file());
    assert!(fs::read_to_string(path.join("replay.txt"))?.contains("--replay"));
    let mut checked = 0;
    for entry in fs::read_dir(path)? {
        let case = entry?.path();
        if !case.is_dir() {
            continue;
        }
        let pid: i32 = fs::read_to_string(case.join("fixture.pid"))?
            .trim()
            .parse()?;
        assert_eq!(
            kill(Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH),
            "fixture PID {pid} survived harness cleanup"
        );
        assert!(case.join("events.jsonl").is_file());
        assert!(fs::read_to_string(case.join("server.stderr"))?.contains("controlled fixture"));
        // No invented frontend snapshot: these fixtures never accept attach.
        assert!(!case.join("frontend-0.ansi").exists());
        checked += 1;
    }
    assert!(checked > 0);
    Ok(())
}
#[test]
fn early_exit_is_reported_and_logs_survive_teardown() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut child = fixture.spawn(
        "#!/bin/sh\necho $$ > fixture.pid\necho 'controlled fixture: early exit' >&2\nexit 17\n",
        10,
    )?;
    finish(&mut child)?;
    verify_bundle(&fixture.bundle()?)?;
    assert!(
        fs::read_to_string(fixture.0.join("harness.stderr"))?.contains("premature server exit")
    );
    Ok(())
}
#[test]
fn hung_startup_is_bounded_and_owned_process_is_reaped() -> Result<()> {
    let fixture = Fixture::new()?;
    let start = Instant::now();
    let mut child = fixture.spawn("#!/bin/sh\necho $$ > fixture.pid\necho 'controlled fixture: hanging' >&2\nexec /bin/sleep 60\n", 2)?;
    finish(&mut child)?;
    assert!(start.elapsed() < Duration::from_secs(10));
    verify_bundle(&fixture.bundle()?)?;
    assert!(fs::read_to_string(fixture.0.join("harness.stderr"))?.contains("deadline exceeded"));
    Ok(())
}
#[test]
fn interruption_preserves_failure_and_cleans_up() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut child = fixture.spawn("#!/bin/sh\necho $$ > fixture.pid\necho 'controlled fixture: interrupted' >&2\nexec /bin/sleep 60\n", 60)?;
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if fixture
            .bundle()
            .is_ok_and(|p| p.join("case-000/fixture.pid").exists())
        {
            break;
        }
        if Instant::now() >= end {
            child.kill()?;
            return Err("fixture did not start".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM)?;
    finish(&mut child)?;
    verify_bundle(&fixture.bundle()?)?;
    assert!(fs::read_to_string(fixture.0.join("harness.stderr"))?.contains("interrupted"));
    Ok(())
}
