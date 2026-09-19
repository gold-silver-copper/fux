//! Command line: `zor serve --name NAME` runs the server in the foreground, `zor <method>
//! [json]` is a thin BRP client that reads `<runtime>/zor/<server>.brp.json`, adds the
//! envelope and prints the reply, `zor events --cursor N` streams `zor/events+watch`, `zor task
//! <verb>` wraps the `zor/task.*` methods, and `zor run -- CMD` runs one command in an
//! ephemeral fux workspace under a task and exits with the process's status once zor holds
//! its final evidence. The CLI holds no World.

mod operations;
use std::io::Write;
use std::process::ExitCode;

use bevy_app::AppExit;
use bevy_ecs::error::BevyError;
use clap::{Args, Parser, Subcommand};

use crate::config::Config;
use crate::paths::Paths;
use crate::remote::client;

pub const DEFAULT_SERVER: &str = "default";

#[derive(Parser, Debug)]
#[command(name = "zor", version, about = "Agent and task policy layer over fux")]
struct Cli {
    /// The zor server to talk to (its `brp.json` name).
    #[arg(long, default_value = DEFAULT_SERVER)]
    server: String,
    /// Select a saved machine; destructive task actions are journaled by the local controller.
    #[arg(long, global = true)]
    machine: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a server in the foreground.
    Serve {
        #[arg(long, default_value = DEFAULT_SERVER)]
        name: String,
    },
    /// Stream `zor/events+watch`, one JSON line per item.
    Events(EventsArgs),
    /// Read the selected controller's server, task and agent projections without starting an observer.
    Status,
    /// Run a command in an ephemeral fux workspace as a task; print the final screen and exit
    /// with the process's status (final evidence, never PTY text alone).
    Run(RunArgs),
    /// `zor/task.*` verbs over the BRP client.
    Task {
        #[command(subcommand)]
        verb: TaskCmd,
    },
    /// Manage saved direct endpoints.
    Machine {
        #[command(subcommand)]
        verb: operations::MachineCmd,
    },
    /// Manage plugin installation, execution and event hooks.
    Plugin {
        #[command(subcommand)]
        verb: operations::PluginCmd,
    },
    /// Open the Bevy scene dashboard hosted by fux.
    Dashboard(operations::DashboardArgs),
    /// Attach to the exact live pane of a task.
    Attach { task: String },
    /// `zor <method> [json]`: one BRP call with the envelope injected.
    #[command(external_subcommand)]
    Other(Vec<String>),
}

