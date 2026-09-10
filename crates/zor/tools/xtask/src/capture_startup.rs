//! No-input startup capture with explicit effects for the original cleanup regressions.
use crate::runtime::{self, Owner, Root};
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

trait Process {
    fn poll(&mut self) -> Result<Option<i32>>;
    fn pid(&self) -> u32;
    fn terminate(&mut self) -> Result<()>;
    fn kill(&mut self) -> Result<()>;
    fn wait(&mut self, seconds: u64) -> Result<i32>;
}
fn allow_disappeared(result: Result<()>) -> Result<()> {
    match result {
        Err(error)
            if error.downcast_ref::<nix::errno::Errno>() == Some(&nix::errno::Errno::ESRCH)
                || error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.raw_os_error() == Some(nix::errno::Errno::ESRCH as i32)) =>
        {
            Ok(())
        }
        other => other,
    }
}
impl Process for Owner {
    fn poll(&mut self) -> Result<Option<i32>> {
        Ok(self.0.try_wait()?.map(|s| s.code().unwrap_or(-1)))
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
    fn terminate(&mut self) -> Result<()> {
        if self.0.try_wait()?.is_none() {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(self.0.id())?),
                nix::sys::signal::Signal::SIGTERM,
            )?;
        }
        Ok(())
    }
    fn kill(&mut self) -> Result<()> {
        if self.0.try_wait()?.is_none() {
            self.0.kill()?;
        }
        Ok(())
    }
    fn wait(&mut self, seconds: u64) -> Result<i32> {
        Ok(Owner::wait(self, Duration::from_secs(seconds))?
            .code()
            .unwrap_or(-1))
    }
}
trait Effects {
    type Child: Process;
    fn version(&mut self, root: &Root, agent: &Path) -> Result<Vec<u8>>;
    fn spawn(&mut self, root: &Root, fux: &Path) -> Result<Self::Child>;
    fn rpc(&mut self, path: &Path, request: Value) -> Result<Value>;
    fn now(&mut self) -> f64;
    fn sleep(&mut self, seconds: f64);
    fn publish(&mut self, path: &Path, value: &Value, root: &Path) -> Result<()>;
}
struct Real {
    start: Instant,
}
impl Effects for Real {
    type Child = Owner;
    fn version(&mut self, root: &Root, agent: &Path) -> Result<Vec<u8>> {
        let mut command = root.command(agent);
        command.arg("--version");
        let result = runtime::output(command, Duration::from_secs(10))?;
        ensure!(
            result.status.success(),
            "agent version failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(result.stdout)
    }
    fn spawn(&mut self, root: &Root, fux: &Path) -> Result<Owner> {
        Ok(Owner(
            root.command(fux)
                .arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        ))
    }
    fn rpc(&mut self, path: &Path, request: Value) -> Result<Value> {
        runtime::rpc(path, request)
    }
    fn now(&mut self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
    fn sleep(&mut self, seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }
    fn publish(&mut self, path: &Path, value: &Value, root: &Path) -> Result<()> {
        publish(path, value, root)
    }
}
fn publish(path: &Path, value: &Value, root: &Path) -> Result<()> {
    fs::write(
        path,
        serde_json::to_string_pretty(value)?
            .replace(root.to_str().context("capture root")?, "<CAPTURE_ROOT>")
            + "\n",
    )?;
    Ok(())
}
struct Options {
    fux: PathBuf,
    agent: PathBuf,
    output: PathBuf,
    duration: u64,
}
fn options(args: Vec<String>) -> Result<Options> {
    let mut values = BTreeMap::new();
    let mut args = args.into_iter();
    while let Some(key) = args.next() {
        ensure!(
            ["--fux", "--agent", "--output", "--duration"].contains(&key.as_str()),
            "unknown option {key}"
        );
        ensure!(
            values
                .insert(key, args.next().context("missing option value")?)
                .is_none(),
            "duplicate option"
        );
    }
    let duration = values
        .get("--duration")
        .map(String::as_str)
        .unwrap_or("6")
        .parse::<u64>()
        .context("--duration must be 1 through 30 seconds")?;
    ensure!(
        (1..=30).contains(&duration),
        "--duration must be 1 through 30 seconds"
    );
    Ok(Options {
        fux: values.get("--fux").context("--fux required")?.into(),
        agent: values.get("--agent").context("--agent required")?.into(),
        output: values.get("--output").context("--output required")?.into(),
        duration,
    })
}
#[derive(Debug)]
pub struct Usage(pub String);
impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Usage {}
pub fn run(args: Vec<String>) -> Result<()> {
    execute(
        options(args).map_err(|error| Usage(error.to_string()))?,
        &mut Real {
            start: Instant::now(),
        },
    )
}
fn execute(options: Options, effects: &mut impl Effects) -> Result<()> {
    let fux = options.fux.canonicalize()?;
    let agent = std::path::absolute(&options.agent)?;
    agent.canonicalize()?;
    if let Some(parent) = options
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::DirBuilder::new().mode(0o700).create(&options.output)?;
    let root = Root::new("zs-rs-")?;
    let version = String::from_utf8(effects.version(&root, &agent)?)?
        .trim()
        .to_owned();
    fs::write(
        root.path().join("config/fux/config.toml"),
        format!(
            "default-command = {{ argv = {} }}\n",
            serde_json::to_string(&[&agent])?
        ),
    )?;
    let mut owner = effects.spawn(&root, &fux)?;
    let control = root.path().join("fux/default.sock");
    let mut captures: Vec<Value> = Vec::new();
    let mut pane = None;
    let mut instance = None;
    let scenario = (|| -> Result<()> {
        let deadline = effects.now() + 10.;
        while !control.exists() {
            ensure!(
                owner.poll()?.is_none() && effects.now() <= deadline,
                "fux startup failed"
            );
            effects.sleep(0.05);
        }
        let listing = effects.rpc(&control, json!({"id":1,"command":"list"}))?;
        pane = Some(listing["workspaces"][0]["tabs"][0]["panes"][0]["id"].clone());
        instance = Some(listing["instance"].clone());
        let started = effects.now();
        while effects.now() - started < options.duration as f64 {
            let capture=effects.rpc(&control,json!({"id":2,"command":"capture","pane":pane,"instance":instance,"attrs":false,"scrollback":0,"max_bytes":131072}))?;
            let elapsed = ((effects.now() - started) * 1000.) as u64;
            let changed = if let Some(last) = captures.last() {
                let mut changed = false;
                for key in ["text", "rows", "columns", "title", "progress", "truncated"] {
                    changed |= capture.get(key).with_context(|| format!("capture {key}"))?
                        != last["capture"]
                            .get(key)
                            .with_context(|| format!("prior capture {key}"))?;
                }
                changed
            } else {
                true
            };
            if changed {
                captures.push(json!({"elapsed_ms":elapsed,"capture":capture,"samples":1,"last_elapsed_ms":elapsed,"last_revision":capture.get("revision").context("capture revision")?}));
            } else {
                let last = captures.last_mut().context("prior sample")?;
                last["samples"] = json!(last["samples"].as_u64().context("sample count")? + 1);
                last["last_elapsed_ms"] = elapsed.into();
                last["last_revision"] =
                    capture.get("revision").context("capture revision")?.clone();
            }
            effects.sleep(0.1);
        }
        Ok(())
    })();
    let mut diagnostic = json!({"agent_version":version,"captures":captures,"cleanup":"pending","server_pid":owner.pid(),"input_sent":false});
    let diagnostic_path = options.output.join("diagnostic.json");
    let initial_publication = effects.publish(&diagnostic_path, &diagnostic, root.path());
    // Every cleanup action remains reachable even when publishing or pane closure fails.
    if pane.is_some()
        && instance.is_some()
        && owner.poll().is_ok_and(|status| status.is_none())
        && let Err(error) = effects.rpc(
            &control,
            json!({"id":3,"command":"kill","instance":instance,"pane":pane}),
        )
    {
        diagnostic["close_error"] = format!("{error:#}").into();
    }
    let cleanup = (|| -> Result<()> {
        if owner.poll()?.is_none() {
            allow_disappeared(owner.terminate())?;
        }
        let status = match owner.wait(10) {
            Ok(status) => status,
            Err(error) => {
                allow_disappeared(owner.kill())?;
                owner.wait(5)?;
                diagnostic["cleanup"] =
                    "server force-killed and reaped after timeout; pane reaping unconfirmed".into();
                return Err(error.context("fux cleanup exceeded deadline"));
            }
        };
        diagnostic["cleanup"] = format!("server reaped with exit code {status}").into();
        ensure!(status == 0, "fux exited unsuccessfully: {status}");
        Ok(())
    })();
    if let Err(error) = &cleanup {
        diagnostic["cleanup_error"] = format!("{error:#}").into();
    }
    let final_publication = effects.publish(&diagnostic_path, &diagnostic, root.path());
    let errors: Vec<_> = [scenario, initial_publication, cleanup, final_publication]
        .into_iter()
        .filter_map(Result::err)
        .map(|e| format!("{e:#}"))
        .collect();
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    ensure!(
        captures.iter().any(|c| c["capture"]["text"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty())),
        "no startup screen captured"
    );
    let platform = if cfg!(target_os = "macos") {
        "Darwin"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        std::env::consts::OS
    };
    let evidence = json!({"schema":1,"captured_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"agent_version":version,"observation_window_seconds":options.duration,"fux_binary_sha256":runtime::hash(&fux)?,"binary_sha256":runtime::hash(&agent)?,"platform":platform,"argv":[agent.file_name().context("agent name")?],"input_sent":false,"isolation":"Fresh HOME, XDG directories, CODEX_HOME and CLAUDE_CONFIG_DIR; environment allowlist.","limitation":"Startup only. No authenticated request, working, completion, exit or resize coverage.","redactions":{"temporary_directory":"<CAPTURE_ROOT>"},"captures":captures});
    effects.publish(&options.output.join("startup.json"), &evidence, root.path())?;
    println!("{}", options.output.join("startup.json").display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    #[derive(Default)]
    struct State {
        calls: Vec<String>,
        status: Option<i32>,
        spawns: usize,
    }
    struct FakeProcess {
        case: String,
        state: Rc<RefCell<State>>,
    }
    impl Process for FakeProcess {
        fn poll(&mut self) -> Result<Option<i32>> {
            Ok(self.state.borrow().status)
        }
        fn pid(&self) -> u32 {
            123
        }
        fn terminate(&mut self) -> Result<()> {
            self.state.borrow_mut().calls.push("terminate".into());
            if self.case == "signal_disappeared" {
                return Err(nix::errno::Errno::ESRCH.into());
            }
            Ok(())
        }
        fn kill(&mut self) -> Result<()> {
            self.state.borrow_mut().calls.push("kill".into());
            if self.case == "kill_disappeared" {
                return Err(
                    std::io::Error::from_raw_os_error(nix::errno::Errno::ESRCH as i32).into(),
                );
            }
            Ok(())
        }
        fn wait(&mut self, seconds: u64) -> Result<i32> {
            self.state
                .borrow_mut()
                .calls
                .push(format!("wait:{seconds}"));
            ensure!(
                self.case != "unreaped"
                    && !(["timeout", "kill_disappeared"].contains(&self.case.as_str())
                        && seconds == 10),
                "fixture timeout"
            );
            let code = if self.case == "nonzero" { 1 } else { 0 };
            self.state.borrow_mut().status = Some(code);
            Ok(code)
        }
    }
    struct Fake {
        case: String,
        state: Rc<RefCell<State>>,
        clock: u64,
        agent: PathBuf,
    }
    impl Effects for Fake {
        type Child = FakeProcess;
        fn version(&mut self, _: &Root, _: &Path) -> Result<Vec<u8>> {
            Ok(if self.case == "invalid_version" {
                vec![255]
            } else {
                b"fixture 1\n".to_vec()
            })
        }
        fn spawn(&mut self, root: &Root, _: &Path) -> Result<FakeProcess> {
            self.state.borrow_mut().spawns += 1;
            let control = root.path().join("fux/default.sock");
            fs::create_dir_all(control.parent().context("control parent")?)?;
            fs::write(control, b"")?;
            if self.case == "invoked_name" {
                ensure!(
                    fs::read_to_string(root.path().join("config/fux/config.toml"))?
                        .contains(self.agent.to_str().context("agent")?),
                    "symlink name changed"
                );
            }
            Ok(FakeProcess {
                case: self.case.clone(),
                state: self.state.clone(),
            })
        }
        fn rpc(&mut self, _: &Path, request: Value) -> Result<Value> {
            match request["command"].as_str() {
                Some("list") => {
                    Ok(json!({"instance":"fixture","workspaces":[{"tabs":[{"panes":[{"id":1}]}]}]}))
                }
                Some("kill") => {
                    ensure!(self.case != "close_failure", "fixture close failure");
                    Ok(Value::Null)
                }
                _ => Ok(
                    json!({"text":"fixture","rows":23,"columns":80,"title":"","progress":null,"truncated":false,"revision":1}),
                ),
            }
        }
        fn now(&mut self) -> f64 {
            let now = self.clock;
            self.clock += 1;
            now as f64
        }
        fn sleep(&mut self, _: f64) {}
        fn publish(&mut self, path: &Path, value: &Value, root: &Path) -> Result<()> {
            ensure!(
                self.case != "diagnostic_failure"
                    || path.file_name() != Some(std::ffi::OsStr::new("diagnostic.json")),
                "fixture publication failure"
            );
            publish(path, value, root)
        }
    }
    #[test]
    fn duration_rejected_before_filesystem_and_spawn() -> Result<()> {
        for duration in ["0", "31", "-1"] {
            let args = [
                "--fux",
                "/absent/fux",
                "--agent",
                "/absent/agent",
                "--output",
                "/absent/output",
                "--duration",
                duration,
            ]
            .map(str::to_owned)
            .to_vec();
            let error = options(args).err().context("invalid duration accepted")?;
            ensure!(
                error.to_string().contains("--duration"),
                "duration validation happened too late"
            );
        }
        Ok(())
    }
    #[test]
    fn original_publication_and_cleanup_failure_matrix() -> Result<()> {
        for case in [
            "normal",
            "diagnostic_failure",
            "close_failure",
            "timeout",
            "unreaped",
            "nonzero",
            "invalid_version",
            "invoked_name",
            "extended",
            "signal_disappeared",
            "kill_disappeared",
        ] {
            let root = tempfile::tempdir_in("/tmp")?;
            let binary = root.path().join("binary");
            fs::write(&binary, b"fixture")?;
            let agent = if case == "invoked_name" {
                let path = root.path().join("stable-name");
                std::os::unix::fs::symlink(&binary, &path)?;
                path
            } else {
                binary.clone()
            };
            let output = root.path().join("output");
            let state = Rc::new(RefCell::new(State::default()));
            let mut effects = Fake {
                case: case.into(),
                state: state.clone(),
                clock: 0,
                agent: agent.clone(),
            };
            let duration = if case == "extended" { 20 } else { 6 };
            let result = execute(
                Options {
                    fux: binary,
                    agent,
                    output: output.clone(),
                    duration,
                },
                &mut effects,
            );
            let successful = [
                "normal",
                "close_failure",
                "invoked_name",
                "extended",
                "signal_disappeared",
            ]
            .contains(&case);
            assert_eq!(result.is_ok(), successful, "{case}: {result:?}");
            assert_eq!(output.join("startup.json").exists(), successful, "{case}");
            let state = state.borrow();
            if case == "invalid_version" {
                assert_eq!(state.spawns, 0);
                assert!(state.calls.is_empty());
            } else {
                assert_eq!(state.calls.first().map(String::as_str), Some("terminate"));
                assert!(state.calls.contains(&"wait:10".into()));
            }
            if ["normal", "extended"].contains(&case) {
                let evidence: Value =
                    serde_json::from_slice(&fs::read(output.join("startup.json"))?)?;
                assert_eq!(evidence["observation_window_seconds"], duration);
                if case == "extended" {
                    assert!(
                        evidence["captures"]
                            .as_array()
                            .context("captures")?
                            .iter()
                            .map(|c| c["samples"].as_u64().unwrap_or(0))
                            .sum::<u64>()
                            > 3
                    );
                }
            }
            if ["timeout", "unreaped", "kill_disappeared"].contains(&case) {
                assert_eq!(&state.calls[state.calls.len() - 2..], ["kill", "wait:5"]);
                let diagnostic = fs::read_to_string(output.join("diagnostic.json"))?;
                assert!(diagnostic.contains("cleanup_error"));
                if case == "unreaped" {
                    assert!(!diagnostic.contains("server reaped"));
                }
            }
        }
        Ok(())
    }
}
