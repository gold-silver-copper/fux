//! Resource pressure on disposable real servers, including a genuinely SIGSTOP'd viewer.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use serde_json::{Value, json};

use super::brp::{self, Descriptor};
use super::pty::Terminal;
use super::stack::Stack;
use super::{
    Binaries, Fixture, Outcome, Result, WAIT, capture, err, exact_viewers, pane_row, until,
};

const PROBE: Duration = Duration::from_secs(2);
const REAP: Duration = Duration::from_secs(13);
const SHUTDOWN: Duration = Duration::from_secs(5);
const BODY_LIMIT: usize = 1024 * 1024;
const REPLY_LIMIT: usize = 64 * 1024;
const PARTIAL_PAIRS: usize = 8;

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|time| !time.is_zero())
        .ok_or_else(|| err("stress HTTP deadline exceeded"))
}

fn connect(descriptor: &Descriptor, deadline: Instant) -> Result<TcpStream> {
    // Product descriptors publish numeric loopback addresses: no unbounded DNS lookup.
    let address = SocketAddr::new(descriptor.host.parse()?, descriptor.port);
    Ok(TcpStream::connect_timeout(&address, remaining(deadline)?)?)
}

fn send(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        let written = stream.write(bytes)?;
        if written == 0 {
            return Err(err("stress HTTP write returned zero"));
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

fn head(length: usize) -> String {
    format!(
        "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
    )
}

fn receive(stream: &mut TcpStream, deadline: Instant) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(raw),
            Ok(count) => {
                if raw.len() + count > REPLY_LIMIT {
                    return Err(err("stress HTTP reply exceeds 64 KiB"));
                }
                raw.extend_from_slice(&buffer[..count]);
            }
            // A timed-out partial header can close with a reset instead of an orderly FIN.
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset && raw.is_empty() => {
                return Ok(raw);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn status(raw: &[u8]) -> Option<u16> {
    std::str::from_utf8(raw)
        .ok()?
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn request(descriptor: &Descriptor, body: &[u8], path: &Path) -> Result<(Vec<u8>, Duration)> {
    let started = Instant::now();
    let deadline = started + PROBE;
    let mut stream = connect(descriptor, deadline)?;
    send(&mut stream, head(body.len()).as_bytes(), deadline)?;
    send(&mut stream, body, deadline)?;
    let raw = receive(&mut stream, deadline)?;
    let elapsed = started.elapsed();
    std::fs::write(path, &raw)?;
    Ok((raw, elapsed))
}

fn info(descriptor: &Descriptor, prefix: &str, authenticated: bool) -> Value {
    let params = if authenticated {
        json!({"token": descriptor.token, "instance": descriptor.instance})
    } else {
        json!({})
    };
    json!({"jsonrpc": "2.0", "id": 1, "method": format!("{prefix}/server.info"), "params": params})
}

fn probe(descriptor: &Descriptor, prefix: &str, path: &Path) -> Result<u128> {
    let body = serde_json::to_vec(&info(descriptor, prefix, true))?;
    let (raw, elapsed) = request(descriptor, &body, path)?;
    let reply = brp::parse_response(&raw)?;
    if reply.get("result").is_none() || reply.get("error").is_some() || elapsed > PROBE {
        return Err(err(format!(
            "{prefix} authenticated probe failed after {elapsed:?}: {reply}"
        )));
    }
    Ok(elapsed.as_millis())
}

fn partials(descriptor: &Descriptor) -> Result<Vec<(bool, TcpStream)>> {
    let deadline = Instant::now() + PROBE;
    let mut streams = Vec::new();
    for _ in 0..PARTIAL_PAIRS {
        let mut header = connect(descriptor, deadline)?;
        send(
            &mut header,
            b"POST / HTTP/1.1\r\nHost: localhost\r\nX-Partial: ",
            deadline,
        )?;
        streams.push((false, header));
        let mut body = connect(descriptor, deadline)?;
        send(&mut body, head(4096).as_bytes(), deadline)?;
        send(&mut body, b"{", deadline)?;
        streams.push((true, body));
    }
    Ok(streams)
}

fn transport(
    descriptor: &Descriptor,
    prefix: &str,
    artifacts: &Path,
    evidence: &mut Value,
) -> Result<()> {
    let path = |name: &str| artifacts.join(format!("stress-{prefix}-{name}.http"));
    let body = vec![b' '; BODY_LIMIT + 1];
    let (raw, elapsed) = request(descriptor, &body, &path("oversized-body"))?;
    evidence["oversized_body"] =
        json!({"bytes": body.len(), "status": status(&raw), "elapsed_ms": elapsed.as_millis()});
    if status(&raw) != Some(413) {
        return Err(err(format!("{prefix}: oversized body was not HTTP 413")));
    }
    let batch = vec![info(descriptor, prefix, true); 65];
    let (raw, elapsed) = request(
        descriptor,
        &serde_json::to_vec(&batch)?,
        &path("oversized-batch"),
    )?;
    let reply = brp::parse_response(&raw)?;
    evidence["oversized_batch"] = json!({"elements": 65, "status": status(&raw), "reply": reply, "elapsed_ms": elapsed.as_millis()});
    if reply["error"]["code"].as_i64() != Some(-32600) || !reply["id"].is_null() {
        return Err(err(format!(
            "{prefix}: oversized batch was not rejected: {reply}"
        )));
    }

    let started = Instant::now();
    let mut streams = partials(descriptor)?;
    evidence["partial_headers"] = json!(PARTIAL_PAIRS);
    evidence["partial_bodies"] = json!(PARTIAL_PAIRS);
    // Exactly two bounded workers, at most 64 unauthorized exchanges total. Their lifetime
    // overlaps the authenticated probes; scoped joins and the stop flag cover failure too.
    let stop = AtomicBool::new(false);
    let ready = std::sync::Barrier::new(3);
    let load = std::thread::scope(|scope| -> Result<Value> {
        let mut workers = Vec::new();
        for worker in 0..2 {
            let stop = &stop;
            let ready = &ready;
            workers.push(scope.spawn(move || -> Result<Vec<Value>> {
                ready.wait();
                let body = serde_json::to_vec(&info(descriptor, prefix, false))?;
                let mut responses = Vec::new();
                for index in 0..32 {
                    if stop.load(Ordering::Relaxed) { break; }
                    let file = artifacts.join(format!("stress-{prefix}-unauthorized-{worker}-{index}.http"));
                    let begin_us = started.elapsed().as_micros();
                    let (raw, elapsed) = request(descriptor, &body, &file)?;
                    let reply = brp::parse_response(&raw)?;
                    if reply["error"]["code"].as_i64() != Some(-32002) {
                        return Err(err(format!("{prefix}: unauthenticated request not refused: {reply}")));
                    }
                    responses.push(json!({"status": status(&raw), "code": -32002, "elapsed_ms": elapsed.as_millis(), "begin_us": begin_us, "end_us": started.elapsed().as_micros()}));
                }
                Ok(responses)
            }));
        }
        ready.wait();
        let probes = (|| -> Result<Vec<Value>> {
            let mut times = Vec::new();
            for index in 0..4 {
                let begin_us = started.elapsed().as_micros();
                let elapsed = probe(descriptor, prefix, &path(&format!("loaded-probe-{index}")))?;
                times.push(json!({"elapsed_ms": elapsed, "begin_us": begin_us, "end_us": started.elapsed().as_micros()}));
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(times)
        })();
        stop.store(true, Ordering::Relaxed);
        let mut responses = Vec::new();
        for worker in workers {
            responses.extend(
                worker
                    .join()
                    .map_err(|_| err("unauthorized worker panicked"))??,
            );
        }
        if responses.is_empty() {
            return Err(err("unauthorized load did not execute"));
        }
        Ok(json!({"authenticated_probes": probes?, "unauthorized_responses": responses}))
    });
    evidence["load"] = load?;
    let deadline = started + REAP;
    let mut closures = Vec::new();
    for (index, (body, stream)) in streams.iter_mut().enumerate() {
        let raw = receive(stream, deadline)?;
        std::fs::write(path(&format!("partial-{index}")), &raw)?;
        let code = status(&raw);
        closures.push(json!({"body": body, "status": code, "observed_closed_ms": started.elapsed().as_millis()}));
        if *body && code != Some(408) {
            return Err(err(format!(
                "{prefix}: partial body did not receive HTTP 408: {code:?}"
            )));
        }
        if !*body && !raw.is_empty() && code != Some(408) {
            return Err(err(format!(
                "{prefix}: unexpected partial header response: {code:?}"
            )));
        }
    }
    evidence["partial_closures"] = json!(closures);
    evidence["recovered_probe_ms"] = json!(probe(descriptor, prefix, &path("recovered-probe"))?);
    Ok(())
}

fn native_pid(pid: u32) -> Result<Pid> {
    Ok(Pid::from_raw(i32::try_from(pid)?))
}

fn alive(pid: u32) -> Result<bool> {
    match kill(native_pid(pid)?, None) {
        Ok(()) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Panes are server-created process groups, not our Child handles. On failure, retire the
/// exact observed groups before Stack's server guards reap their direct children.
struct PaneGroups(Vec<u32>);
impl Drop for PaneGroups {
    fn drop(&mut self) {
        for pid in &self.0 {
            if let Ok(pid) = native_pid(*pid) {
                let _ = killpg(pid, Signal::SIGKILL);
            }
        }
    }
}

fn groups_gone(pids: &[u32]) -> Result<bool> {
    for pid in pids {
        match killpg(native_pid(*pid)?, None) {
            Err(Errno::ESRCH) => {}
            Ok(()) => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

pub(super) fn limits(fixture: &mut Fixture) -> Result<Outcome> {
    let fux_binary = fixture.local.fux_binary().to_path_buf();
    let bins = Binaries {
        dir: fux_binary
            .parent()
            .ok_or_else(|| err("binary directory"))?
            .to_path_buf(),
        fux: fux_binary,
        zor: fixture.local.zor_binary().to_path_buf(),
    };
    let mut stack = Stack::start("Resource", &bins)?;
    let mut groups = PaneGroups(Vec::new());
    let mut terminal = None;
    let mut evidence = json!({
        "probe_bound_ms": PROBE.as_millis(), "partial_reap_bound_ms": REAP.as_millis(),
        "shutdown_bound_ms": SHUTDOWN.as_millis(), "body_limit_bytes": BODY_LIMIT,
        "batch_limit": 64, "memory": "not measured; no RSS bound claimed",
        "fux": {}, "zor": {},
    });
    let exercised = (|| -> Result<()> {
        let fux = stack.fux()?;
        let zor = stack.zor()?;
        evidence["servers"] = json!({"fux_pid": fux.pid, "zor_pid": zor.pid, "fux_instance": fux.instance, "zor_instance": zor.instance});
        if !alive(fux.pid)? || !alive(zor.pid)? {
            return Err(err("resource server PID absent"));
        }
        // Include the bootstrap shell in shutdown ownership evidence, not just our hot pane.
        let initial = super::first_pane(&fux)?;
        let initial_pid = until(WAIT, "bootstrap live PID", || {
            Ok(pane_row(&fux, initial)?["pid"]
                .as_u64()
                .filter(|pid| *pid > 0))
        })?;
        groups.0.push(u32::try_from(initial_pid)?);
        let cwd = stack.path().join("home");
        stack.zor_run(&[
            "task",
            "create",
            "scn-resource-hot",
            "--title",
            "Resource pressure hot producer",
            "--cwd",
            cwd.to_str().ok_or_else(|| err("cwd"))?,
        ])?;
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let operation = format!("resource-{:x}-{epoch:x}", std::process::id());
        // One producer shell and at most one sleep child. A bounded burst followed by 20 ms
        // prevents this acceptance scenario from needlessly saturating the workstation.
        let script = "i=0; while :; do n=0; while [ \"$n\" -lt 512 ]; do printf 'STRESS_HOT_%012d xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' \"$i\"; i=$((i+1)); n=$((n+1)); done; sleep 0.02; done";
        stack.zor_run(&[
            "task",
            "launch",
            "scn-resource-hot",
            "--operation",
            &operation,
            "--",
            "/bin/sh",
            "-c",
            script,
        ])?;
        let (pane, pid) = until(WAIT, "resource hot attempt", || {
            let task = stack.zor_json(&["task", "inspect", "scn-resource-hot"])?;
            Ok(task["attempts"]
                .as_array()
                .into_iter()
                .flatten()
                .find_map(|attempt| {
                    if attempt["state"].as_str() != Some("live") {
                        return None;
                    }
                    Some((
                        attempt["pane"].as_u64()?,
                        u32::try_from(attempt["pid"].as_u64()?).ok()?,
                    ))
                }))
        })?;
        groups.0.push(pid);
        if pane_row(&fux, pane)?["pid"].as_u64() != Some(u64::from(pid)) || !alive(pid)? {
            return Err(err("hot task does not match a live fux child"));
        }
        evidence["hot_pane"] = json!({"pane": pane, "pid": pid, "owned_groups": groups.0});
        let mut command = stack.command(stack.fux_binary());
        command.args([
            "attach",
            "--brp",
            stack
                .fux_descriptor_path()
                .to_str()
                .ok_or_else(|| err("descriptor path"))?,
            "--pane",
            &pane.to_string(),
            "--pid",
            &pid.to_string(),
        ]);
        terminal = Some(Terminal::spawn(command, 24, 100)?);
        let viewer = terminal.as_mut().ok_or_else(|| err("viewer"))?;
        until(WAIT, "resource exact viewer paints hot output", || {
            if !viewer.running()? {
                return Err(err("resource viewer exited before pause"));
            }
            Ok((exact_viewers(&fux)? == 1
                && viewer
                    .screen(24, 100)
                    .iter()
                    .any(|line| line.contains("STRESS_HOT_")))
            .then_some(()))
        })?;
        killpg(native_pid(viewer.pid())?, Signal::SIGSTOP)?;
        let paused_at = Instant::now();
        let stopped = until(PROBE, "viewer actual stopped process state", || {
            let output = stack.invoke(
                Path::new("/bin/ps"),
                &["-o", "stat=", "-p", &viewer.pid().to_string()],
            )?;
            let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            Ok(state.contains('T').then_some(state))
        })?;
        evidence["paused_viewer"] =
            json!({"pid": viewer.pid(), "ps_stat": stopped, "signal": "SIGSTOP"});
        let before = capture(&fux, pane)?;
        std::fs::write(fixture.artifacts.join("stress-hot-before.txt"), &before)?;
        until(PROBE, "hot output progresses with stopped viewer", || {
            let after = capture(&fux, pane)?;
            if after != before && after.contains("STRESS_HOT_") {
                std::fs::write(fixture.artifacts.join("stress-hot-paused.txt"), after)?;
                Ok(Some(()))
            } else {
                Ok(None)
            }
        })?;
        transport(&fux, "fux", &fixture.artifacts, &mut evidence["fux"])?;
        transport(&zor, "zor", &fixture.artifacts, &mut evidence["zor"])?;
        // Hold fresh partial requests through shutdown on both actual acceptors.
        let _fux_pending = partials(&fux)?;
        let _zor_pending = partials(&zor)?;
        evidence["shutdown_pending_per_server"] = json!(PARTIAL_PAIRS * 2);
        if !alive(pid)? || !viewer.running()? {
            return Err(err("hot child or paused viewer died before shutdown"));
        }
        let later_output = capture(&fux, pane)?;
        until(
            PROBE,
            "hot output still advances after sustained stopped-viewer pressure",
            || {
                let output = capture(&fux, pane)?;
                if output != later_output && output.contains("STRESS_HOT_") {
                    std::fs::write(
                        fixture.artifacts.join("stress-hot-before-shutdown.txt"),
                        output,
                    )?;
                    Ok(Some(()))
                } else {
                    Ok(None)
                }
            },
        )?;
        let output = stack.invoke(
            Path::new("/bin/ps"),
            &["-o", "stat=", "-p", &viewer.pid().to_string()],
        )?;
        let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !state.contains('T') {
            return Err(err(format!("viewer no longer stopped: {state}")));
        }
        evidence["paused_viewer"]["before_shutdown_stat"] = json!(state);
        evidence["paused_viewer"]["paused_ms"] = json!(paused_at.elapsed().as_millis());
        for (prefix, expected_pid) in [("zor", zor.pid), ("fux", fux.pid)] {
            let (pid, status, elapsed) = stack.terminate(prefix, SHUTDOWN)?;
            evidence["shutdown"][prefix] = json!({"pid": pid, "status": status.to_string(), "elapsed_ms": elapsed.as_millis()});
            if pid != expected_pid || !status.success() || elapsed > SHUTDOWN || alive(pid)? {
                return Err(err(format!(
                    "{prefix} SIGTERM shutdown failed: pid {pid}, {status}, {elapsed:?}"
                )));
            }
        }
        until(SHUTDOWN, "all owned pane process groups gone", || {
            Ok(groups_gone(&groups.0)?.then_some(()))
        })?;
        evidence["owned_groups_gone"] = json!(groups.0);
        groups.0.clear();
        // A stopped viewer cannot consume disconnect until resumed; its natural exit proves
        // the server closed its connection, while Terminal's guard remains the fallback.
        killpg(native_pid(viewer.pid())?, Signal::SIGCONT)?;
        until(SHUTDOWN, "resumed viewer observes server shutdown", || {
            Ok((!viewer.running()?).then_some(()))
        })?;
        evidence["viewer_exited_after_resume"] = json!(true);
        Ok(())
    })();
    if let Err(error) = &exercised {
        evidence["failure"] = json!(error.to_string());
    }
    let saved = std::fs::write(
        fixture.artifacts.join("stress-limits.json"),
        serde_json::to_vec_pretty(&evidence)?,
    );
    let logs = (|| -> Result<()> {
        for prefix in ["fux", "zor"] {
            std::fs::write(
                fixture.artifacts.join(format!("stress-{prefix}-serve.log")),
                stack.log(&format!("{prefix}-serve.log")),
            )?;
        }
        if let Some(viewer) = &terminal {
            std::fs::write(
                fixture.artifacts.join("stress-viewer.ansi"),
                viewer.output(),
            )?;
        }
        Ok(())
    })();
    drop(terminal);
    drop(groups);
    drop(stack);
    exercised?;
    saved?;
    logs?;
    Ok(Outcome::Pass("real fux + zor: 1 MiB+1 bodies HTTP 413, 65-call batches -32600; bounded unauthorized load plus 16 partial requests each kept authenticated probes under 2 s, stalled bodies HTTP 408 within 13 s; hot PTY advanced with a SIGSTOP-confirmed viewer; both SIGTERM exits under 5 s, owned process groups gone; raw HTTP/PID/latency evidence in stress-* (RSS not measured)".into()))
}