#[derive(Args, Debug)]
struct EventsArgs {
    #[arg(long)]
    cursor: Option<u64>,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// The fux workspace to create for this run (default: `zor-run-<random>`).
    #[arg(long)]
    workspace: Option<String>,
    /// Seconds to wait for the process; on expiry the pane is stopped and the exit code is 124.
    #[arg(long)]
    timeout: Option<u64>,
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum TaskCmd {
    Create {
        task: String,
        #[arg(long)]
        title: String,
        #[arg(long, conflicts_with = "worktree")]
        cwd: Option<String>,
        #[arg(long)]
        worktree: Option<String>,
    },
    List,
    Inspect {
        task: String,
    },
    Launch {
        task: String,
        #[arg(long)]
        operation: String,
        #[arg(long, default_value = "default")]
        workspace: String,
        #[arg(long)]
        ephemeral: bool,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Recreate an eligible native provider session without resending a prompt.
    Resume {
        task: String,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        fux_instance: String,
    },
    ResumeStatus { task: String },
    Adopt {
        task: String,
        #[arg(long)]
        instance: String,
        #[arg(long, default_value = "default")]
        workspace: String,
        #[arg(long)]
        pane: u64,
        #[arg(long)]
        pid: Option<u32>,
    },
    Prompt {
        task: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        text: String,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
        /// Prepare only; `zor task submit` is not separate, re-run without this flag.
        #[arg(long)]
        no_submit: bool,
    },
    Wait {
        prompt: String,
    },
    Stop {
        task: String,
    },
    Cancel {
        task: String,
    },
    Abandon {
        prompt: String,
    },
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "zor: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> Result<i32, BevyError> {
    match cli.command {
        Cmd::Serve { name } => {
            if cli.machine.is_some() {
                return Err("serve is local; --machine cannot start a remote service".into());
            }
            serve(&name)
        }
        Cmd::Plugin { verb: operations::PluginCmd::Hook { name } } => {
            crate::plugins::hooks::run(&name).map_err(BevyError::from)?;
            Ok(0)
        }
        Cmd::Plugin { verb: operations::PluginCmd::Supervise } => {
            crate::plugins::host::supervise().map_err(BevyError::from)
        }
        command => {
            let paths = Paths::discover()?;
            let local = operations::control_descriptor(&paths, &cli.server, None)?;
            if let (Some(machine), Cmd::Task { verb }) = (
                cli.machine.as_deref().filter(|name| !name.eq_ignore_ascii_case("local")),
                &command,
            ) {
                if matches!(verb, TaskCmd::Stop { .. } | TaskCmd::Cancel { .. } | TaskCmd::Resume { .. }) {
                    return operations::machine_task(&local, machine, verb);
                }
            }
            let selection = if matches!(&command, Cmd::Machine { .. } | Cmd::Dashboard(_)) {
                None
            } else {
                cli.machine.as_deref()
            };
            let descriptor = operations::selected_control(&local, selection)?;
            match command {
                Cmd::Events(args) => events(&descriptor, args),
                Cmd::Status => status(&descriptor),
                Cmd::Run(args) => run(&descriptor, args),
                Cmd::Task { verb } => task(&descriptor, verb),
                Cmd::Other(words) => other(&descriptor, words),
                Cmd::Machine { verb } => operations::machine(&descriptor, verb),
                Cmd::Plugin { verb } => operations::plugin(&descriptor, verb),
                Cmd::Dashboard(args) => operations::dashboard(&paths, &descriptor, cli.machine.as_deref(), args),
                Cmd::Attach { task } => operations::attach(&paths, &local, &descriptor, cli.machine.as_deref(), &task, None),
                Cmd::Serve { .. } => Err("serve dispatch requires local execution".into()),
            }
        }
    }
}

fn status(descriptor: &client::Descriptor) -> Result<i32, BevyError> {
    let server: crate::remote::methods::ServerInfo = serde_json::from_value(
        client::call_with(descriptor, "zor/server.info", serde_json::json!({}))?,
    )?;
    if server.nonce != descriptor.instance {
        return Err("selected controller incarnation changed".into());
    }
    let tasks: crate::remote::task_methods::TaskList = serde_json::from_value(
        client::call_with(descriptor, "zor/task.list", serde_json::json!({}))?,
    )?;
    let agents: crate::remote::provider_methods::AgentList = serde_json::from_value(
        client::call_with(descriptor, "zor/agent.list", serde_json::json!({}))?,
    )?;
    // Reads do not require an incarnation nonce. Refuse a snapshot if the endpoint restarted
    // between projections instead of combining rows from different controller instances.
    let current: crate::remote::methods::ServerInfo = serde_json::from_value(
        client::call_with(descriptor, "zor/server.info", serde_json::json!({}))?,
    )?;
    if current.nonce != descriptor.instance {
        return Err("selected controller incarnation changed".into());
    }
    print_reply(serde_json::json!({"server":server, "tasks":tasks.tasks, "agents":agents.agents}))
}

fn other(descriptor: &client::Descriptor, words: Vec<String>) -> Result<i32, BevyError> {
    let mut words = words.into_iter();
    let Some(method) = words.next() else {
        return Err(BevyError::from("expected `zor <method> [json]`"));
    };
    if !is_method(&method) {
        return Err(BevyError::from(format!(
            "unknown command {method:?}: expected `zor serve`, `zor events`, `zor run -- CMD` \
             or `zor <method> [json]`"
        )));
    }
    let params = parse_params(words.next())?;
    if let Some(extra) = words.next() {
        return Err(BevyError::from(format!("unexpected argument {extra:?}")));
    }
    print_reply(client::call_with(descriptor, &method, params)?)
}

fn is_method(word: &str) -> bool {
    word.contains('.') || word.contains('/')
}

fn task(descriptor: &client::Descriptor, verb: TaskCmd) -> Result<i32, BevyError> {
    let (method, params) = match verb {
        TaskCmd::Create {
            task,
            title,
            cwd,
            worktree,
        } => (
            "zor/task.create",
            serde_json::json!({ "task": task, "title": title, "cwd": cwd, "worktree": worktree }),
        ),
        TaskCmd::List => ("zor/task.list", serde_json::json!({})),
        TaskCmd::Inspect { task } => ("zor/task.inspect", serde_json::json!({ "task": task })),
        TaskCmd::Launch {
            task,
            operation,
            workspace,
            ephemeral,
            command,
        } => (
            "zor/task.launch",
            serde_json::json!({
                "task": task, "operation": operation, "argv": command,
                "workspace": workspace, "ephemeral": ephemeral,
            }),
        ),
        TaskCmd::Resume { task, operation, fux_instance } => (
            "zor/task.resume",
            serde_json::json!({"task":task,"operation":operation,"fux_instance":fux_instance}),
        ),
        TaskCmd::ResumeStatus { task } => ("zor/task.resume-status", serde_json::json!({"task":task})),
        TaskCmd::Adopt {
            task,
            instance,
            workspace,
            pane,
            pid,
        } => (
            "zor/task.adopt",
            serde_json::json!({
                "task": task, "fux_instance": instance, "workspace": workspace, "pane": pane, "pid": pid,
            }),
        ),
        TaskCmd::Prompt {
            task,
            prompt,
            text,
            timeout_ms,
            no_submit,
        } => (
            "zor/task.prompt",
            serde_json::json!({
                "task": task, "prompt": prompt, "text": text, "timeout_ms": timeout_ms,
                "submit": !no_submit,
            }),
        ),
        TaskCmd::Wait { prompt } => ("zor/task.wait", serde_json::json!({ "prompt": prompt })),
        TaskCmd::Stop { task } => ("zor/task.stop", serde_json::json!({ "task": task })),
        TaskCmd::Cancel { task } => ("zor/task.cancel", serde_json::json!({ "task": task })),
        TaskCmd::Abandon { prompt } => {
            ("zor/task.abandon", serde_json::json!({ "prompt": prompt }))
        }
    };
    print_reply(client::call_with(descriptor, method, params)?)
}

/// Poll period of `zor run`.
const RUN_POLL: std::time::Duration = std::time::Duration::from_millis(100);
/// After the timeout's stop request, how long final evidence is awaited before giving up.
const RUN_STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
/// The conventional exit status of a timed-out command.
const RUN_TIMED_OUT: i32 = 124;

/// One command under a task in a fresh workspace: creation and launch are journaled intents
/// on the server; the CLI only polls `zor/attempt.inspect` until the attempt is `Finished`
/// with final evidence, prints its final screen and exits with the recorded status.
fn run(descriptor: &client::Descriptor, args: RunArgs) -> Result<i32, BevyError> {
    let mut suffix = fux::attach::random_hex256()?;
    suffix.truncate(12);
    let task = format!("run-{suffix}");
    let workspace = args
        .workspace
        .unwrap_or_else(|| format!("zor-run-{suffix}"));
    let cwd = std::env::current_dir()?.display().to_string();
    client::call_with(
        descriptor,
        "zor/task.create",
        serde_json::json!({ "task": task, "title": args.command.join(" "), "cwd": cwd }),
    )?;
    let attempt = client::call_with(
        descriptor,
        "zor/task.launch",
        serde_json::json!({
            "task": task, "operation": format!("launch-{suffix}"), "argv": args.command,
            "workspace": workspace, "ephemeral": true,
        }),
    )?;
    let attempt = attempt
        .get("attempt")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| BevyError::from("launch reply without an attempt id"))?;
    let started = std::time::Instant::now();
    let timeout = args.timeout.map(std::time::Duration::from_secs);
    let mut stopped_at: Option<std::time::Instant> = None;
    loop {
        let record = client::call_with(
            descriptor,
            "zor/attempt.inspect",
            serde_json::json!({ "attempt": attempt }),
        )?;
        if record.get("state").and_then(serde_json::Value::as_str) == Some("finished") {
            let evidence = record.get("final");
            let mut out = std::io::stdout().lock();
            let output = evidence
                .and_then(|e| e.get("output"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if !output.is_empty() {
                writeln!(out, "{output}")?;
            }
            if stopped_at.is_some() {
                return Ok(RUN_TIMED_OUT);
            }
            return match evidence
                .and_then(|e| e.get("exit_code"))
                .and_then(serde_json::Value::as_i64)
            {
                Some(code) => Ok(i32::try_from(code).unwrap_or(1)),
                None => Err(BevyError::from(
                    "the pane closed without an observed exit status",
                )),
            };
        }
        if record.get("lost").and_then(serde_json::Value::as_bool) == Some(true) {
            return Err(BevyError::from(format!(
                "fux was replaced; attempt {attempt} is lost: {}",
                record
                    .get("problem")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            )));
        }
        match stopped_at {
            Some(at) if at.elapsed() > RUN_STOP_GRACE => {
                return Err(BevyError::from(format!(
                    "timed out and no final evidence arrived after the stop request; attempt {attempt} is {}",
                    record
                        .get("state")
                        .map_or_else(|| "null".to_owned(), |state| state.to_string())
                )));
            }
            None if timeout.is_some_and(|t| started.elapsed() > t) => {
                client::call_with(
                    descriptor,
                    "zor/task.stop",
                    serde_json::json!({ "task": task }),
                )?;
                stopped_at = Some(std::time::Instant::now());
            }
            _ => {}
        }
        std::thread::sleep(RUN_POLL);
    }
}

/// Streams `zor/events+watch`, printing one JSON line per item; ends when the server closes
/// the stream.
fn events(descriptor: &client::Descriptor, args: EventsArgs) -> Result<i32, BevyError> {
    let mut params = serde_json::Map::new();
    if let Some(cursor) = args.cursor {
        params.insert("cursor".into(), serde_json::json!(cursor));
    }
    let mut out = std::io::stdout().lock();
    client::stream(
        descriptor,
        crate::remote::watch::EVENTS_WATCH_METHOD,
        serde_json::Value::Object(params),
        |item| writeln!(out, "{item}").is_ok(),
    )?;
    Ok(0)
}

fn parse_params(json: Option<String>) -> Result<serde_json::Value, BevyError> {
    match json {
        None => Ok(serde_json::json!({})),
        Some(text) => Ok(serde_json::from_str(&text)?),
    }
}

fn print_reply(reply: serde_json::Value) -> Result<i32, BevyError> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", serde_json::to_string_pretty(&reply)?)?;
    Ok(0)
}


fn serve(name: &str) -> Result<i32, BevyError> {
    let paths = Paths::discover()?;
    paths.prepare()?;
    let config = Config::load(&paths.config_file())?;
    let (sender, inbound) = async_channel::bounded(crate::runner::INBOUND_QUEUE);
    let (control_sender, control) = async_channel::bounded(crate::runner::CONTROL_QUEUE);
    let mut app = crate::app::build(&config, &paths, name, sender.clone());
    crate::runner::install_signals(control_sender)?;
    let fux_brp = paths.fux_descriptor(&config.fux_server);
    let adapters: Vec<Box<dyn crate::runner::Adapter>> = vec![
        Box::new(crate::fux_client::FuxAdapter::new(
            fux_brp.clone(),
            sender.clone(),
        )),
        Box::new(crate::git::GitAdapter::new(sender.clone())),
        Box::new(crate::providers::ProviderAdapter::new(sender.clone())),
        Box::new(crate::checks::runner::CheckRunner::new(sender.clone())),
        Box::new(crate::plugins::host::PluginAdapter::new(sender.clone())),
    ];
    // The consumer needs the task pools, which `TaskPoolPlugin` created during `build`.
    crate::fux_client::spawn_events_consumer(fux_brp, 0, sender);
    crate::runner::install(
        &mut app,
        fux::runner::Sources { control, inbound },
        adapters,
    );
    Ok(match app.run() {
        AppExit::Success => 0,
        AppExit::Error(code) => i32::from(code.get()),
    })
}
