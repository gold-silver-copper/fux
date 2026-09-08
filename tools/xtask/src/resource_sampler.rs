//! Owned-process macOS sampler calibration; independent of production binaries.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::process::{self, Guard};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::fd::{AsFd, AsRawFd},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn worker() -> Result<()> {
    println!("ready");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    ensure!(!line.is_empty(), "allocation command missing");
    let mut allocation = vec![0u8; 32 * 1024 * 1024];
    for offset in (0..allocation.len()).step_by(4096) {
        allocation[offset] = 1;
    }
    std::hint::black_box(&allocation);
    println!("allocated");
    std::io::stdout().flush()?;
    line.clear();
    std::io::stdin().read_line(&mut line)?;
    ensure!(!line.is_empty(), "exit command missing");
    std::hint::black_box(&allocation);
    Ok(())
}
fn line(pipe: &mut std::process::ChildStdout, expected: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut bytes = Vec::new();
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .context("owned worker readiness timeout")?;
        let mut poll = [nix::poll::PollFd::new(
            pipe.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        match nix::poll::poll(&mut poll, u16::try_from(left.as_millis().clamp(1, 5000))?) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        let mut byte = [0];
        let count = pipe.read(&mut byte)?;
        ensure!(count == 1, "worker closed before readiness");
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
        ensure!(bytes.len() < 128, "worker readiness line bound");
    }
    ensure!(
        String::from_utf8(bytes)?.trim() == expected,
        "unexpected worker readiness"
    );
    Ok(())
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len() == 2 && args[0] == "--output",
        "usage: fux-xtask resource-sampler-check --output PATH"
    );
    ensure!(cfg!(target_os = "macos"), "resource sampler requires macOS");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("repository root")?
        .join("tools/comparisons/resource_sampler.c");
    let temp = tempfile::Builder::new()
        .prefix("resource-sampler-rs-")
        .tempdir_in("/tmp")?;
    let binary = temp.path().join("sample");
    let mut compile = Command::new("/usr/bin/clang");
    compile
        .args(["-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&binary);
    let result = process::output(compile, Duration::from_secs(30), 1024 * 1024)?;
    ensure!(
        result.status.success(),
        "sampler compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let invoke = |arg: &str| -> Result<Value> {
        let mut c = Command::new(&binary);
        c.arg(arg);
        let r = process::output(c, Duration::from_secs(5), 1024 * 1024)?;
        ensure!(
            r.status.success(),
            "sampler {arg}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        Ok(serde_json::from_slice(&r.stdout)?)
    };
    let calibration = (0..3)
        .map(|_| invoke("--calibrate"))
        .collect::<Result<Vec<_>>>()?;
    for arg in ["0", "1", "-1", "123junk", "2147483648", ""] {
        let mut c = Command::new(&binary);
        c.arg(arg);
        let r = process::output(c, Duration::from_secs(5), 1024 * 1024)?;
        ensure!(
            r.status.code() == Some(2),
            "invalid PID {arg:?}: {}",
            r.status
        );
    }
    let mut child = Guard(
        Command::new(std::env::current_exe()?)
            .arg("resource-sampler-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let measured = (|| -> Result<(Value, Value, i32)> {
        let stdout = child.0.stdout.as_mut().context("worker stdout")?;
        // A poll event alone does not bound a blocking read after a partial line.
        let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
        ensure!(
            flags >= 0
                && unsafe {
                    libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                } == 0,
            "nonblocking worker output"
        );
        line(stdout, "ready")?;
        let before = invoke(&child.0.id().to_string())?;
        child
            .0
            .stdin
            .as_mut()
            .context("worker stdin")?
            .write_all(b"allocate\n")?;
        line(
            child.0.stdout.as_mut().context("worker stdout")?,
            "allocated",
        )?;
        let after = invoke(&child.0.id().to_string())?;
        let number = |v: &Value, k: &str| v[k].as_u64().with_context(|| format!("missing {k}"));
        ensure!(
            number(&before, "start_abstime")? == number(&after, "start_abstime")?
                && number(&after, "start_abstime")? > 0,
            "worker identity changed"
        );
        for k in ["rss_bytes", "footprint_bytes"] {
            ensure!(
                number(&after, k)?
                    .checked_sub(number(&before, k)?)
                    .is_some_and(|n| n >= 24 * 1024 * 1024),
                "insufficient {k} allocation growth"
            );
        }
        child
            .0
            .stdin
            .as_mut()
            .context("worker stdin")?
            .write_all(b"exit\n")?;
        let status = process::wait(&mut child.0, Duration::from_secs(5))?;
        ensure!(status.success(), "worker exit {status}");
        Ok((before, after, status.code().context("worker exit code")?))
    })();
    if child.0.try_wait()?.is_none() {
        child.0.kill()?;
        process::wait(&mut child.0, Duration::from_secs(5))?;
    }
    let (before, after, exit) = measured?;
    let mut uname = Command::new("/usr/bin/uname");
    uname.arg("-a");
    let platform = process::output(uname, Duration::from_secs(5), 65536)?;
    ensure!(platform.status.success(), "platform probe");
    let evidence = json!({"recorded_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"platform":String::from_utf8(platform.stdout)?.trim(),"source_sha256":format!("{:x}",Sha256::digest(fs::read(source)?)),"harness_kind":"rust-owned-allocation-worker","harness_sha256":format!("{:x}",Sha256::digest(include_bytes!("resource_sampler.rs"))),"calibration":calibration,"allocation_bytes":32*1024*1024,"before":before,"after":after,"worker_exit":exit,"invalid_pid_arguments_rejected":6});
    let mut bytes = serde_json::to_vec_pretty(&evidence)?;
    bytes.push(b'\n');
    fs::write(&args[1], bytes)?;
    Ok(())
}
