//! Real managed OpenCode hooks with local synthetic responses and test-only reload.
use crate::{
    capture_native::{events, normalized},
    integration,
    native_provider::Provider,
    runtime::{self, Owner, Root},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::{fs::DirBuilderExt, net::UnixStream},
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
fn field<'a>(v: &'a Value, p: &str) -> Result<&'a Value> {
    v.pointer(p).with_context(|| format!("missing {p}"))
}
fn task(root: &Root, zor: &Path, args: &[&str]) -> Result<Value> {
    let end = Instant::now() + Duration::from_secs(12);
    loop {
        let mut c = root.command(zor);
        c.arg("task").args(args);
        let r = runtime::output(
            c,
            end.checked_duration_since(Instant::now())
                .context("task deadline")?,
        )?;
        if r.stderr == b"zor: zor journal is busy; retry the operation ID\n" && Instant::now() < end
        {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        ensure!(
            r.status.success(),
            "task {args:?}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        return Ok(serde_json::from_slice(&r.stdout)?);
    }
}
fn until<T>(
    root: &Path,
    owner: &mut Owner,
    mut check: impl FnMut() -> Result<Option<T>>,
) -> Result<T> {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        ensure!(!root.join("reload.error").exists(), "reload failed");
        if let Some(v) = check()? {
            return Ok(v);
        }
        ensure!(owner.0.try_wait()?.is_none(), "fux exited");
        ensure!(Instant::now() < end, "integration observation deadline");
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn adapter(path: &Path, request: &Value) -> Result<Value> {
    use nix::{
        fcntl::{FcntlArg, FdFlag, OFlag, fcntl},
        sys::socket::{self, AddressFamily, SockFlag, SockType, UnixAddr, sockopt},
    };
    use std::os::fd::{AsFd, AsRawFd};
    let deadline = Instant::now() + Duration::from_secs(3);
    let fd = socket::socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )?;
    fcntl(&fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    fcntl(&fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
    match socket::connect(fd.as_raw_fd(), &UnixAddr::new(path)?) {
        Ok(()) => {}
        Err(nix::errno::Errno::EINPROGRESS) => loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .context("adapter connect deadline")?;
            let mut polls = [nix::poll::PollFd::new(
                fd.as_fd(),
                nix::poll::PollFlags::POLLOUT,
            )];
            match nix::poll::poll(&mut polls, u16::try_from(left.as_millis().clamp(1, 100))?) {
                Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
                Ok(_) => {}
                Err(e) => return Err(e.into()),
            }
            ensure!(
                socket::getsockopt(&fd, sockopt::SocketError)? == 0,
                "adapter connect failed"
            );
            break;
        },
        Err(e) => return Err(e.into()),
    }
    let mut peer = UnixStream::from(fd);
    peer.set_nonblocking(false)?;
    peer.set_read_timeout(Some(Duration::from_secs(3)))?;
    peer.set_write_timeout(Some(Duration::from_secs(3)))?;
    writeln!(peer, "{request}")?;
    let mut bytes = Vec::new();
    loop {
        let mut b = [0];
        peer.read_exact(&mut b)?;
        bytes.push(b[0]);
        ensure!(bytes.len() <= 4096, "adapter reply bound");
        if b[0] == b'\n' {
            return Ok(serde_json::from_slice(&bytes)?);
        }
    }
}
pub fn run(args: Vec<String>) -> Result<()> {
    let mut flags = BTreeMap::new();
    let mut reload = false;
    let mut args = args.iter();
    while let Some(k) = args.next() {
        if k == "--reload-adapter" {
            ensure!(!reload, "duplicate reload flag");
            reload = true;
            continue;
        }
        ensure!(
            ["--fux", "--zor", "--opencode", "--output", "--blocker"].contains(&k.as_str()),
            "unknown flag"
        );
        ensure!(
            flags
                .insert(k.as_str(), args.next().context("flag value")?.as_str())
                .is_none(),
            "duplicate flag"
        );
    }
    let blocker = *flags.get("--blocker").unwrap_or(&"permission");
    ensure!(
        ["permission", "question"].contains(&blocker) && (!reload || blocker == "question"),
        "blocker/reload selection"
    );
    let fux = Path::new(flags.get("--fux").context("fux")?).canonicalize()?;
    let zor = Path::new(flags.get("--zor").context("zor")?).canonicalize()?;
    let agent = Path::new(flags.get("--opencode").context("opencode")?).canonicalize()?;
    let output = Path::new(flags.get("--output").context("output")?);
    ensure!(!output.exists(), "use new output");
    if let Some(p) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(p)?;
    }
    fs::DirBuilder::new().mode(0o700).create(output)?;
    let mut root = Root::new("zoi-rs-")?;
    let path = root.path().canonicalize()?;
    for (key, suffix) in [
        ("HOME", ""),
        ("XDG_RUNTIME_DIR", ""),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ] {
        let value = if suffix.is_empty() {
            path.clone()
        } else {
            path.join(suffix)
        };
        root.set_env(
            key,
            value.to_str().context("private environment path")?.into(),
        );
    }
    let fixture = path.join("work/fixture.txt");
    fs::write(&fixture, "Owned fixture.\n")?;
    let probe = path.join("probe.mjs");
    fs::write(&probe,include_str!("native-probe.mjs").replace("\"permission.replied\"","\"permission.replied\", \"question.asked\", \"question.replied\", \"question.rejected\""))?;
    let executable = std::env::current_exe()?;
    let mut e = json!({"schema":1,"storage_metadata":true,"harness_kind":"rust-binary","harness_sha256":runtime::hash(&executable)?,"fux_sha256":runtime::hash(&fux)?,"zor_sha256":runtime::hash(&zor)?,"opencode_sha256":runtime::hash(&agent)?,"probe_sha256":runtime::hash(&probe)?,"limitation":"Real local OpenCode integration with a synthetic provider; not model quality, authenticated cloud or verified task success.","blocker":blocker,"requests":[],"outcomes":[]});
    if reload {
        let p = path.join("reload.mjs");
        fs::write(&p, include_bytes!("reload-plugin.mjs"))?;
        e["reload_plugin_sha256"] = runtime::hash(&p)?.into();
        e["reload_exec_kind"] = json!("rust-binary");
        e["reload_exec_sha256"] = runtime::hash(&executable)?.into();
        root.set_env("ZOR_FIXTURE_WRAPPER", p.to_str().context("wrapper")?.into());
        root.set_env(
            "ZOR_FIXTURE_RELOAD",
            path.join("reload").to_str().context("reload")?.into(),
        );
    }
    let mut provider = Provider::start_integration(&fixture)?;
    let config = json!({"model":"fixture/fixture","small_model":"fixture/fixture","enabled_providers":["fixture"],"permission":{"*":"deny","read":"allow","bash":"ask","question":"allow"},"provider":{"fixture":{"npm":"@ai-sdk/openai-compatible","name":"Local Fixture","options":{"baseURL":format!("http://127.0.0.1:{}/v1",provider.port),"apiKey":"fixture"},"models":{"fixture":{"name":"Fixture","limit":{"context":65536,"output":1024}}}}}});
    fs::write(
        path.join("work/opencode.json"),
        serde_json::to_vec(&config)?,
    )?;
    for (k, v) in [
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        ("TMPDIR", path.to_str().context("root")?.into()),
        (
            "OPENCODE_CONFIG",
            path.join("work/opencode.json")
                .to_str()
                .context("config")?
                .into(),
        ),
        (
            "ZOR_PROBE_LOG",
            path.join("events.jsonl").to_str().context("events")?.into(),
        ),
        (
            "OPENCODE_CONFIG_CONTENT",
            json!({"plugin":[probe]}).to_string(),
        ),
        ("OPENCODE_DISABLE_DEFAULT_PLUGINS", "true".into()),
        ("OPENCODE_DISABLE_MODELS_FETCH", "true".into()),
        ("OPENCODE_DISABLE_AUTOUPDATE", "true".into()),
    ] {
        root.set_env(k, v);
    }
    let control = path.join("fux/default.sock");
    let mut process = None;
    let mut pane = None;
    let mut instance = Value::Null;
    let outcome = (|| -> Result<()> {
        let mut c = root.command(&agent);
        c.arg("--version");
        let r = runtime::output(c, Duration::from_secs(10))?;
        ensure!(r.status.success(), "OpenCode version");
        e["opencode_version"] = String::from_utf8(r.stdout)?.trim().into();
        fs::write(
            path.join("config/fux/config.toml"),
            "default-command = { argv = [\"/bin/cat\"] }\n",
        )?;
        let mut c = root.command(&fux);
        c.arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(path.join("stderr"))?);
        process = Some(Owner(c.spawn()?));
        let owner = process.as_mut().unwrap();
        until(&path, owner, || Ok(control.exists().then_some(())))?;
        let listing = runtime::rpc(&control, json!({"id":1,"command":"list"}))?;
        instance = field(&listing, "/instance")?.clone();
        let work = path.join("work");
        let mut argv = vec![
            "start",
            "integrated",
            "--integration",
            "opencode",
            "--title",
            "OpenCode fixture",
            "--instance",
            instance.as_str().context("instance")?,
            "--workspace",
            "default",
            "--cwd",
            work.to_str().context("work")?,
            "--",
        ];
        if reload {
            argv.extend([
                executable.to_str().context("executable")?,
                "reload-opencode-fixture",
            ]);
        }
        argv.push(agent.to_str().context("agent")?);
        let launched = task(&root, &zor, &argv)?;
        pane = Some(field(&launched, "/session/target/pane")?.clone());
        let capture = || {
            runtime::rpc(
                &control,
                json!({"id":1,"command":"capture","instance":instance,"pane":pane,"max_bytes":131072}),
            )
        };
        e["registered"] = until(&path, owner, || {
            let v = task(&root, &zor, &["inspect", "integrated"])?;
            Ok(v.pointer("/launch/integration/producer")
                .is_some_and(|v| !v.is_null() && v != &json!(""))
                .then_some(v))
        })?;
        let healthy = || -> Result<Option<Value>> {
            let v = task(&root, &zor, &["adapter-status", "integrated"])?;
            Ok(
                (v["availability"] == "reachable" && v["heartbeat"]["status"] == "current")
                    .then_some(v),
            )
        };
        e["adapter_status"] = json!([until(&path, owner, healthy)?]);
        let mut profile = field(&e, "/registered/launch/integration")?.clone();
        e["plugin_sha256"] = runtime::hash(Path::new(
            field(&profile, "/plugin")?.as_str().context("plugin")?,
        ))?
        .into();
        until(&path, owner, || {
            Ok(capture()?["text"]
                .as_str()
                .context("text")?
                .contains("Ask anything")
                .then_some(()))
        })?;
        let operation = "retired-before-input";
        task(
            &root,
            &zor,
            &[
                "prepare",
                "integrated",
                "--operation",
                operation,
                "--text",
                "NEVER SUBMIT THIS FIXTURE",
                "--timeout-ms",
                "30000",
            ],
        )?;
        let reserved = task(&root, &zor, &["reserve", operation])?;
        let journal = Path::new(field(&profile, "/root")?.as_str().context("profile root")?)
            .join("journal.json");
        let mut state: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        state["prompts"][operation]["arm"] = json!({"producer":field(&profile,"/producer")?,"input_operation":field(&reserved,"/receipt/operation")?,"acknowledged":false,"input_started":false,"disarm_requested":false,"disarmed":false});
        let temp = journal.with_file_name("fixture-crash.tmp");
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?
            .write_all(&serde_json::to_vec(&state)?)?;
        fs::rename(&temp, &journal)?;
        let arm = json!({"v":1,"marker":field(&e,"/registered/launch/marker")?,"op":"arm","producer":field(&profile,"/producer")?,"prompt":{"operation":operation,"token":field(&reserved,"/report_token")?,"input_operation":field(&reserved,"/receipt/operation")?,"text":field(&reserved,"/text")?,"deadline_ms":field(&reserved,"/deadline_ms")?}});
        let socket = Path::new(field(&profile, "/socket")?.as_str().context("socket")?);
        let ack = adapter(socket, &arm)?;
        ensure!(
            ack["status"] == "armed"
                && field(&ack, "/token")? == field(&reserved, "/report_token")?
                && ack["operation"] == operation
                && field(&ack, "/producer")? == field(&profile, "/producer")?,
            "arm acknowledgement"
        );
        let before = capture()?["input_sequence"].clone();
        let retired = task(&root, &zor, &["abandon", operation])?;
        ensure!(
            task(&root, &zor, &["abandon", operation])? == retired,
            "abandon idempotence"
        );
        let late = adapter(socket, &arm)?;
        e["retirement"] = json!({"injection":"Synthetic durable pre-submit crash: adapter arm accepted, acknowledgement not journaled.","arm_status":ack["status"],"late_arm_status":late["status"],"prompt":retired,"input_before":before,"input_after":capture()?["input_sequence"]});
        let prompts = [
            "Reply with FIXTURE RESPONSE",
            "ZOR_TOOL_STOP: inspect the fixture",
            if blocker == "question" {
                "ZOR_QUESTION: ask which fixture option to use"
            } else {
                "ZOR_APPROVAL: request a harmless command"
            },
        ];
        for (i, text) in prompts.iter().enumerate() {
            let op = format!("prompt-{}", i + 1);
            task(
                &root,
                &zor,
                &[
                    "prepare",
                    "integrated",
                    "--operation",
                    &op,
                    "--text",
                    text,
                    "--timeout-ms",
                    "30000",
                ],
            )?;
            let submitted = task(&root, &zor, &["submit", &op])?;
            ensure!(
                submitted["arm"]["acknowledged"] == true
                    && field(&submitted, "/arm/producer")? == field(&profile, "/producer")?,
                "submit arm"
            );
            let result = until(&path, owner, || {
                let v = task(&root, &zor, &["wait", &op])?;
                if v["wait"] == "pending" {
                    return Ok(None);
                }
                ensure!(
                    v["wait"]
                        == if i == 2 {
                            "needs-input"
                        } else {
                            "response-observed"
                        },
                    "prompt outcome"
                );
                Ok(Some(v))
            })?;
            ensure!(
                field(&result, "/report_binding/message")? == field(&result, "/response/message")?,
                "response binding"
            );
            ensure!(
                field(&result, "/response/sequence")?
                    .as_u64()
                    .context("sequence")?
                    > field(&result, "/report_binding/sequence")?
                        .as_u64()
                        .context("sequence")?,
                "producer sequence"
            );
            ensure!(
                result["receipt"]["input_sequence"] == i + 1
                    && capture()?["input_sequence"] == i + 1,
                "input sequence"
            );
            e["outcomes"].as_array_mut().unwrap().push(result);
            if reload && i == 0 {
                let before = task(&root, &zor, &["inspect", "integrated"])?;
                let hook = || -> Result<Value> {
                    match fs::read(path.join("reload.json")) {
                        Ok(b) => Ok(serde_json::from_slice(&b).unwrap_or(json!({}))),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
                        Err(e) => Err(e.into()),
                    }
                };
                let mut recovered = json!({"before":profile,"target_before":field(&before,"/session/target")?,"input_before":capture()?["input_sequence"],"hook_before":hook()?});
                fs::write(path.join("reload"), "reload")?;
                recovered["hook_after"] = until(&path, owner, || {
                    let v = hook()?;
                    Ok((v["generation"] == 2).then_some(v))
                })?;
                let after = task(&root, &zor, &["inspect", "integrated"])?;
                profile = field(&after, "/launch/integration")?.clone();
                recovered["after"] = profile.clone();
                recovered["target_after"] = field(&after, "/session/target")?.clone();
                recovered["input_after"] = capture()?["input_sequence"].clone();
                recovered["retained_response_after"] =
                    field(&task(&root, &zor, &["wait", &op])?, "/response")?.clone();
                e["producer_reload"] = recovered;
            }
        }
        let current = task(&root, &zor, &["inspect", "integrated"])?;
        ensure!(current["task"]["outcome"] == "open", "task outcome");
        e["task_outcome"] = json!("open");
        e["capture"] = until(&path, owner, || {
            let v = capture()?;
            Ok(v["text"]
                .as_str()
                .context("text")?
                .contains(if blocker == "permission" {
                    "Permission required"
                } else {
                    "Which fixture option?"
                })
                .then_some(v))
        })?;
        let received = field(&e["outcomes"][2], "/response/received_ms")?
            .as_u64()
            .context("received")?;
        let heartbeat = until(&path, owner, || {
            let Some(v) = healthy()? else { return Ok(None) };
            Ok((field(&v, "/heartbeat/received_ms")?
                .as_u64()
                .context("received")?
                > received
                && v.pointer("/heartbeat/observation/state") == Some(&json!("blocked")))
            .then_some(v))
        })?;
        e["adapter_status"].as_array_mut().unwrap().push(heartbeat);
        let mut c = root.command(&zor);
        c.arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut service = Owner(c.spawn()?);
        let observation = (|| -> Result<Value> {
            until(&path, owner, || {
                Ok(path.join("zor/control.sock").exists().then_some(()))
            })?;
            until(&path, owner, || {
                ensure!(service.0.try_wait()?.is_none(), "zor service exited");
                let mut c = root.command(&zor);
                c.args(["dashboard", "--once"]);
                let r = runtime::output(c, Duration::from_secs(8))?;
                if !r.status.success()
                    && r.stderr == b"zor: zor journal is busy; retry the operation ID\n"
                {
                    return Ok(None);
                }
                ensure!(r.status.success(), "dashboard");
                let v: Value = serde_json::from_slice(&r.stdout)?;
                let found = v["rows"].as_array().context("rows")?.iter().any(|r| {
                    r["kind"] == "observation"
                        && r.pointer("/target/pane") == pane.as_ref()
                        && r["status"] == "blocked"
                        && r.pointer("/evidence/source") == Some(&json!("integration"))
                });
                Ok(found.then_some(v))
            })
        })();
        let stopped = service.stop();
        e["zor_service_exit"] = json!(stopped?.code());
        e["dashboard"] = observation?;
        e["observation_event_count"] = json!(
            events(&path.join("events.jsonl"))?
                .as_array()
                .context("events")?
                .len()
        );
        Ok(())
    })();
    let mut errors = Vec::new();
    if let Some(owner) = process.as_mut() {
        let kill = (|| -> Result<()> {
            if owner.0.try_wait()?.is_none()
                && let Some(pane) = pane
            {
                runtime::rpc(
                    &control,
                    json!({"id":1,"command":"kill","instance":instance,"pane":pane}),
                )?;
            }
            Ok(())
        })();
        if let Err(e) = kill {
            errors.push(format!("pane: {e:#}"));
        }
        match owner.stop() {
            Ok(v) => e["fux_exit"] = json!(v.code()),
            Err(e) => errors.push(format!("fux: {e:#}")),
        }
    }
    let stopped = provider.close();
    e["provider_thread_stopped"] = json!(stopped.is_ok());
    if let Err(e) = stopped {
        errors.push(format!("provider: {e:#}"));
    }
    e["requests"] = json!(provider.records());
    match events(&path.join("events.jsonl")) {
        Ok(v) => e["events"] = v,
        Err(e) => errors.push(format!("events: {e:#}")),
    }
    e = normalized(&e, &path)?;
    let mut encoded = serde_json::to_string_pretty(&e)?;
    for token in e["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["report_token"].as_str())
        .chain(
            e.pointer("/retirement/prompt/report_token")
                .and_then(Value::as_str),
        )
    {
        ensure!(!token.is_empty(), "empty report token");
        encoded = encoded.replace(token, "<PROMPT_TOKEN>");
    }
    e = serde_json::from_str(&encoded)?;
    fs::write(output.join("diagnostic.json"), &encoded)?;
    outcome.context(format!("cleanup errors: {errors:?}"))?;
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    integration::validate(&e)?;
    fs::write(output.join("integration.json"), encoded + "\n")?;
    println!("{}", output.join("integration.json").display());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_adapter_backlog_cannot_block_capture_cleanup() -> Result<()> {
        use nix::{
            fcntl::{FcntlArg, OFlag, fcntl},
            sys::socket::{self, AddressFamily, Backlog, SockFlag, SockType, UnixAddr},
        };
        use std::os::fd::AsRawFd;
        let root = tempfile::tempdir()?;
        let path = root.path().join("adapter.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        socket::listen(&listener, Backlog::new(1)?)?;
        let mut clients = Vec::new();
        let mut saturated = false;
        for _ in 0..512 {
            let fd = socket::socket(
                AddressFamily::Unix,
                SockType::Stream,
                SockFlag::empty(),
                None,
            )?;
            fcntl(&fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
            match socket::connect(fd.as_raw_fd(), &UnixAddr::new(&path)?) {
                Ok(()) => clients.push(fd),
                Err(nix::errno::Errno::EAGAIN | nix::errno::Errno::EINPROGRESS) => {
                    clients.push(fd);
                    saturated = true;
                    break;
                }
                Err(nix::errno::Errno::ECONNREFUSED) if !clients.is_empty() => {
                    // macOS can refuse new connects once this live listener's queue is full.
                    saturated = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        ensure!(saturated, "fixture did not fill adapter backlog");
        let started = Instant::now();
        let result = adapter(&path, &json!({"op":"arm"}));
        ensure!(result.is_err(), "unaccepted adapter returned response");
        ensure!(
            started.elapsed() < Duration::from_secs(5),
            "adapter connection was not bounded"
        );
        Ok(())
    }
}
