//! Command line (prompt 3.12): `fux` attaches to (or starts) the `default` server, `fux NAME`
//! attaches to workspace NAME, `fux serve --name` runs a server in the foreground,
//! `fux attach --brp FILE`, `fux workspace list|new|kill`, `fux [WORKSPACE] layout
//! save|load NAME | list`, and `fux [SERVER] <method> [json]` is a thin BRP client. The CLI
//! holds no World.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use bevy_app::{AppExit, Startup};
use bevy_ecs::prelude::*;
use clap::{Args, Parser, Subcommand};

use crate::config::Config;
use crate::model::PaneId;
use crate::paths::Paths;
use crate::remote::client;
use crate::viewer::{self, ViewerOptions};
use crate::wire::ExactTargetSpec;

pub const DEFAULT_SERVER: &str = "default";
pub const DEFAULT_WORKSPACE: &str = "default";
/// How long `fux` waits for a freshly started server to publish its descriptor.
pub const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(name = "fux", version, about = "Persistent terminal multiplexer")]
struct Cli {
    /// Server whose descriptor is used (`<runtime>/fux/<server>.brp.json`).
    #[arg(long, global = true, default_value = DEFAULT_SERVER)]
    server: String,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a server in the foreground.
    Serve {
        #[arg(long, default_value = DEFAULT_SERVER)]
        name: String,
        /// Detach from the controlling terminal (new session); used by `fux` when it starts
        /// the default server.
        #[arg(long, hide = true)]
        detached: bool,
    },
    /// Attach a viewer to a server named by its descriptor file.
    Attach(AttachArgs),
    /// Manage workspaces.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCmd,
    },
    /// Save, load or list named layouts of the `default` workspace (`fux WORKSPACE layout …`
    /// for another).
    Layout {
        #[command(subcommand)]
        command: LayoutCmd,
    },
    /// `fux NAME` attaches to workspace NAME; `fux [SERVER] <method> [json]` calls BRP.
    #[command(external_subcommand)]
    Other(Vec<String>),
}

#[derive(Args, Debug)]
struct AttachArgs {
    #[arg(long)]
    brp: PathBuf,
    #[arg(long, default_value = DEFAULT_WORKSPACE)]
    workspace: String,
    /// Exact attachment: target this pane only; detach when it goes away.
    #[arg(long)]
    pane: Option<u64>,
    /// Verify the exact pane's process id before attaching.
    #[arg(long, requires = "pane")]
    pid: Option<u32>,
}

#[derive(Subcommand, Debug)]
enum WorkspaceCmd {
    List,
    New { name: String },
    Kill { name: String },
}

#[derive(Subcommand, Debug, Clone)]
enum LayoutCmd {
    /// Names of the saved layouts and the built-in templates.
    List,
    /// Export the workspace's layout to `<config>/layouts/NAME.scn.ron`.
    Save { name: String },
    /// Apply the saved layout NAME (or the built-in of that name); existing panes fill its
    /// leaves in order.
    Load {
        name: String,
        /// Close the panes the layout has no leaf for instead of refusing.
        #[arg(long)]
        close_unplaced: bool,
    },
}

/// `fux WORKSPACE layout …`: the words after WORKSPACE parsed as the `layout` subcommand.
#[derive(Parser, Debug)]
#[command(name = "layout", no_binary_name = true)]
struct LayoutWords {
    #[command(subcommand)]
    command: LayoutCmd,
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "fux: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> Result<i32, BevyError> {
    match cli.command {
        Some(Cmd::Serve { name, detached }) => serve(&name, detached),
        Some(Cmd::Attach(args)) => {
            let exact = args.pane.map(|pane| ExactTargetSpec {
                pane: PaneId(pane),
                pid: args.pid,
            });
            viewer::run(ViewerOptions {
                brp_path: args.brp,
                workspace: args.workspace,
                exact,
            })
        }
        Some(Cmd::Workspace { command }) => {
            let brp = descriptor_or_error(&cli.server)?;
            let (method, params) = match command {
                WorkspaceCmd::List => ("fux/workspace.list", serde_json::json!({})),
                WorkspaceCmd::New { name } => {
                    ("fux/workspace.new", serde_json::json!({ "name": name }))
                }
                WorkspaceCmd::Kill { name } => {
                    ("fux/workspace.kill", serde_json::json!({ "name": name }))
                }
            };
            print_reply(client::call(&brp, method, params)?)
        }
        Some(Cmd::Layout { command }) => layout(&cli.server, DEFAULT_WORKSPACE, command),
        Some(Cmd::Other(words)) => other(&cli.server, words),
        None => attach_workspace(&cli.server, DEFAULT_WORKSPACE),
    }
}

