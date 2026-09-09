//! Two-process koh gateway measurement over the deterministic local (loopback) network profile:
//! `koh gateway serve` in front of a harness echo service and `koh gateway connect` exposing it.
//! Both koh processes are sampled separately; the echo service and the client run in this
//! harness, whose CPU is not attributed to koh. Bytes are validated end to end, so a lossy or
//! truncated transfer cannot appear fast. This is local loopback evidence only, not relay,
//! NAT or live network performance.
use crate::measure::{cpu, rss};
use anyhow::{Context, Result, bail, ensure};
use fux_xtask::support::{
    local::until,
    process::{self, Guard},
};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const CHUNK: usize = 16 * 1024;
const PINGS: usize = 200;
const PASSPHRASE: &str = "fux-measure-koh";

fn pattern(offset: usize, buffer: &mut [u8]) {
    for (index, byte) in buffer.iter_mut().enumerate() {
        let position = offset.wrapping_add(index);
        *byte = (position.wrapping_mul(31) ^ (position >> 8)) as u8;
    }
}

fn command(binary: &Path, root: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_RUNTIME_DIR", root)
        .env("KOH_KEY_PASSPHRASE", PASSPHRASE)
        .env("KOH_KEY_NEW_PASSPHRASE", PASSPHRASE)
        .current_dir(root);
    command
}

/// Echo service: every accepted connection is copied back verbatim until the peer shuts down.
fn echo_service(socket: &Path) -> Result<Arc<AtomicUsize>> {
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    thread::Builder::new()
        .name("koh-echo".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                count.fetch_add(1, Ordering::AcqRel);
                let _ = thread::Builder::new().name("koh-echo-conn".into()).spawn(
                    move || -> Result<()> {
                        let mut writer = stream.try_clone()?;
                        let mut buffer = vec![0; CHUNK];
                        loop {
                            let length = stream.read(&mut buffer)?;
                            if length == 0 {
                                let _ = writer.shutdown(std::net::Shutdown::Write);
                                return Ok(());
                            }
                            writer.write_all(buffer.get(..length).context("read length")?)?;
                        }
                    },
                );
            }
        })?;
    Ok(accepted)
}

fn wait_socket(path: &Path, timeout: Duration, child: &mut Guard) -> Result<()> {
    until(timeout, || {
        ensure!(
            child.0.try_wait()?.is_none(),
            "koh exited before {path:?} appeared"
        );
        Ok(UnixStream::connect(path).ok().map(drop))
    })
}

struct Sample {
    cpu: f64,
    rss: u64,
}
fn sample(pid: u32) -> Result<Sample> {
    Ok(Sample {
        cpu: cpu(pid)?,
        rss: rss(pid)?,
    })
}

