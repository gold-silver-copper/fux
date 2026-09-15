//! Command line: `zor serve --name NAME` runs the server in the foreground, `zor <method>
//! [json]` is a thin BRP client that reads `<runtime>/zor/<server>.brp.json`, adds the
//! envelope and prints the reply, `zor events --cursor N` streams `zor/events+watch`, and
//! `zor run -- CMD` names the milestone that implements it. The CLI holds no World.

use std::io::Write;
use std::path::PathBuf;
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
    /// Run a command in an ephemeral workspace with final evidence (milestone 6).
    Run {
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// `zor <method> [json]`: one BRP call with the envelope injected.
    #[command(external_subcommand)]
    Other(Vec<String>),
}

#[derive(Args, Debug)]
struct EventsArgs {
    #[arg(long)]
    cursor: Option<u64>,
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
        Cmd::Serve { name } => serve(&name),
        Cmd::Events(args) => events(&cli.server, args),
        Cmd::Run { .. } => Err(BevyError::from(
            "`zor run -- CMD` (ephemeral workspace, final evidence, exit status) is milestone 6: \
             provider adapters and the task lifecycle are not implemented yet",
        )),
        Cmd::Other(words) => other(&cli.server, words),
    }
}

fn other(server: &str, words: Vec<String>) -> Result<i32, BevyError> {
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
    let brp = descriptor_or_error(server)?;
    print_reply(client::call(&brp, &method, params)?)
}

fn is_method(word: &str) -> bool {
    word.contains('.') || word.contains('/')
}

/// Streams `zor/events+watch`, printing one JSON line per item; ends when the server closes
/// the stream.
fn events(server: &str, args: EventsArgs) -> Result<i32, BevyError> {
    let brp = descriptor_or_error(server)?;
    let descriptor = client::read_descriptor(&brp)?;
    let mut params = serde_json::Map::new();
    if let Some(cursor) = args.cursor {
        params.insert("cursor".into(), serde_json::json!(cursor));
    }
    let mut out = std::io::stdout().lock();
    client::stream(
        &descriptor,
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

fn descriptor_or_error(server: &str) -> Result<PathBuf, BevyError> {
    let paths = Paths::discover()?;
    let brp = paths.descriptor(server);
    if !brp.is_file() {
        return Err(BevyError::from(format!(
            "server {server} is not running ({} missing)",
            brp.display()
        )));
    }
    Ok(brp)
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
    let adapters: Vec<Box<dyn crate::runner::Adapter>> = vec![Box::new(
        crate::fux_client::FuxAdapter::new(fux_brp.clone(), sender.clone()),
    )];
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
