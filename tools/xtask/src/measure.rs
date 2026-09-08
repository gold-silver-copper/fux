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
fn cpu(pid: u32) -> Result<f64> {
    let mut total = 0.;
    for part in ps(pid, "cputime")?.replace('-', ":").split(':') {
        total = total * 60. + part.parse::<f64>()?;
    }
    Ok(total)
}
fn rss(pid: u32) -> Result<u64> {
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
fn drain(peer: &mut UnixStream, timeout: Duration) -> Result<()> {
    peer.set_read_timeout(Some(timeout))?;
    loop {
        match attachment::receive(peer) {
            Ok(_) => {}
            Err(e) => {
                if e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )
                }) {
                    return Ok(());
                }
                return Err(e);
            }
        }
    }
}
fn visible(peer: &mut UnixStream, marker: &str) -> Result<()> {
    loop {
        if attachment::text(&attachment::receive(peer)?)?.contains(marker) {
            return Ok(());
        }
    }
}
fn round(value: f64, digits: usize) -> f64 {
    format!("{value:.digits$}")
        .parse()
        .expect("formatted number")
}
pub fn run(args: Vec<String>) -> Result<()> {
    let binary = Path::new(
        args.first()
            .context("usage: measure BINARY [--version N] [--samples N]")?,
    )
    .canonicalize()?;
    let mut version = 3u32;
    let mut samples = 20usize;
    ensure!((args.len() - 1).is_multiple_of(2), "missing option value");
    for p in args[1..].chunks_exact(2) {
        match p[0].as_str() {
            "--version" => version = p[1].parse()?,
            "--samples" => samples = p[1].parse()?,
            _ => anyhow::bail!("unknown option {}", p[0]),
        }
    }
    ensure!(samples > 0, "samples must be positive");
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
                        attachment::send(
                            &mut p,
                            &json!({"type":"hello","version":version,"rows":24,"columns":80}),
                        )?;
                        let hello = attachment::receive(&mut p)?;
                        ensure!(
                            hello == json!({"hello":{"version":version}}),
                            "unexpected hello: {hello}"
                        );
                        break p;
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
        peer.set_read_timeout(Some(Duration::from_secs(10)))?;
        for index in 0..samples {
            let marker = format!("MARK{index:03}");
            let begin = Instant::now();
            let input = format!("printf {marker}Z\\\\n\n");
            attachment::send(&mut peer, &json!({"type":"input","bytes":input.as_bytes()}))?;
            visible(&mut peer, &format!("{marker}Z"))?;
            latencies.push(begin.elapsed().as_secs_f64());
            drain(&mut peer, Duration::from_millis(200))?;
            peer.set_read_timeout(Some(Duration::from_secs(10)))?;
        }
        let burst=b"i=0; while [ $i -lt 20000 ]; do echo line$i; i=$((i+1)); done; printf BURSTDONE\\\\n\n";
        attachment::send(&mut peer, &json!({"type":"input","bytes":burst.as_slice()}))?;
        let begin = Instant::now();
        visible(&mut peer, "BURSTDONE")?;
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