pub fn run(args: Vec<String>) -> Result<()> {
    let mut args = args.into_iter();
    let binary = PathBuf::from(
        args.next()
            .context("usage: measure-koh KOH_BINARY [--bytes N] [--idle-seconds S]")?,
    );
    let mut bytes: usize = 64 * 1024 * 1024;
    let mut idle_seconds: u64 = 5;
    while let Some(arg) = args.next() {
        let value = args.next().context("missing option value")?;
        match arg.as_str() {
            "--bytes" => bytes = value.parse()?,
            "--idle-seconds" => idle_seconds = value.parse()?,
            other => bail!("unknown option {other}"),
        }
    }
    ensure!(
        (CHUNK..=4 << 30).contains(&bytes),
        "--bytes must be between {CHUNK} and 4 GiB"
    );
    ensure!(
        (1..=120).contains(&idle_seconds),
        "--idle-seconds must be 1..=120"
    );
    let directory = tempfile::Builder::new()
        .prefix("fux-koh-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")?;
    let root = directory.path();
    let app_socket = root.join("app.sock");
    let viewer_socket = root.join("viewer.sock");
    let accepted = echo_service(&app_socket)?;

    // Client identity first: the gateway authorizes exactly this endpoint.
    let mut id = command(&binary, root);
    id.args(["id", "--key-file"]).arg(root.join("client.key"));
    let id = process::output(id, Duration::from_secs(30), 65536)?;
    ensure!(
        id.status.success(),
        "koh id: {}",
        String::from_utf8_lossy(&id.stderr)
    );
    let client_id = String::from_utf8(id.stdout)?.trim().to_owned();
    ensure!(!client_id.is_empty(), "koh id printed nothing");

    let started = Instant::now();
    let mut serve = command(&binary, root);
    serve
        .args(["gateway", "serve", "--local", "--socket"])
        .arg(&app_socket)
        .arg("--key-file")
        .arg(root.join("gateway.key"))
        .arg("--allow")
        .arg(&client_id)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(fs::File::create(root.join("serve.stderr"))?);
    let mut serve = Guard(serve.spawn()?);
    let mut ready = String::new();
    BufReader::new(serve.0.stdout.take().context("serve stdout")?).read_line(&mut ready)?;
    let ready: serde_json::Value = serde_json::from_str(ready.trim())
        .with_context(|| format!("gateway readiness line: {ready:?}"))?;
    let serve_startup = started.elapsed();
    let endpoint = ready["endpoint_id"]
        .as_str()
        .context("endpoint_id")?
        .to_owned();
    let direct = ready["direct_addr"]
        .as_str()
        .context("direct_addr")?
        .to_owned();

    let started = Instant::now();
    let mut connect = command(&binary, root);
    connect
        .args(["gateway", "connect", &endpoint, "--socket"])
        .arg(&viewer_socket)
        .arg("--key-file")
        .arg(root.join("client.key"))
        .arg("--direct")
        .arg(&direct)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(root.join("connect.stderr"))?);
    let mut connect = Guard(connect.spawn()?);
    wait_socket(&viewer_socket, Duration::from_secs(20), &mut connect)?;
    let connect_startup = started.elapsed();
    let (serve_pid, connect_pid) = (serve.0.id(), connect.0.id());

    // Idle: both processes exist, no application connection.
    thread::sleep(Duration::from_secs(1));
    let (idle_serve, idle_connect) = (sample(serve_pid)?, sample(connect_pid)?);
    thread::sleep(Duration::from_secs(idle_seconds));
    let idle_serve_cpu = cpu(serve_pid)? - idle_serve.cpu;
    let idle_connect_cpu = cpu(connect_pid)? - idle_connect.cpu;

    // Bulk transfer: the first byte also measures the lazy session/application handshake.
    let (before_serve, before_connect) = (sample(serve_pid)?, sample(connect_pid)?);
    let mut viewer = UnixStream::connect(&viewer_socket)?;
    viewer.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut writer = viewer.try_clone()?;
    let total = bytes;
    let sender =
        thread::Builder::new()
            .name("koh-sender".into())
            .spawn(move || -> Result<()> {
                let mut buffer = vec![0; CHUNK];
                let mut offset = 0;
                while offset < total {
                    let length = CHUNK.min(total - offset);
                    let chunk = buffer.get_mut(..length).context("chunk")?;
                    pattern(offset, chunk);
                    writer.write_all(chunk)?;
                    offset += length;
                }
                Ok(())
            })?;
    let started = Instant::now();
    let mut first_byte = None;
    let receive = (|| -> Result<()> {
        let mut expected = vec![0; CHUNK];
        let mut buffer = vec![0; CHUNK];
        let mut received = 0;
        while received < total {
            let length = viewer.read(&mut buffer)?;
            ensure!(
                length > 0,
                "echo stream ended after {received} of {total} bytes"
            );
            if first_byte.is_none() {
                first_byte = Some(started.elapsed());
            }
            let chunk = buffer.get(..length).context("read length")?;
            let check = expected.get_mut(..length).context("expected length")?;
            pattern(received, check);
            ensure!(chunk == check, "echoed bytes differ at offset {received}");
            received += length;
        }
        Ok(())
    })();
    let bulk = started.elapsed();
    if let Err(error) = receive {
        // Release the sender so its own failure, if any, is reported alongside the reader's.
        let _ = viewer.shutdown(std::net::Shutdown::Both);
        let sent = sender
            .join()
            .map_err(|_| anyhow::anyhow!("sender panicked"))
            .and_then(|result| result);
        bail!("receive: {error:#}; sender: {sent:?}");
    }
    sender
        .join()
        .map_err(|_| anyhow::anyhow!("sender panicked"))??;
    let first_byte = first_byte.context("no bytes received")?;

    // Small-message round trips on the established session.
    let mut latencies = Vec::with_capacity(PINGS);
    let mut ping = [0u8; 64];
    let mut pong = [0u8; 64];
    for index in 0..PINGS {
        pattern(index, &mut ping);
        let sent = Instant::now();
        viewer.write_all(&ping)?;
        viewer.read_exact(&mut pong)?;
        ensure!(ping == pong, "ping {index} echoed differently");
        latencies.push(sent.elapsed().as_secs_f64() * 1000.);
    }
    latencies.sort_by(|a, b| a.total_cmp(b));
    let percentile = |fraction: f64| -> f64 {
        let rank = ((latencies.len() as f64 * fraction).ceil() as usize).max(1) - 1;
        latencies.get(rank).copied().unwrap_or(f64::NAN)
    };
    let (after_serve, after_connect) = (sample(serve_pid)?, sample(connect_pid)?);
    viewer.shutdown(std::net::Shutdown::Write)?;
    let mut rest = Vec::new();
    viewer.read_to_end(&mut rest)?;
    ensure!(
        rest.is_empty(),
        "unexpected trailing echo bytes: {}",
        rest.len()
    );
    drop(viewer);

    // Cleanup: both koh processes must stop on SIGTERM within bounds.
    let started = Instant::now();
    let failures = process::stop_owned(&mut [Some(&mut connect.0), Some(&mut serve.0)]);
    ensure!(failures.is_empty(), "owned koh cleanup: {failures:?}");
    let shutdown = started.elapsed();
    ensure!(
        accepted.load(Ordering::Acquire) == 1,
        "echo service accepted {} connections, expected 1",
        accepted.load(Ordering::Acquire)
    );

    let mib = bytes as f64 / (1024. * 1024.);
    let transfer_serve_cpu = after_serve.cpu - before_serve.cpu;
    let transfer_connect_cpu = after_connect.cpu - before_connect.cpu;
    let report = json!({
        "binary": binary,
        "profile": "local",
        "bytes": bytes,
        "serve_startup_ms": (serve_startup.as_secs_f64() * 1000.).round(),
        "connect_startup_ms": (connect_startup.as_secs_f64() * 1000.).round(),
        "idle_seconds": idle_seconds,
        "idle_serve_cpu_ms": (idle_serve_cpu * 1000.).round(),
        "idle_connect_cpu_ms": (idle_connect_cpu * 1000.).round(),
        "idle_serve_rss_kib": idle_serve.rss,
        "idle_connect_rss_kib": idle_connect.rss,
        "first_byte_ms": (first_byte.as_secs_f64() * 1000. * 100.).round() / 100.,
        "bulk_s": (bulk.as_secs_f64() * 1000.).round() / 1000.,
        "bulk_mib_per_s": (mib / bulk.as_secs_f64() * 10.).round() / 10.,
        "serve_cpu_ms_per_mib": (transfer_serve_cpu * 1000. / mib * 100.).round() / 100.,
        "connect_cpu_ms_per_mib": (transfer_connect_cpu * 1000. / mib * 100.).round() / 100.,
        "transfer_serve_cpu_s": (transfer_serve_cpu * 1000.).round() / 1000.,
        "transfer_connect_cpu_s": (transfer_connect_cpu * 1000.).round() / 1000.,
        "serve_rss_after_kib": after_serve.rss,
        "connect_rss_after_kib": after_connect.rss,
        "ping_count": PINGS,
        "ping_median_ms": (percentile(0.5) * 1000.).round() / 1000.,
        "ping_p95_ms": (percentile(0.95) * 1000.).round() / 1000.,
        "shutdown_ms": (shutdown.as_secs_f64() * 1000.).round(),
        "echo_accepts": 1,
        "validated": true,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
