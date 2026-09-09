//! Real-binary scenarios migrated from tests/verify, using only public APIs.
mod binding_adapter;
mod control_workflow;
mod detach_drain;
mod event_sync;
mod events_proxy;
mod final_records;
mod input_receipts;
mod local_attachment;
mod local_tty;
mod migration;
mod observer;
mod rejection;
mod run_command;
mod service_fixture;
mod service_tasks;
mod service_worktrees;
mod source_workers;
mod viewer;
mod zor_bindings;
mod zor_changes;
mod zor_check_workers;
mod zor_checks;
mod zor_dashboard;
mod zor_events;
mod zor_groups;
mod zor_headless;
mod zor_launch;
mod zor_native;
mod zor_producers;
mod zor_recovery;
mod zor_service;
mod zor_sources;
mod zor_tasks;
mod zor_workflow;
mod zor_worktree;
use crate::support::{
    local::{Root, completed, rpc, until},
    process::wait,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};

pub fn run(args: Vec<String>) -> Result<()> {
    let name = args.first().context("missing scenario name")?;
    let binary = Path::new(args.get(1).context("missing fux binary")?).canonicalize()?;
    match name.as_str() {
        "empty-arguments" => empty_arguments(&binary),
        "utf8-capture" => utf8_capture(&binary),
        "protocol-rejection" => rejection::run(&binary),
        "local-attachment" => local_attachment::run(&binary),
        "local-tty" => local_tty::run(&binary),
        "detach-drain" => detach_drain::run(&binary),
        "control-workflow" => control_workflow::run(&binary),
        "run-command" => run_command::run(&binary),
        "final-records" => final_records::run(&binary),
        "event-sync" => event_sync::run(&binary),
        "input-receipts" => input_receipts::run(&binary),
        "migration" => migration::run(&binary),
        "viewer" => viewer::run(&binary),
        "zor-dashboard" => zor_dashboard::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
            &Path::new(args.get(3).context("missing notifier fixture binary")?).canonicalize()?,
        ),
        "zor-worktree" => zor_worktree::run(
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-workflow" => zor_workflow::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
            &Path::new(args.get(3).context("missing Codex fixture binary")?).canonicalize()?,
        ),
        "zor-groups" | "zor-group-scheduler" => zor_groups::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
            name == "zor-group-scheduler",
        ),
        "zor-service" => zor_service::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-bindings" => zor_bindings::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-sources" => zor_sources::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-checks" => zor_checks::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-tasks" => zor_tasks::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-launch" => zor_launch::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-events" => zor_events::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-producers" => zor_producers::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-recovery" => zor_recovery::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-check-workers" => zor_check_workers::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-changes" => zor_changes::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        "zor-headless" => zor_headless::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
            &Path::new(args.get(3).context("missing argv fixture binary")?).canonicalize()?,
        ),
        "zor-native" => zor_native::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
            &Path::new(args.get(3).context("missing Codex fixture binary")?).canonicalize()?,
        ),
        "observer" => observer::run(
            &binary,
            &Path::new(args.get(2).context("missing zor binary")?).canonicalize()?,
        ),
        _ => anyhow::bail!("unknown scenario: {name}"),
    }
}

