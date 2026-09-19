//! CLI orchestration only: descriptors remain private, and every mutation stays on its owner.
mod dashboard_cli;
pub use dashboard_cli::run as dashboard;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use bevy_ecs::error::BevyError;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::machines::{MachineGuard, supervision::{ActionPhase, Freshness, PaneIdentity}, transport::Transport};
use crate::paths::Paths;
use crate::remote::{client, descriptor::{self, Descriptor, DescriptorGuard}};

#[derive(Subcommand, Debug)]
pub enum MachineCmd {
    List,
    Inspect { machine: String },
    Add {
        name: String,
        #[arg(long)] control_brp: Option<PathBuf>,
        /// Bind a workspace to a private fux descriptor (WORKSPACE=PATH).
        #[arg(long = "attach")] attachments: Vec<String>,
    },
    Rename { machine: String, name: String },
    Remove { machine: String },
    Control { machine: String, #[arg(long)] brp: Option<PathBuf> },
    Bind { machine: String, workspace: String, #[arg(long)] brp: Option<PathBuf> },
    Reload,
    Status { #[arg(long)] operation: Option<String> },
}

#[derive(Subcommand, Debug)]
pub enum PluginCmd {
    List,
    Inspect { name: String },
    Install { path: PathBuf, #[arg(long)] enabled: bool },
    Link { path: PathBuf, #[arg(long)] enabled: bool },
    Enable { name: String },
    Disable { name: String },
    Uninstall { name: String },
    Run { name: String, action: String, #[arg(long)] workspace: Option<String>, #[arg(long)] task: Option<String> },
    Pane { name: String, pane: String, workspace: String, #[arg(long)] target: Option<u64>, #[arg(long)] task: Option<String> },
    OpenLink { url: String },
    Logs { name: String, #[arg(long, default_value_t = 100)] limit: usize },
    Bindings,
    #[command(hide = true)] Hook { name: String },
    #[command(hide = true)] Supervise,
}

#[derive(Args, Debug)]
pub struct DashboardArgs {
    /// Print the current rows without opening a terminal viewer.
    #[arg(long)] once: bool,
    /// Existing workspace and empty template leaf; omit both to create a dedicated workspace.
    #[arg(long, requires = "node")] workspace: Option<String>,
    #[arg(long, requires = "workspace")] node: Option<u64>,
    #[arg(long, requires = "node")] generation: Option<u64>,
}

pub fn control_descriptor(paths: &Paths, server: &str, machine: Option<&str>) -> Result<Descriptor, BevyError> {
    let local = client::read_descriptor(&descriptor::descriptor_path(&paths.runtime_dir, server))?;
    selected_control(&local, machine)
}

pub(super) fn selected_control(local: &Descriptor, machine: Option<&str>) -> Result<Descriptor, BevyError> {
    match machine.filter(|name| !name.eq_ignore_ascii_case("local")) {
        None => Ok(local.clone()),
        Some(machine) => {
            let endpoint: crate::remote::machine_methods::MachineEndpoint = serde_json::from_value(
                client::call_with(local, "zor/machine.endpoint", json!({"machine": machine}))?,
            )?;
            Ok(endpoint.descriptor)
        }
    }
}

fn direct(path: PathBuf) -> Result<Transport, BevyError> {
    let brp = client::read_descriptor(&path)?;
    let transport = Transport::Direct { host: brp.http.host.clone(), port: brp.http.port, brp };
    transport.validate()?;
    Ok(transport)
}

pub fn machine(control: &Descriptor, verb: MachineCmd) -> Result<i32, BevyError> {
    let (method, params) = match verb {
        MachineCmd::List => ("zor/machine.list", json!({})),
        MachineCmd::Inspect { machine } => ("zor/machine.inspect", json!({"machine":machine})),
        MachineCmd::Add { name, control_brp, attachments } => {
            let control = control_brp.map(direct).transpose()?;
            let mut bindings = BTreeMap::new();
            for binding in attachments {
                let (workspace, path) = binding.split_once('=').ok_or("attachment requires WORKSPACE=PATH")?;
                if bindings.insert(workspace.to_string(), direct(PathBuf::from(path))?).is_some() {
                    return Err("duplicate attachment workspace".into());
                }
            }
            ("zor/machine.add", json!({"name":name,"control":control,"attachments":bindings}))
        }
        MachineCmd::Rename { machine, name } => ("zor/machine.rename", json!({"machine":machine,"name":name})),
        MachineCmd::Remove { machine } => ("zor/machine.remove", json!({"machine":machine})),
        MachineCmd::Control { machine, brp } => ("zor/machine.control", json!({"machine":machine,"control":brp.map(direct).transpose()?})),
        MachineCmd::Bind { machine, workspace, brp } => ("zor/machine.bind", json!({"machine":machine,"workspace":workspace,"attachment":brp.map(direct).transpose()?})),
        MachineCmd::Reload => ("zor/machine.reload", json!({})),
        MachineCmd::Status { operation } => ("zor/machine.status", json!({"operation":operation})),
    };
    super::print_reply(client::call_with(control, method, params)?)
}

/// Submit once through the durable local controller, never directly to the selected endpoint.
/// Exit 2 means accepted/pending, 3 means uncertain, and 1 means refused/failed. Even a
/// successful acknowledgement describes the request, not completion of the task lifecycle.
pub fn machine_task(control: &Descriptor, machine: &str, verb: &super::TaskCmd) -> Result<i32, BevyError> {
    let (method, task, operation, fux_instance) = match verb {
        super::TaskCmd::Stop { task } => ("zor/machine.stop", task, fux::attach::random_hex256()?, None),
        super::TaskCmd::Cancel { task } => ("zor/machine.cancel", task, fux::attach::random_hex256()?, None),
        super::TaskCmd::Resume { task, operation, fux_instance } => (
            "zor/machine.resume", task, operation.clone(), Some(fux_instance),
        ),
        _ => return Err("task verb does not use a machine action".into()),
    };
    let inspected: crate::remote::machine_methods::MachineInspect = serde_json::from_value(
        client::call_with(control, "zor/machine.inspect", json!({"machine":machine}))?,
    )?;
    let snapshot = inspected.machine;
    if snapshot.freshness != Freshness::Fresh {
        return Err(format!("machine observation is {}; no action submitted", snapshot.freshness.name()).into());
    }
    let view = snapshot.view.ok_or("machine has no observation; no action submitted")?;
    let mut tasks = view.rows.iter().filter(|row| row.id == *task);
    let row = tasks.next().ok_or("task is not observed on this machine; no action submitted")?;
    if tasks.next().is_some() {
        return Err("task observation is ambiguous; no action submitted".into());
    }
    let pane = if let Some(attempt) = row.current_attempt {
        let mut agents = view.agents.iter().filter(|agent| agent.task.as_deref() == Some(task.as_str()) && agent.attempt == Some(attempt));
        let agent = agents.next().ok_or("current attempt has no observed pane; no action submitted")?;
        if agents.next().is_some() {
            return Err("current attempt has ambiguous pane identity; no action submitted".into());
        }
        Some(PaneIdentity {
            instance:agent.instance.clone(), workspace:agent.workspace.clone(),
            pane:agent.pane, pid:agent.pid,
        })
    } else {
        None
    };
    let guard = MachineGuard { instance:view.instance, attempt:row.current_attempt, pane };
    let mut params = json!({"machine":snapshot.id,"task":task,"operation":operation,"guard":guard});
    if let Some(fux_instance) = fux_instance {
        params["fux_instance"] = json!(fux_instance);
    }
    // A lost or malformed acknowledgement cannot authorize another submission. Preserve
    // the operation identity even on this path so the caller can inspect the durable intent.
    let result = client::call_with(control, method, params)
        .map_err(|error| error.to_string())
        .and_then(|reply| serde_json::from_value::<crate::remote::machine_methods::MachineActionResult>(reply).map_err(|error| error.to_string()));
    let action = match result {
        Ok(result) => result.action,
        Err(problem) => {
            super::print_reply(json!({
                "operation":operation, "status":"uncertain", "problem":problem,
                "guidance":"Acceptance is unconfirmed. Inspect `zor machine status --operation ID`; do not resubmit.",
            }))?;
            return Ok(3);
        }
    };
    let (status, code) = match action.phase {
        ActionPhase::Submitting => ("pending", 2),
        ActionPhase::Done => ("acknowledged", 0),
        ActionPhase::Failed => ("failed", 1),
        ActionPhase::Refused => ("refused", 1),
        ActionPhase::Uncertain => ("uncertain", 3),
    };
    super::print_reply(json!({
        "operation":operation, "status":status, "action":action,
        "guidance":"Inspect `zor machine status --operation ID`; an acknowledgement is not task completion. Do not replay uncertain actions.",
    }))?;
    Ok(code)
}

pub fn plugin(control: &Descriptor, verb: PluginCmd) -> Result<i32, BevyError> {
    let asynchronous = matches!(&verb, PluginCmd::Install { .. } | PluginCmd::Link { .. });
    let (method, params) = match verb {
        PluginCmd::List => ("zor/plugin.list", json!({})),
        PluginCmd::Inspect { name } => ("zor/plugin.inspect", json!({"name":name})),
        PluginCmd::Install { path, enabled } => ("zor/plugin.install", json!({"path":path.canonicalize()?.display().to_string(),"enabled":enabled})),
        PluginCmd::Link { path, enabled } => ("zor/plugin.link", json!({"path":path.canonicalize()?.display().to_string(),"enabled":enabled})),
        PluginCmd::Enable { name } => ("zor/plugin.enable", json!({"name":name})),
        PluginCmd::Disable { name } => ("zor/plugin.disable", json!({"name":name})),
        PluginCmd::Uninstall { name } => ("zor/plugin.uninstall", json!({"name":name})),
        PluginCmd::Run { name, action, workspace, task } => ("zor/plugin.run", json!({"name":name,"action":action,"workspace":workspace,"task":task})),
        PluginCmd::Pane { name, pane, workspace, target, task } => ("zor/plugin.pane", json!({"name":name,"pane":pane,"workspace":workspace,"target":target,"task":task})),
        PluginCmd::OpenLink { url } => ("zor/plugin.open_link", json!({"url":url})),
        PluginCmd::Logs { name, limit } => ("zor/plugin.logs", json!({"name":name,"limit":limit})),
        PluginCmd::Bindings => ("zor/plugin.bindings", json!({})),
        PluginCmd::Hook { .. } | PluginCmd::Supervise => return Err("plugin helpers do not use controller CLI dispatch".into()),
    };
    let mut result = client::call_with(control, method, params)?;
    if asynchronous {
        let operation = result.get("operation").and_then(Value::as_u64).ok_or("plugin install omitted operation")?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            result = client::call_with(control, "zor/plugin.operation", json!({"operation":operation}))?;
            match result.get("state").and_then(Value::as_str) {
                Some("complete") => break,
                Some("failed") => return Err(result.get("problem").and_then(Value::as_str).unwrap_or("plugin installation failed").to_string().into()),
                Some("pending") if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Some("pending") => return Err(format!("plugin operation {operation} is still pending; inspect it rather than resubmit").into()),
                _ => return Err("invalid plugin operation response".into()),
            }
        }
    }
    super::print_reply(result)
}

fn attachment_descriptor(paths: &Paths, local: &Descriptor, machine: Option<&str>, pane: &PaneIdentity) -> Result<Descriptor, BevyError> {
    let descriptor = match machine.filter(|name| !name.eq_ignore_ascii_case("local")) {
        Some(machine) => {
            let endpoint: crate::remote::machine_methods::MachineEndpoint = serde_json::from_value(
                client::call_with(local, "zor/machine.endpoint", json!({"machine":machine, "workspace":pane.workspace}))?,
            )?;
            endpoint.descriptor
        }
        None => {
            let config = crate::config::Config::load(&paths.config_file())?;
            client::read_descriptor(&paths.fux_descriptor(&config.fux_server))?
        }
    };
    if descriptor.instance != pane.instance || descriptor.attach.is_none() {
        return Err("attachment binding does not identify the task's fux incarnation".into());
    }
    Ok(descriptor)
}

fn temporary_descriptor(paths: &Paths, descriptor: &Descriptor) -> Result<DescriptorGuard, BevyError> {
    paths.prepare()?;
    let path = paths.runtime_dir.join(format!("viewer-{}.brp.json", fux::attach::random_hex256()?));
    descriptor::write_descriptor(&path, descriptor)?;
    Ok(DescriptorGuard(path))
}

fn viewer(brp: &Path, zor_brp: &Path, workspace: &str, pane: Option<&PaneIdentity>) -> Result<Child, BevyError> {
    let executable = std::env::current_exe()?.with_file_name("fux");
    let mut command = Command::new(executable);
    command.arg("attach").arg("--brp").arg(brp).arg("--workspace").arg(workspace);
    command.env("ZOR_BRP", zor_brp);
    if let Some(pane) = pane {
        command.arg("--pane").arg(pane.pane.to_string());
        if let Some(pid) = pane.pid { command.arg("--pid").arg(pid.to_string()); }
    }
    Ok(command.spawn()?)
}

/// A fresh task observation authorizes a single exact attachment, never a workspace fallback.
pub fn attach(paths: &Paths, local: &Descriptor, control: &Descriptor, machine: Option<&str>, task: &str, expected: Option<&crate::dashboard::Target>) -> Result<i32, BevyError> {
    if expected.is_some_and(|target| target.instance != control.instance || target.task != task) {
        return Err("dashboard handoff names another controller incarnation or task".into());
    }
    let record: crate::remote::task_methods::TaskInspect = serde_json::from_value(
        client::call_with(control, "zor/task.inspect", json!({"task":task}))?,
    )?;
    let mut live = record.attempts.iter().filter(|attempt| attempt.state != crate::model::AttemptState::Finished);
    let attempt = live.next().ok_or("task has no live attempt")?;
    if live.next().is_some() { return Err("task has multiple live attempts".into()); }
    if attempt.state != crate::model::AttemptState::Live || attempt.pane == 0 || attempt.pid.is_none() {
        return Err("attempt has no authenticated live pane process".into());
    }
    let id = attempt.attempt;
    let pane = PaneIdentity {
        instance:attempt.instance.clone(), workspace:attempt.workspace.clone(),
        pane:attempt.pane, pid:attempt.pid,
    };
    if expected.is_some_and(|target| target.attempt != Some(id) || target.pane.as_ref() != Some(&pane)) {
        return Err("task changed after dashboard selection; select it again".into());
    }
    let descriptor = attachment_descriptor(paths, local, machine, &pane)?;
    let brp = temporary_descriptor(paths, &descriptor)?;
    let zor_brp = temporary_descriptor(paths, control)?;
    dashboard_cli::exact_viewer(&brp.0, &zor_brp.0, &pane)
}
