//! No-input lifecycle capture; explicit termination never establishes natural completion.
use crate::{
    lifecycle::validate,
    runtime::{self, Owner, Root},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

fn until<T>(owner: &mut Owner, mut check: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        ensure!(owner.0.try_wait()?.is_none(), "owned fux exited");
        if let Some(value) = check()? {
            return Ok(value);
        }
        ensure!(Instant::now() < deadline, "lifecycle observation deadline");
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub fn run(args: Vec<String>) -> Result<()> {
    let mut paths = BTreeMap::new();
    let mut args = args.into_iter();
    while let Some(key) = args.next() {
        ensure!(
            ["--fux", "--agent", "--output"].contains(&key.as_str()),
            "unknown argument {key}"
        );
        ensure!(
            paths
                .insert(key, args.next().context("missing option value")?)
                .is_none(),
            "duplicate option"
        );
    }
    let fux = Path::new(paths.get("--fux").context("--fux required")?).canonicalize()?;
    let agent = std::path::absolute(paths.get("--agent").context("--agent required")?)?;
    agent.canonicalize()?;
    let output = PathBuf::from(paths.get("--output").context("--output required")?);
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::DirBuilder::new().mode(0o700).create(&output)?;
    let root = Root::new("zlife-rs-")?;
    let mut value = json!({"schema":1,"captured_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"agent":agent,"binary_sha256":runtime::hash(&agent)?,"fux_sha256":runtime::hash(&fux)?,"harness_sha256":runtime::hash(&std::env::current_exe()?)?,"harness_kind":"rust-binary","input_sent":false,"stages":[],"exit_trigger":"explicit fux kill; not natural completion","limitation":"Fresh unauthenticated HOME/XDG; viewport snapshots, not working or task-completion evidence."});
    let mut version = root.command(&agent);
    version.arg("--version");
    let version = runtime::output(version, Duration::from_secs(10))?;
    ensure!(
        version.status.success(),
        "agent version failed: {}",
        String::from_utf8_lossy(&version.stderr)
    );
    value["version"] = String::from_utf8(version.stdout)?.trim().into();
    fs::write(
        root.path().join("config/fux/config.toml"),
        format!(
            "default-command = {{ argv = {} }}\n",
            serde_json::to_string(&[&agent])?
        ),
    )?;
    let mut owner = Owner(
        root.command(&fux)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let control = root.path().join("fux/default.sock");
    let scenario = (|| -> Result<()> {
        until(&mut owner, || Ok(control.exists().then_some(())))?;
        let listing = runtime::rpc(&control, json!({"id":1,"command":"list"}))?;
        value["instance"] = listing["instance"].clone();
        value["pane"] = listing["workspaces"][0]["tabs"][0]["panes"][0].clone();
        let instance = value["instance"].clone();
        let pane = value["pane"]["id"].clone();
        let command = |name: &str, mut fields: Value| {
            fields["id"] = 1.into();
            fields["command"] = name.into();
            fields["instance"] = instance.clone();
            runtime::rpc(&control, fields)
        };
        let capture = || command("capture", json!({"pane":pane,"max_bytes":131072}));
        until(&mut owner, || {
            Ok(capture()?["text"]
                .as_str()
                .is_some_and(|t| !t.is_empty())
                .then_some(()))
        })?;
        let mut other = Value::Null;
        for name in ["initial", "narrow", "restored"] {
            if name == "narrow" {
                other = command(
                    "split",
                    json!({"axis":"horizontal","target":pane,"argv":["/bin/cat"]}),
                )?["pane"]
                    .clone();
            } else if name == "restored" {
                command("kill", json!({"pane":other}))?;
            }
            let mut captures = Vec::new();
            for _ in 0..15 {
                captures.push(capture()?);
                std::thread::sleep(Duration::from_millis(100));
            }
            value["stages"]
                .as_array_mut()
                .context("stages")?
                .push(json!({"name":name,"instance":instance,"pane":pane,"captures":captures}));
        }
        command("kill", json!({"pane":pane}))?;
        let final_record = until(&mut owner, || {
            let mut command = root.command(&fux);
            command.args([
                "final",
                "--instance",
                instance.as_str().context("instance")?,
                &pane.to_string(),
            ]);
            let result = runtime::output(command, Duration::from_secs(3))?;
            if !result.status.success() {
                let mut bytes = result.stdout;
                bytes.extend(result.stderr);
                value["final_problem"] = String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(1024)
                    .collect::<String>()
                    .into();
                return Ok(None);
            }
            Ok(Some(
                serde_json::from_slice::<Value>(&result.stdout)?["result"]["value"]["record"]
                    .clone(),
            ))
        })?;
        value["final"] = final_record;
        Ok(())
    })();
    let cleanup = owner.stop();
    if let Ok(status) = &cleanup {
        value["server_exit"] = json!(status.code().unwrap_or(-1));
    }
    let write = |name: &str, value: &Value| -> Result<()> {
        fs::write(
            output.join(name),
            serde_json::to_string_pretty(value)?.replace(
                root.path().to_str().context("capture root")?,
                "<CAPTURE_ROOT>",
            ) + "\n",
        )?;
        Ok(())
    };
    write("diagnostic.json", &value)?;
    let errors: Vec<_> = [scenario, cleanup.map(|_| ())]
        .into_iter()
        .filter_map(Result::err)
        .map(|e| format!("{e:#}"))
        .collect();
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    validate(&value)?;
    write("lifecycle.json", &value)?;
    println!("{}", output.join("lifecycle.json").display());
    Ok(())
}
