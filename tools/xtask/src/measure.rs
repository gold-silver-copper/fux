//! Legacy local attachment measurements; preserve the original workload/report contract.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    attachment,
    local::Root,
    process::{self, Guard, OwnedProcess},
};
use serde_json::json;
use std::{
    fs,
    io::Read,
    os::unix::net::UnixStream,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn ps(pid: u32, key: &str) -> Result<String> {
    let mut c = Command::new("ps");
    c.args(["-o", &format!("{key}="), "-p", &pid.to_string()]);
    let r = process::output(c, Duration::from_secs(5), 65536)?;
    ensure!(
        r.status.success(),
        "ps {key}: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    Ok(String::from_utf8(r.stdout)?.trim().into())
}
/// Total user+system CPU seconds of `pid`. macOS reads `proc_pid_rusage` (microsecond
/// precision); elsewhere `ps cputime` (10 ms precision) is retained as in main.
pub(crate) fn cpu(pid: u32) -> Result<f64> {
    #[cfg(target_os = "macos")]
    {
        cpu_rusage(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut total = 0.;
        for part in ps(pid, "cputime")?.replace('-', ":").split(':') {
            total = total * 60. + part.parse::<f64>()?;
        }
        Ok(total)
    }
}
#[cfg(target_os = "macos")]
#[allow(deprecated)] // libc marks mach_timebase_info deprecated in favour of mach2; no new dependency.
fn cpu_rusage(pid: u32) -> Result<f64> {
    let pid = i32::try_from(pid).context("pid range")?;
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: `info` is a valid, writable rusage_info_v2 and the flavor matches its layout.
    let status = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    ensure!(status == 0, "proc_pid_rusage({pid}) failed: {status}");
    // SAFETY: the call succeeded, so the kernel filled the structure.
    let info = unsafe { info.assume_init() };
    let mut timebase = libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: a valid out-pointer for the timebase structure.
    ensure!(
        unsafe { libc::mach_timebase_info(&mut timebase) } == 0 && timebase.denom != 0,
        "mach_timebase_info"
    );
    let ticks = info.ri_user_time.saturating_add(info.ri_system_time) as f64;
    Ok(ticks * f64::from(timebase.numer) / f64::from(timebase.denom) / 1e9)
}
pub(crate) fn rss(pid: u32) -> Result<u64> {
    let s = ps(pid, "rss")?;
    Ok(if s.is_empty() { 0 } else { s.parse()? })
}
fn switches(pid: u32) -> Result<Option<u64>> {
    let p = format!("/proc/{pid}/status");
    let text = match fs::read_to_string(p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let re = regex::Regex::new(r"voluntary_ctxt_switches:\s+(\d+)")?;
    re.captures(&text).map(|c| Ok(c[1].parse()?)).transpose()
}
fn retryable_connect(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
        )
    }) || error.downcast_ref::<nix::errno::Errno>().is_some_and(|e| {
        matches!(
            e,
            nix::errno::Errno::ECONNREFUSED | nix::errno::Errno::ENOENT
        )
    })
}
/// Keeps partial frame bytes across quiet-window timeouts. A timed-out read must
/// never discard a length prefix and interpret the remaining body as a new frame.
struct Reader {
    peer: UnixStream,
    pending: Vec<u8>,
}
impl Reader {
    fn receive(&mut self, deadline: Instant) -> Result<serde_json::Value> {
        loop {
            let target = if self.pending.len() < 4 {
                4
            } else {
                let length = u32::from_be_bytes(self.pending[..4].try_into()?) as usize;
                ensure!(
                    length <= 16 * 1024 * 1024,
                    "measurement frame exceeds limit"
                );
                length + 4
            };
            if self.pending.len() >= 4 && self.pending.len() == target {
                let value = serde_json::from_slice(&self.pending[4..])?;
                self.pending.clear();
                return Ok(value);
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|time| !time.is_zero())
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "measurement frame deadline")
                })?;
            self.peer.set_read_timeout(Some(remaining))?;
            let mut bytes = [0; 65536];
            let wanted = (target - self.pending.len()).min(bytes.len());
            match self.peer.read(&mut bytes[..wanted]) {
                Ok(0) => anyhow::bail!("measurement peer closed mid-frame"),
                Ok(count) => self.pending.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
}
fn timeout(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    })
}
fn drain(reader: &mut Reader, quiet: Duration) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        ensure!(Instant::now() < end, "measurement never became quiet");
        match reader.receive(end.min(Instant::now() + quiet)) {
            Ok(_) => {}
            Err(error) if timeout(&error) && Instant::now() < end => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}