pub fn worker(args: Vec<String>) -> Result<()> {
    if matches!(
        args.first().map(String::as_str),
        Some(
            "source-produce"
                | "source-unsafe"
                | "source-recovered"
                | "source-renamed"
                | "source-held"
                | "source-capacity"
        )
    ) {
        return source_workers::run(&args);
    }
    if args.first().map(String::as_str) == Some("service-descendant") {
        return service_fixture::descendant(&args);
    }
    if matches!(
        args.first().map(String::as_str),
        Some("check-large" | "check-ordered" | "check-hold")
    ) {
        return zor_checks::worker(&args);
    }
    if args.first().map(String::as_str) == Some("worktree-filter") {
        return zor_worktree::filter(Path::new(args.get(1).context("missing filter root")?));
    }
    if args.first().map(String::as_str) == Some("workflow-check") {
        ensure!(
            fs::read_to_string("output.txt")? == *args.get(1).context("expected content")?,
            "worker content mismatch"
        );
        return Ok(());
    }
    if matches!(args.first().map(String::as_str), Some("workflow" | "group")) {
        use std::io::BufRead;
        let marker = Path::new(args.get(1).context("completion marker")?);
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            let text = line.trim();
            fs::write("output.txt", text)?;
            for argv in [
                vec!["add", "output.txt"],
                vec!["commit", "--allow-empty", "-m", "worker output"],
            ] {
                let mut command = std::process::Command::new("/usr/bin/git");
                command.args(argv);
                let result =
                    crate::support::process::output(command, Duration::from_secs(5), 1024 * 1024)?;
                ensure!(
                    result.status.success(),
                    "worker git failed: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            if args[0] == "group" {
                use std::io::Write;
                writeln!(
                    fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(marker)?,
                    "{text}"
                )?;
            } else {
                fs::write(marker, text)?;
            }
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("hold") {
        let root = Path::new(args.get(1).context("hold root")?);
        fs::write(root.join(args.get(2).context("hold marker")?), b"")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !root.join("release").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("old-manager") {
        return migration::old_manager(Path::new(args.get(1).context("old manager root")?));
    }
    if matches!(
        args.first().map(String::as_str),
        Some("event-titles" | "input-bytes")
    ) {
        let input = std::io::stdin();
        let mut termios = nix::sys::termios::tcgetattr(&input)?;
        nix::sys::termios::cfmakeraw(&mut termios);
        nix::sys::termios::tcsetattr(&input, nix::sys::termios::SetArg::TCSANOW, &termios)?;
        let mut input = input.lock();
        let mut output = std::io::stdout().lock();
        output.write_all(b"READY\r\n")?;
        output.flush()?;
        let mut bytes = [0; 1024];
        loop {
            let count = input.read(&mut bytes)?;
            if count == 0 {
                return Ok(());
            }
            for byte in &bytes[..count] {
                if args[0] == "event-titles" {
                    output.write_all(b"\x1b]2;title-")?;
                    output.write_all(&[*byte, 7])?;
                } else {
                    if *byte == b'!' {
                        output.write_all(b"STOPPED\r\n")?;
                        output.flush()?;
                        std::thread::sleep(Duration::from_secs(60));
                    }
                    write!(output, "BYTE {byte:02x}\r\n")?;
                }
                output.flush()?;
            }
        }
    }
    ensure!(
        args.first().map(String::as_str) == Some("split-utf8"),
        "unknown fixture worker"
    );
    let release = Path::new(args.get(1).context("missing release marker")?);
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(b"prefix \xc3")?;
    stdout.flush()?;
    until(Duration::from_secs(10), || {
        Ok(release.exists().then_some(()))
    })?;
    stdout.write_all(&"Ü 🐝 end".as_bytes()[1..])?;
    stdout.flush()?;
    std::thread::sleep(Duration::from_secs(60));
    Ok(())
}

fn utf8_capture(binary: &Path) -> Result<()> {
    let worker = std::env::current_exe()?.to_string_lossy().into_owned();
    let root = Root::new("fux-utf8-rs-", &["/bin/cat".into()])?;
    let release = root.path().join("continue");
    let argv = vec![
        worker,
        "fixture-worker".into(),
        "split-utf8".into(),
        release.to_string_lossy().into_owned(),
    ];
    fs::write(
        root.path().join("config/fux/config.toml"),
        format!(
            "default-command = {{ argv = {} }}\n",
            serde_json::to_string(&argv)?
        ),
    )?;
    let mut server = root.server(binary)?;
    let listing = completed(&root.control(), json!({"id":1,"command":"list"}))?;
    let pane = &listing["workspaces"][0]["tabs"][0]["panes"][0]["id"];
    let capture = || -> Result<Value> {
        Ok(completed(&root.control(), json!({"id":2,"command":"capture","pane":pane,"instance":listing["instance"],"attrs":false,"scrollback":0,"max_bytes":131072}))?["text"].clone())
    };
    until(Duration::from_secs(8), || {
        Ok((capture()? == "prefix ").then_some(()))
    })?;
    fs::write(&release, b"")?;
    until(Duration::from_secs(8), || {
        Ok((capture()? == "prefix Ü 🐝 end").then_some(()))
    })?;
    server.finish()?;
    println!("PASS split UTF-8 capture preserves subsequent ASCII and wide characters");
    Ok(())
}

fn empty_arguments(binary: &Path) -> Result<()> {
    let argv: Vec<String> = [
        "/bin/sh",
        "-c",
        "printf \"ARGC=%s\\nFIRST=<%s>\\nSECOND=<%s>\\n\" \"$#\" \"$1\" \"$2\"; read line",
        "fixture",
        "",
        "tail",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let root = Root::new("fargv-rs-", &argv)?;
    let mut server = root.server(binary)?;
    let listing = completed(&root.control(), json!({"id":1,"command":"list"}))?;
    let instance = listing["instance"].clone();
    let pane = listing["workspaces"][0]["tabs"][0]["panes"][0]["id"].clone();
    let new = completed(
        &root.control(),
        json!({"id":1,"command":"split","axis":"horizontal","instance":instance,"argv":argv}),
    )?["pane"]
        .clone();
    let split = completed(&root.control(), json!({"id":1,"command":"split","instance":instance,"target":pane,"axis":"horizontal","argv":argv}))?["pane"].clone();
    let panes = [pane, new, split];
    let visible = |text: &str| {
        ["ARGC=2", "FIRST=<>", "SECOND=<tail>"]
            .iter()
            .all(|marker| text.contains(marker))
    };
    for pane in &panes {
        until(Duration::from_secs(10), || {
            let value = completed(
                &root.control(),
                json!({"id":1,"command":"capture","instance":instance,"pane":pane,"max_bytes":131072}),
            )?;
            Ok(visible(value["text"].as_str().context("capture text")?).then_some(()))
        })?;
    }
    for bad in [json!([""]), json!(["/bin/sh", "\0"])] {
        let reply = rpc(
            &root.control(),
            json!({"id":1,"command":"split","axis":"horizontal","instance":instance,"argv":bad}),
        )?;
        ensure!(
            reply["status"] == "failed" && reply["error"]["code"] == "invalid-request",
            "invalid argv accepted: {reply}"
        );
    }
    for pane in &panes {
        completed(
            &root.control(),
            json!({"id":1,"command":"kill","instance":instance,"pane":pane}),
        )?;
    }
    for pane in panes {
        let record = until(Duration::from_secs(10), || {
            let mut output = tempfile::tempfile()?;
            let mut child = root
                .command(binary)
                .args([
                    "final",
                    "--instance",
                    instance.as_str().context("instance")?,
                    &pane.to_string(),
                ])
                .stdin(Stdio::null())
                .stdout(output.try_clone()?)
                .stderr(Stdio::null())
                .spawn()?;
            let result = wait(&mut child, Duration::from_secs(3));
            if result.is_err() {
                let _ = child.kill();
                let _ = wait(&mut child, Duration::from_secs(3));
            }
            if !result?.success() {
                return Ok(None);
            }
            use std::io::{Seek, SeekFrom};
            output.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            output.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 1024 * 1024, "final reply byte limit");
            let reply: Value = serde_json::from_slice(&bytes)?;
            Ok(Some(reply["result"]["value"]["record"].clone()))
        })?;
        ensure!(record["command"] == json!(argv), "final argv changed");
        ensure!(
            visible(
                record["capture"]["text"]
                    .as_str()
                    .context("final capture")?
            ),
            "final output changed"
        );
    }
    server.finish()?;
    println!(
        "empty config/default-target/explicit-target split arguments and retained final argv passed"
    );
    Ok(())
}