fn other(server: &str, words: Vec<String>) -> Result<i32, BevyError> {
    let mut words = words.into_iter();
    let Some(first) = words.next() else {
        return attach_workspace(server, DEFAULT_WORKSPACE);
    };
    if is_method(&first) {
        let params = parse_params(words.next())?;
        let brp = descriptor_or_error(server)?;
        return print_reply(client::call(&brp, &first, params)?);
    }
    match words.next() {
        Some(word) if word == "layout" => {
            let parsed = LayoutWords::try_parse_from(words)?;
            layout(server, &first, parsed.command)
        }
        Some(method) if is_method(&method) => {
            let params = parse_params(words.next())?;
            let brp = descriptor_or_error(&first)?;
            print_reply(client::call(&brp, &method, params)?)
        }
        Some(extra) => Err(BevyError::from(format!(
            "unexpected argument {extra:?}: expected `fux NAME` or `fux [SERVER] <method> [json]`"
        ))),
        None => attach_workspace(server, &first),
    }
}

fn is_method(word: &str) -> bool {
    word.contains('.') || word.contains('/')
}

fn layout(server: &str, workspace: &str, command: LayoutCmd) -> Result<i32, BevyError> {
    let brp = descriptor_or_error(server)?;
    let (method, params) = match command {
        LayoutCmd::List => ("fux/scene.list", serde_json::json!({})),
        LayoutCmd::Save { name } => (
            "fux/scene.save",
            serde_json::json!({ "workspace": workspace, "name": name }),
        ),
        LayoutCmd::Load {
            name,
            close_unplaced,
        } => (
            "fux/scene.restore",
            serde_json::json!({
                "workspace": workspace,
                "name": name,
                "adopt": true,
                "close_unplaced": close_unplaced,
            }),
        ),
    };
    print_reply(client::call(&brp, method, params)?)
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

/// Attaches to `workspace` on `server`, starting the server and creating the workspace when
/// they do not exist.
fn attach_workspace(server: &str, workspace: &str) -> Result<i32, BevyError> {
    let paths = Paths::discover()?;
    paths.prepare()?;
    let brp = ensure_server(&paths, server)?;
    ensure_workspace(&brp, workspace)?;
    viewer::run(ViewerOptions {
        brp_path: brp,
        workspace: workspace.into(),
        exact: None,
    })
}

fn server_alive(brp: &Path) -> bool {
    brp.is_file() && client::call(brp, "fux/server.info", serde_json::json!({})).is_ok()
}

fn ensure_server(paths: &Paths, server: &str) -> Result<PathBuf, BevyError> {
    let brp = paths.descriptor(server);
    if server_alive(&brp) {
        return Ok(brp);
    }
    if brp.exists() {
        std::fs::remove_file(&brp)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file(server))?;
    let exe = std::env::current_exe()?;
    // No `process_group(0)`: the child calls `setsid`, which fails for a process-group leader
    // and gives the new session its own group anyway.
    Command::new(exe)
        .args(["serve", "--name", server, "--detached"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    let started = Instant::now();
    while started.elapsed() < START_TIMEOUT {
        if server_alive(&brp) {
            return Ok(brp);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(BevyError::from(format!(
        "server {server} did not publish {} within {START_TIMEOUT:?}; see {}",
        brp.display(),
        paths.log_file(server).display()
    )))
}

fn ensure_workspace(brp: &Path, workspace: &str) -> Result<(), BevyError> {
    let list = client::call(brp, "fux/workspace.list", serde_json::json!({}))?;
    let exists = list
        .get("workspaces")
        .and_then(|w| w.as_array())
        .is_some_and(|ws| {
            ws.iter()
                .any(|w| w.get("name").and_then(|n| n.as_str()) == Some(workspace))
        });
    if !exists {
        client::call(
            brp,
            "fux/workspace.new",
            serde_json::json!({ "name": workspace }),
        )?;
    }
    Ok(())
}

fn serve(name: &str, detached: bool) -> Result<i32, BevyError> {
    if detached {
        // The parent already redirected stdio; only the session needs to be ours.
        nix::unistd::setsid()?;
    }
    let paths = Paths::discover()?;
    paths.prepare()?;
    let config = Config::load(&paths.config_file())?;
    let (sender, inbound) = async_channel::bounded(8192);
    let (control_sender, control) = async_channel::bounded(crate::runner::CONTROL_QUEUE);
    let mut app = crate::app::build(&config, &paths, name, sender.clone());
    app.add_systems(Startup, |world: &mut World| -> Result<(), BevyError> {
        crate::lifecycle::bootstrap(world, DEFAULT_WORKSPACE, &[])
    });
    crate::runner::signals::install(control_sender)?;
    let pty = crate::pty::PtyAdapter::new(sender);
    let attach = crate::attach::AttachAdapter::from_app(&app)?;
    crate::runner::install(
        &mut app,
        crate::runner::Sources { control, inbound },
        pty,
        attach,
    );
    Ok(match app.run() {
        AppExit::Success => 0,
        AppExit::Error(code) => i32::from(code.get()),
    })
}