fn visible(reader: &mut Reader, marker: &str, duration: Duration) -> Result<()> {
    let deadline = Instant::now() + duration;
    loop {
        if attachment::text(&reader.receive(deadline)?)?.contains(marker) {
            return Ok(());
        }
    }
}
pub(crate) fn round(value: f64, digits: usize) -> f64 {
    format!("{value:.digits$}")
        .parse()
        .expect("formatted number")
}
pub fn run(args: Vec<String>) -> Result<()> {
    let binary = Path::new(
        args.first()
            .context("usage: measure BINARY [--samples N] [--version HISTORICAL_VERSION]")?,
    )
    .canonicalize()?;
    let mut version: Option<u32> = None;
    let mut samples = 20usize;
    ensure!((args.len() - 1).is_multiple_of(2), "missing option value");
    for p in args[1..].chunks_exact(2) {
        match p[0].as_str() {
            "--version" => version = Some(p[1].parse()?),
            "--samples" => samples = p[1].parse()?,
            _ => anyhow::bail!("unknown option {}", p[0]),
        }
    }
    ensure!((1..=10000).contains(&samples), "samples must be 1-10000");
    let root = Root::new("fux-measure-rs-", &["/bin/sh".into()])?;
    let log = tempfile::tempfile()?;
    let mut c = root.command(&binary);
    c.args(["serve", "--name", "default"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log.try_clone()?);
    let started = Instant::now();
    let mut server = Guard(c.spawn()?);
    let measured = (|| -> Result<serde_json::Value> {
        let sock = root.path().join("fux/default.attach.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut peer = loop {
            ensure!(
                server.0.try_wait()?.is_none(),
                "server exited before attachment"
            );
            ensure!(Instant::now() < deadline, "server did not start");
            if sock.exists() {
                match attachment::connect(&sock) {
                    Ok(mut p) => {
                        let mut hello = json!({"type":"hello","rows":24,"columns":80});
                        if let Some(version) = version {
                            hello["version"] = version.into();
                        }
                        attachment::send(&mut p, &hello)?;
                        let mut reader = Reader {
                            peer: p,
                            pending: Vec::new(),
                        };
                        let hello = reader.receive(deadline)?;
                        let expected = version.map_or_else(
                            || json!({"hello":{}}),
                            |version| json!({"hello":{"version":version}}),
                        );
                        ensure!(hello == expected, "unexpected hello: {hello}");
                        break reader;
                    }
                    Err(e) => {
                        ensure!(retryable_connect(&e), "attachment connect: {e:#}");
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let startup = started.elapsed().as_secs_f64();
        drain(&mut peer, Duration::from_secs(2))?;
        let rss_start = rss(server.0.id())?;
        let cpu_before = cpu(server.0.id())?;
        let wake_before = switches(server.0.id())?;
        std::thread::sleep(Duration::from_secs(10));
        let cpu_after = cpu(server.0.id())?;
        let wake_after = switches(server.0.id())?;
        let wakeups = match wake_before {
            None => json!("n/a"),
            Some(b) => json!(
                wake_after
                    .context("missing wakeup counter")?
                    .checked_sub(b)
                    .context("decreasing wakeup counter")?
            ),
        };
        let mut latencies = Vec::new();
        peer.peer.set_read_timeout(Some(Duration::from_secs(10)))?;
        for index in 0..samples {
            let marker = format!("MARK{index:03}");
            let begin = Instant::now();
            let input = format!("printf {marker}Z\\\\n\n");
            attachment::send(
                &mut peer.peer,
                &json!({"type":"input","bytes":input.as_bytes()}),
            )?;
            visible(&mut peer, &format!("{marker}Z"), Duration::from_secs(10))?;
            latencies.push(begin.elapsed().as_secs_f64());
            drain(&mut peer, Duration::from_millis(200))?;
            peer.peer.set_read_timeout(Some(Duration::from_secs(10)))?;
        }
        let burst=b"i=0; while [ $i -lt 20000 ]; do echo line$i; i=$((i+1)); done; printf BURST''DONE\\\\n\n";
        attachment::send(
            &mut peer.peer,
            &json!({"type":"input","bytes":burst.as_slice()}),
        )?;
        let begin = Instant::now();
        visible(&mut peer, "BURSTDONE", Duration::from_secs(60))?;
        let burst = begin.elapsed().as_secs_f64();
        let rss_after = rss(server.0.id())?;
        latencies.sort_by(f64::total_cmp);
        let median = if samples.is_multiple_of(2) {
            (latencies[samples / 2 - 1] + latencies[samples / 2]) / 2.
        } else {
            latencies[samples / 2]
        };
        let rank = (samples as f64 * 0.95) as usize;
        let p95 = latencies[if rank == 0 { samples - 1 } else { rank - 1 }];
        Ok(
            json!({"binary":binary,"startup_s":round(startup,4),"idle_cpu_s_per_10s":round(cpu_after-cpu_before,3),"idle_wakeups_per_10s":wakeups,"rss_start_kib":rss_start,"rss_after_burst_kib":rss_after,"burst_20000_lines_s":round(burst,3),"latency_median_ms":round(median*1000.,2),"latency_p95_ms":round(p95*1000.,2)}),
        )
    })();
    let cleanup = (|| -> Result<()> {
        server.0.terminate()?;
        process::wait(&mut server.0, Duration::from_secs(10))?;
        Ok(())
    })();
    if cleanup.is_err() && server.0.try_wait()?.is_none() {
        server.0.kill()?;
        process::wait(&mut server.0, Duration::from_secs(3))?;
    }
    cleanup?;
    println!("{}", serde_json::to_string_pretty(&measured?)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn quiet_timeouts_keep_partial_frames_and_reject_oversized_prefixes() -> Result<()> {
        let (peer, mut writer) = UnixStream::pair()?;
        let mut reader = Reader {
            peer,
            pending: Vec::new(),
        };
        let value = json!({"hello":{}});
        let bytes = serde_json::to_vec(&value)?;
        let mut frame = u32::try_from(bytes.len())?.to_be_bytes().to_vec();
        frame.extend(bytes);
        writer.write_all(&frame[..2])?;
        let error = reader
            .receive(Instant::now() + Duration::from_millis(20))
            .unwrap_err();
        assert!(timeout(&error));
        assert_eq!(reader.pending.len(), 2);
        writer.write_all(&frame[2..6])?;
        assert!(timeout(
            &reader
                .receive(Instant::now() + Duration::from_millis(20))
                .unwrap_err()
        ));
        assert_eq!(reader.pending.len(), 6);
        writer.write_all(&frame[6..])?;
        writer.write_all(&frame)?;
        for _ in 0..2 {
            assert_eq!(
                reader.receive(Instant::now() + Duration::from_secs(1))?,
                value
            );
        }
        writer.write_all(&(16 * 1024 * 1024 + 1u32).to_be_bytes())?;
        assert!(
            reader
                .receive(Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .to_string()
                .contains("exceeds limit")
        );
        assert_eq!(reader.pending.len(), 4);
        Ok(())
    }

    #[test]
    fn decimal_reporting_preserves_binary_float_rounding() {
        assert_eq!(super::round(2.675, 2), 2.67);
        assert_eq!(super::round(2.685, 2), 2.69);
        assert_eq!(super::round(1.25, 1), 1.2);
        assert_eq!(super::round(-1.25, 1), -1.2);
    }
    #[test]
    fn startup_retries_only_absent_or_refused_sockets() {
        for errno in [
            nix::errno::Errno::ENOENT,
            nix::errno::Errno::ECONNREFUSED,
            nix::errno::Errno::EACCES,
        ] {
            let expected = errno != nix::errno::Errno::EACCES;
            assert_eq!(
                super::retryable_connect(&anyhow::Error::new(errno)),
                expected
            );
            assert_eq!(
                super::retryable_connect(&anyhow::Error::new(std::io::Error::from_raw_os_error(
                    errno as i32
                ))),
                expected
            );
        }
    }
}
