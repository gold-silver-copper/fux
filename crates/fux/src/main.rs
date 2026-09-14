#![forbid(unsafe_code)]
// This binary is the CLI output surface: it prints command results as JSON to stdout and
// diagnostics to stderr. The library modules stay strict; only this entrypoint may print.
#![allow(clippy::print_stdout, clippy::print_stderr)]
mod layout_cli;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use fux::daemon::{ManagerReply, ManagerRequest};
use fux::ids::{PaneId, TabId};
use fux::proto::control::{FocusTarget, Request, TabAction, TabTarget};

/// A minimal persistent terminal multiplexer: workspaces group tabs, tabs switch layouts, splits
/// show terminals together. `fux` attaches to the default workspace, starting a session server on
/// demand; `fux NAME` attaches to (or creates) a named workspace.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Workspace name for attaching and for the control commands below.
    name: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Args)]
struct AttachTargetArgs {
    #[arg(long, requires_all = ["target_workspace", "target_stream", "target_pane", "target_pid"])]
    target_instance: Option<String>,
    #[arg(long, requires = "target_instance")]
    target_workspace: Option<String>,
    #[arg(long, requires = "target_instance")]
    target_stream: Option<u64>,
    #[arg(long, requires = "target_instance")]
    target_pane: Option<u32>,
    #[arg(long, requires = "target_instance")]
    target_pid: Option<u32>,
}
impl AttachTargetArgs {
    fn value(self) -> Result<Option<fux::proto::attach::InitialTarget>> {
        let Some(instance) = self.target_instance else {
            return Ok(None);
        };
        Ok(Some(fux::proto::attach::InitialTarget {
            instance,
            workspace: self.target_workspace.context("target workspace required")?,
            stream: self.target_stream.context("target stream required")?,
            pane: PaneId(self.target_pane.context("target pane required")?),
            pid: self.target_pid.context("target PID required")?,
        }))
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Locate a live pane by server identity, independently of its current workspace.
    LocatePane(PaneLocationArgs),
    /// Run the session server in the foreground.
    Serve(ServeArgs),
    /// Attach to an explicit attachment socket (for example a koh gateway proxy socket).
    Attach {
        #[arg(long)]
        socket: PathBuf,
        /// Emit a bounded JSON exit report on stderr for a supervising controller.
        #[arg(long)]
        report_exit: bool,
        #[command(flatten)]
        target: AttachTargetArgs,
    },
    /// Show the configured prefix and keybindings.
    Bindings,
    /// Manage workspaces through the session server (lists them when no subcommand is given).
    Workspace {
        #[command(subcommand)]
        action: Option<WorkspaceCommand>,
    },
    /// Send one raw JSON control request to the workspace control socket.
    Ctl {
        /// The request, as one JSON object (may be split across arguments).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        json: Vec<String>,
    },
    /// Open a pane beside the focused one (sugar for a horizontal split of the focused pane).
    New(PaneArgs),
    /// Split a pane and run a command in the new half.
    Split(SplitArgs),
    /// Move the workspace focus.
    Focus {
        /// left|right|up|down|next|previous|last or a pane id.
        #[arg(value_parser = parse_focus)]
        target: FocusTarget,
    },
    /// Close a pane and terminate its process.
    Kill { pane: u32 },
    /// Set a manual pane label; an empty name restores the application title.
    RenamePane(RenamePaneArgs),
    /// Choose ordinary right-click ownership for a pane.
    PaneInput(PaneInputArgs),
    /// Resize the split around a pane by DELTA cells (negative shrinks).
    Resize {
        pane: u32,
        #[arg(allow_hyphen_values = true)]
        delta: i16,
    },
    /// Inspect and edit existing pane layouts without restarting their processes.
    Layout(layout_cli::LayoutArgs),
    /// Move a live pane to another workspace on this server, preserving its process.
    TransferPane(layout_cli::WorkspaceTransferArgs),
    /// Send input to a pane.
    SendKeys(SendKeysArgs),
    /// Capture a pane's screen.
    Capture(CaptureArgs),
    /// List the workspace's tabs and panes as JSON.
    List,
    /// Show the session server's pid, version, runtime directory and request bounds as JSON.
    Info,
    /// Tab commands.
    Tab {
        #[command(subcommand)]
        action: TabCommand,
    },
    /// Stream the workspace's lifecycle events as JSON lines.
    Subscribe,
}

/// Options shared by `new` and `split`: where and how the pane command runs.
#[derive(Debug, Args)]
struct PaneArgs {
    /// Working directory of the pane command (made absolute).
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Extra environment for the pane command, repeatable.
    #[arg(long, value_name = "NAME=VALUE", value_parser = parse_env)]
    env: Vec<(String, String)>,
    /// Initial pane height when no viewer sizes the tab (a headless workspace).
    #[arg(long)]
    rows: Option<u16>,
    /// Initial pane width when no viewer sizes the tab (a headless workspace).
    #[arg(long)]
    columns: Option<u16>,
    /// Ownership of ordinary right-clicks in the new pane.
    #[arg(long, value_enum, default_value_t = fux::view::RightClickPolicy::Auto)]
    right_click: fux::view::RightClickPolicy,
    /// The existing pane's share of the split on a 10000 scale.
    #[arg(long, default_value_t = 5000, value_parser = clap::value_parser!(u16).range(500..=9500))]
    ratio: u16,
    /// Focus the new pane (default).
    #[arg(long, overrides_with = "no_focus")]
    focus: bool,
    /// Keep the focus where it is.
    #[arg(long)]
    no_focus: bool,
    /// The command to run; the configured default command when omitted. Precede with `--`
    /// when it starts with a dash.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    argv: Vec<String>,
}

#[derive(Debug, Args)]
struct SplitArgs {
    /// horizontal (side by side) or vertical (stacked); `h` and `v` are accepted.
    #[arg(value_enum)]
    axis: fux::layout::Axis,
    /// The pane to split (the focused pane when omitted).
    #[arg(long)]
    target: Option<u32>,
    #[command(flatten)]
    pane: PaneArgs,
}

#[derive(Debug, Args)]
struct SendKeysArgs {
    pane: u32,
    /// Interpret the input as space-separated key names (`C-c Enter`) instead of byte escapes
    /// (`\n \r \t \e \\ \0 \xHH`).
    #[arg(long)]
    keys: bool,
    /// The input: one escaped string, or key names when `--keys` is given.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    input: Vec<String>,
}

#[derive(Debug, Args)]
struct CaptureArgs {
    pane: u32,
    /// Include text attributes.
    #[arg(long)]
    attrs: bool,
    /// Lines of history above the screen to include.
    #[arg(long, default_value_t = 0)]
    scrollback: u32,
    /// Return the visible grid cell by cell instead of text.
    #[arg(long)]
    cells: bool,
}

#[derive(Debug, Subcommand)]
enum TabCommand {
    /// Open a new tab, optionally named.
    New { name: Option<String> },
    /// Show the next tab.
    Next,
    /// Show the previous tab.
    #[command(alias = "prev")]
    Previous,
    /// Show the tab at a position.
    Select { index: u32 },
    /// Show a tab by its stable id.
    SelectId { tab: u32 },
    /// Rename a tab.
    Rename { tab: u32, name: String },
    /// Close a tab and terminate its panes.
    Close { tab: u32 },
    /// Move a tab before another (to the end when BEFORE is omitted).
    Reorder { tab: u32, before: Option<u32> },
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// List workspace names.
    List,
    /// Show the session server's identity and bounds.
    Info,
    /// List every workspace with its identity, layout generation and panes.
    Catalog,
    /// Export every workspace layout as an archive.
    ExportLayout,
    /// Apply a layout archive, checked against the export it was derived from.
    ApplyLayout {
        file: PathBuf,
        #[arg(long)]
        against: PathBuf,
    },
    /// Create a workspace (starting the session server when none is running).
    New { name: Option<String> },
    /// Terminate a workspace and its panes.
    Kill { name: String },
    /// Close a workspace by its observed identity.
    Close {
        name: String,
        /// Observed server instance from workspace catalog/list.
        #[arg(long)]
        instance: String,
        /// Observed workspace lifetime from workspace catalog/list.
        #[arg(long)]
        stream: u64,
    },
    /// Set a workspace's display label.
    Rename {
        name: String,
        label: String,
        #[arg(long)]
        instance: String,
        #[arg(long)]
        stream: u64,
    },
    /// Move a workspace before another (to the end when BEFORE is omitted).
    Reorder {
        name: String,
        before: Option<String>,
    },
}

#[derive(Debug, Args)]
struct PaneInputArgs {
    pane: u32,
    #[arg(long, value_enum)]
    right_click: fux::view::RightClickPolicy,
    #[arg(long)]
    instance: String,
}

#[derive(Debug, Args)]
struct RenamePaneArgs {
    pane: u32,
    name: String,
    /// Observed server instance from `fux info`.
    #[arg(long)]
    instance: String,
}

#[derive(Debug, Args)]
struct PaneLocationArgs {
    pane: u32,
    #[arg(long)]
    instance: String,
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Initial workspace name.
    #[arg(long, default_value = "default")]
    name: String,
    #[arg(long, hide = true)]
    daemon: bool,
    #[arg(long, hide = true)]
    startup_channel: Option<PathBuf>,
}

fn parse_env(value: &str) -> Result<(String, String), String> {
    value
        .split_once('=')
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .ok_or_else(|| "expected NAME=VALUE".to_owned())
}

fn parse_focus(value: &str) -> Result<FocusTarget, String> {
    Ok(match value {
        "last" => FocusTarget::Last,
        "next" => FocusTarget::Next,
        "previous" => FocusTarget::Previous,
        "left" => FocusTarget::Left,
        "right" => FocusTarget::Right,
        "up" => FocusTarget::Up,
        "down" => FocusTarget::Down,
        pane => FocusTarget::Pane(PaneId(pane.parse().map_err(|_| {
            "expected left|right|up|down|next|previous|last or a pane id".to_owned()
        })?)),
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let daemon = matches!(&cli.command, Some(Command::Serve(args)) if args.daemon);
    if let Err(error) = init_diagnostics(daemon) {
        eprintln!("fux: diagnostics unavailable: {error}");
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("fux: cannot start runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("fux: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_diagnostics(daemon: bool) -> Result<()> {
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};
    let detailed = std::env::var_os("FUX_DIAGNOSTICS").is_some_and(|value| value == "1");
    let writer = if daemon || detailed {
        let paths = fux::daemon::DaemonPaths::discover()?;
        paths.prepare()?;
        let log = paths.state_dir.join(if detailed {
            "diagnostics.log"
        } else {
            "daemon.log"
        });
        BoxMakeWriter::new(move || fux::daemon::CappedLog::open(&log))
    } else {
        BoxMakeWriter::new(std::io::stderr)
    };
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer)
        .with_filter(tracing_subscriber::filter::filter_fn(move |metadata| {
            if metadata.target() == "fux::diagnostics" {
                detailed
            } else {
                *metadata.level() <= tracing::Level::INFO
            }
        }));
    tracing_subscriber::registry()
        .with(layer)
        .try_init()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(())
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let workspace = cli.name.as_deref();
    match cli.command {
        Some(Command::Serve(args)) => {
            let config = fux::config::Config::load()?;
            fux::ids::validate_workspace_name(&args.name)?;
            let paths = fux::daemon::DaemonPaths::discover()?;
            let startup_lock = if args.startup_channel.is_none() {
                paths.prepare()?;
                Some(fux::daemon::StartupLock::acquire(&paths.runtime_dir)?)
            } else {
                None
            };
            let options = fux::server::ServeOptions {
                name: args.name,
                daemon: args.daemon,
                startup_channel: args.startup_channel,
                startup_lock,
            };
            fux::server::run(config, paths, options).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Attach {
            socket,
            target,
            report_exit,
        }) => {
            let config = fux::config::Config::load()?;
            let outcome = fux::client::attach_reported(
                &socket,
                &config,
                fux::client::AttachOptions {
                    initial: target.value()?,
                    manager_socket: None,
                },
            )
            .await?;
            if report_exit {
                eprintln!("{}", serde_json::to_string(&outcome)?);
            }
            Ok(outcome.code.map_or(ExitCode::SUCCESS, exit_code))
        }
        Some(Command::Bindings) => {
            let config = fux::config::Config::load()?;
            let bindings = fux::commands::configured_bindings(&config)?;
            println!("Prefix: {}", fux::commands::Key(bindings.prefix()));
            let mut previous = None;
            for (key, action) in bindings.entries() {
                let group = action.group();
                if previous != Some(group) {
                    println!("\n{}", group.label());
                    previous = Some(group);
                }
                println!(
                    "{:8} {}",
                    fux::commands::Key(key).to_string(),
                    action.label()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::LocatePane(args)) => manager(&ManagerRequest::PaneLocation {
            instance: args.instance,
            pane: PaneId(args.pane),
        }),
        Some(Command::Workspace { action }) => {
            workspace_command(action.unwrap_or(WorkspaceCommand::List))
        }
        Some(Command::Ctl { json }) => {
            let request = fux::proto::control::decode_request_frame(json.join(" ").as_bytes())?;
            send_control(workspace, request)
        }
        Some(Command::TransferPane(args)) => manager(&args.request()?),
        Some(Command::Layout(args)) => send_control(workspace, args.request()?),
        Some(command) => {
            let request = control_request(command, configured_final_retain_ms)?;
            send_control(workspace, request)
        }
        None => attach(workspace).await,
    }
}

/// The control request a control subcommand stands for; `final_retain_ms` is consulted only by
/// the commands that create panes, so the others never depend on a config file or `$HOME`.
fn control_request(
    command: Command,
    final_retain_ms: impl FnOnce() -> Result<u64>,
) -> Result<Request> {
    let id = 1;
    let split = |axis, target: Option<u32>, pane: PaneArgs| -> Result<Request> {
        Ok(Request::Split {
            stream: None,
            instance: None,
            id,
            axis,
            target: target.map(PaneId),
            cwd: pane.cwd.map(std::path::absolute).transpose()?,
            argv: pane.argv,
            env: pane.env,
            rows: pane.rows,
            columns: pane.columns,
            final_retain_ms: final_retain_ms()?,
            fixed_workspace: false,
            right_click: pane.right_click,
            ratio: pane.ratio,
            focus: !pane.no_focus,
        })
    };
    let request = match command {
        Command::New(pane) => split(fux::layout::Axis::Horizontal, None, pane)?,
        Command::Split(args) => split(args.axis, args.target, args.pane)?,
        Command::Focus { target } => Request::Focus {
            instance: None,
            id,
            target,
        },
        Command::Kill { pane } => Request::Kill {
            instance: None,
            id,
            pane: PaneId(pane),
        },
        Command::Resize { pane, delta } => Request::Resize {
            instance: None,
            id,
            pane: PaneId(pane),
            delta,
        },
        Command::PaneInput(args) => Request::PaneInput {
            id,
            instance: Some(args.instance),
            pane: PaneId(args.pane),
            right_click: args.right_click,
        },
        Command::RenamePane(args) => Request::RenamePane {
            id,
            instance: Some(args.instance),
            pane: PaneId(args.pane),
            name: args.name,
        },
        Command::SendKeys(args) => {
            let (notation, keys) = if args.keys {
                (fux::proto::control::KeyNotation::Keys, args.input.join(" "))
            } else {
                if args.input.len() != 1 {
                    bail!("send-keys takes one escaped string, or key names after --keys");
                }
                (
                    fux::proto::control::KeyNotation::Escapes,
                    args.input.into_iter().next().unwrap_or_default(),
                )
            };
            if keys.is_empty() {
                bail!("send-keys requires keys");
            }
            Request::SendKeys {
                instance: None,
                id,
                pane: PaneId(args.pane),
                keys,
                notation,
            }
        }
        Command::Capture(args) => Request::Capture {
            if_revision: None,
            instance: None,
            id,
            pane: PaneId(args.pane),
            attrs: args.attrs,
            scrollback: args.scrollback,
            max_bytes: fux::proto::control::MAX_CAPTURE_BYTES,
            format: if args.cells {
                fux::proto::control::CaptureFormat::Cells
            } else {
                fux::proto::control::CaptureFormat::Text
            },
        },
        Command::List => Request::List { instance: None, id },
        Command::Info => Request::Info { instance: None, id },
        Command::Tab { action } => Request::Tab {
            instance: None,
            id,
            action: match action {
                TabCommand::New { name } => TabAction::New { name },
                TabCommand::Next => TabAction::Next,
                TabCommand::Previous => TabAction::Previous,
                TabCommand::Select { index } => TabAction::Select {
                    target: TabTarget::Index(index),
                },
                TabCommand::SelectId { tab } => TabAction::Select {
                    target: TabTarget::Id(TabId(tab)),
                },
                TabCommand::Rename { tab, name } => TabAction::Rename {
                    tab: TabId(tab),
                    name,
                },
                TabCommand::Close { tab } => TabAction::Close { tab: TabId(tab) },
                TabCommand::Reorder { tab, before } => TabAction::Reorder {
                    tab: TabId(tab),
                    before: before.map(TabId),
                },
            },
        },
        Command::Subscribe => Request::Subscribe {
            after: None,
            instance: None,
            id,
        },
        Command::Serve(_)
        | Command::Attach { .. }
        | Command::Bindings
        | Command::LocatePane(_)
        | Command::Workspace { .. }
        | Command::Ctl { .. }
        | Command::Layout(_)
        | Command::TransferPane(_) => bail!("not a control command"),
    };
    request.validate()?;
    Ok(request)
}

fn exit_code(code: u32) -> ExitCode {
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
}

/// Attach to a workspace, starting the session server when none is running.
async fn attach(name: Option<&str>) -> Result<ExitCode> {
    if let Some(name) = name {
        fux::ids::validate_workspace_name(name)?;
    }
    let config = fux::config::Config::load()?;
    let paths = fux::daemon::DaemonPaths::discover()?;
    paths.prepare()?;
    let descriptor = {
        let _startup = fux::daemon::StartupLock::acquire(&paths.runtime_dir)?;
        match fux::daemon::resolve(&paths, name)? {
            Some(descriptor) => descriptor,
            None => fux::daemon::start_server(&paths, name.unwrap_or("default"))?,
        }
    };
    let code = fux::client::attach(
        &descriptor.socket_path,
        &config,
        fux::client::AttachOptions {
            initial: None,
            manager_socket: Some(paths.manager_socket.clone()),
        },
    )
    .await?;
    Ok(code.map_or(ExitCode::SUCCESS, exit_code))
}

/// Sends one manager request, prints the reply and maps it to an exit status.
fn manager(request: &ManagerRequest) -> Result<ExitCode> {
    let paths = fux::daemon::DaemonPaths::discover()?;
    let reply = fux::daemon::manager_request(&paths.manager_socket, request)?;
    print_reply(&reply)
}

fn print_reply(reply: &ManagerReply) -> Result<ExitCode> {
    println!("{}", serde_json::to_string(reply)?);
    Ok(reply.exit_code())
}

fn workspace_command(action: WorkspaceCommand) -> Result<ExitCode> {
    let paths = fux::daemon::DaemonPaths::discover()?;
    let identified = |name: String, instance, stream, action| -> Result<ExitCode> {
        let request = Request::Workspace {
            id: 1,
            instance: Some(instance),
            stream: Some(stream),
            action,
        };
        request.validate()?;
        send_control(Some(&name), request)
    };
    match action {
        WorkspaceCommand::List => manager(&ManagerRequest::List),
        WorkspaceCommand::Info => manager(&ManagerRequest::Info),
        WorkspaceCommand::Catalog => manager(&ManagerRequest::Catalog),
        WorkspaceCommand::ExportLayout => manager(&ManagerRequest::ExportLayout),
        WorkspaceCommand::ApplyLayout { file, against } => manager(&ManagerRequest::ApplyLayout {
            archive: layout_cli::read_archive(&file)?,
            expected: layout_cli::read_archive(&against)?,
        }),
        WorkspaceCommand::Kill { name } => manager(&ManagerRequest::Kill { name }),
        WorkspaceCommand::Reorder { name, before } => {
            manager(&ManagerRequest::Reorder { name, before })
        }
        WorkspaceCommand::Rename {
            name,
            label,
            instance,
            stream,
        } => identified(
            name,
            instance,
            stream,
            fux::proto::control::WorkspaceAction::Rename { label },
        ),
        WorkspaceCommand::Close {
            name,
            instance,
            stream,
        } => identified(
            name.clone(),
            instance,
            stream,
            fux::proto::control::WorkspaceAction::Kill { name },
        ),
        WorkspaceCommand::New { name } => {
            paths.prepare()?;
            let _startup = fux::daemon::StartupLock::acquire(&paths.runtime_dir)?;
            if let Some(name) = &name {
                fux::ids::validate_workspace_name(name)?;
            }
            let reply = match fux::daemon::manager_request(
                &paths.manager_socket,
                &ManagerRequest::Resolve { name: name.clone() },
            ) {
                Ok(reply) => reply,
                Err(error) if fux::daemon::no_server(&error) => {
                    let descriptor =
                        fux::daemon::start_server(&paths, name.as_deref().unwrap_or("default"))?;
                    ManagerReply::Attach { descriptor }
                }
                Err(error) => return Err(error),
            };
            print_reply(&reply)
        }
    }
}

/// The `[final] retain-ms` a CLI-created pane's final record keeps.
fn configured_final_retain_ms() -> Result<u64> {
    Ok(fux::config::Config::load()?.final_records.retain_ms)
}

fn control_path(workspace: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FUX_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let paths = fux::daemon::DaemonPaths::discover()?;
    Ok(paths.control_socket(workspace.unwrap_or("default"))?)
}

fn send_control(workspace: Option<&str>, request: Request) -> Result<ExitCode> {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;
    let answer_window = std::time::Duration::from_secs(30);
    let socket = control_path(workspace)?;
    let mut stream = UnixStream::connect(&socket)
        .map_err(|error| anyhow::anyhow!("connecting to {}: {error}", socket.display()))?;
    stream.set_read_timeout(Some(answer_window))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    fux::proto::socket::negotiate_client(&mut stream)?;
    fux::proto::control::write_frame(&mut stream, &request)?;
    let mut stdout = std::io::stdout().lock();
    let mut print_line = |frame: &[u8]| -> std::io::Result<()> {
        stdout.write_all(frame)?;
        stdout.write_all(b"\n")?;
        stdout.flush()
    };
    if matches!(request, Request::Subscribe { .. }) {
        let accepted =
            fux::daemon::read_json_frame(&mut stream, std::time::Duration::from_secs(30))?;
        print_line(&accepted)?;
        match serde_json::from_slice::<fux::proto::control::Reply>(&accepted)? {
            fux::proto::control::Reply::Accepted { .. } => {}
            fux::proto::control::Reply::Failed { .. } => return Ok(ExitCode::FAILURE),
            _ => bail!("unexpected subscription reply"),
        }
        stream.set_read_timeout(None)?;
        loop {
            let frame =
                fux::daemon::read_json_frame(&mut stream, std::time::Duration::from_secs(86_400))?;
            print_line(&frame)?;
        }
    }
    let frame = fux::daemon::read_json_frame(&mut stream, answer_window)?;
    let reply: fux::proto::control::Reply = serde_json::from_slice(&frame)?;
    print_line(&frame)?;
    Ok(reply.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    fn request(args: &[&str]) -> Result<Request> {
        let cli = Cli::try_parse_from(std::iter::once("fux").chain(args.iter().copied()))?;
        control_request(cli.command.context("subcommand")?, || {
            Ok(fux::config::DEFAULT_FINAL_RETAIN_MS)
        })
    }

    #[test]
    fn control_subcommands_build_validated_requests() {
        let split = request(&[
            "split",
            "vertical",
            "--target",
            "3",
            "--ratio",
            "7000",
            "--no-focus",
            "--right-click",
            "pane",
            "--cwd",
            "/tmp",
            "--",
            "sh",
            "-l",
        ]);
        assert!(matches!(
            split,
            Ok(Request::Split { axis: fux::layout::Axis::Vertical, target: Some(PaneId(3)), right_click: fux::view::RightClickPolicy::Pane, ratio: 7000, focus: false, argv, final_retain_ms: fux::config::DEFAULT_FINAL_RETAIN_MS, .. }) if argv == ["sh", "-l"]
        ));
        assert!(matches!(
            request(&["split", "h", "--ratio", "7000", "--no-focus"]),
            Ok(Request::Split { axis: fux::layout::Axis::Horizontal, target: None, focus: false, ratio: 7000, argv, .. }) if argv.is_empty()
        ));
        assert!(matches!(
            request(&["focus", "left"]),
            Ok(Request::Focus {
                target: FocusTarget::Left,
                ..
            })
        ));
        assert!(matches!(
            request(&["focus", "4"]),
            Ok(Request::Focus {
                target: FocusTarget::Pane(PaneId(4)),
                ..
            })
        ));
        assert!(request(&["focus", "sideways"]).is_err());
        assert!(request(&["resize", "1", "0"]).is_err());
        assert!(matches!(
            request(&["resize", "1", "-3"]),
            Ok(Request::Resize { delta: -3, .. })
        ));
        assert!(request(&["popup"]).is_err());
        assert!(request(&["tab", "close", "2"]).is_ok());
        assert!(request(&["tab", "prev"]).is_ok());
        assert!(request(&["subscribe"]).is_ok());
        assert!(request(&["subscribe", "pane.closed"]).is_err());
        assert!(request(&["new", "--right-click", "invalid"]).is_err());
        assert!(matches!(
            request(&["new", "--", "sh", "--right-click", "pane"]),
            Ok(Request::Split { right_click: fux::view::RightClickPolicy::Auto, argv, .. }) if argv == ["sh", "--right-click", "pane"]
        ));
        assert!(matches!(
            request(&["new", "--env", "A=1", "--rows", "10", "--columns", "40", "sh"]),
            Ok(Request::Split { rows: Some(10), columns: Some(40), env, argv, .. }) if env == [("A".to_owned(), "1".to_owned())] && argv == ["sh"]
        ));
        assert!(request(&["new", "--env", "novalue"]).is_err());
        assert!(request(&["wait", "1", "exit"]).is_err());
        for ratio in ["0", "499", "9501", "nan", "inf", "-1", "0.5"] {
            assert!(request(&["new", "--ratio", ratio]).is_err(), "{ratio}");
        }
        assert!(matches!(
            request(&["send-keys", "2", "\\x02"]),
            Ok(Request::SendKeys { keys, notation: fux::proto::control::KeyNotation::Escapes, .. }) if keys == "\\x02"
        ));
        assert!(matches!(
            request(&["send-keys", "2", "--keys", "C-c", "Enter"]),
            Ok(Request::SendKeys { keys, notation: fux::proto::control::KeyNotation::Keys, .. }) if keys == "C-c Enter"
        ));
        assert!(request(&["send-keys", "2"]).is_err());
        assert!(matches!(
            request(&["capture", "7", "--attrs", "--scrollback", "5"]),
            Ok(Request::Capture {
                pane: PaneId(7),
                attrs: true,
                scrollback: 5,
                format: fux::proto::control::CaptureFormat::Text,
                ..
            })
        ));
        assert!(matches!(
            request(&["capture", "7", "--cells"]),
            Ok(Request::Capture {
                attrs: false,
                format: fux::proto::control::CaptureFormat::Cells,
                ..
            })
        ));
        assert!(
            request(&["capture", "7", "--cells", "--attrs"]).is_err(),
            "cells already carry styles"
        );
        assert!(request(&["run"]).is_err());
    }

    #[test]
    fn workspace_and_name_parse_together() -> Result<()> {
        let cli = Cli::try_parse_from(["fux", "other", "workspace", "new", "x"])?;
        assert_eq!(cli.name.as_deref(), Some("other"));
        assert!(matches!(
            cli.command,
            Some(Command::Workspace {
                action: Some(WorkspaceCommand::New { name: Some(name) })
            }) if name == "x"
        ));
        let cli = Cli::try_parse_from(["fux", "workspace"])?;
        assert!(matches!(
            cli.command,
            Some(Command::Workspace { action: None })
        ));
        let cli = Cli::try_parse_from([
            "fux",
            "workspace",
            "apply-layout",
            "a.json",
            "--against",
            "b.json",
        ])?;
        assert!(matches!(
            cli.command,
            Some(Command::Workspace {
                action: Some(WorkspaceCommand::ApplyLayout { .. })
            })
        ));
        assert!(Cli::try_parse_from(["fux", "workspace", "apply-layout", "a.json"]).is_err());
        assert!(Cli::try_parse_from(["fux", "workspace", "close", "x"]).is_err());
        Ok(())
    }

    /// Every subcommand's help names its own options, so `--help` never drifts from the parser.
    #[test]
    fn help_matches_the_parser() {
        let mut command = Cli::command();
        let help = |name: &str, command: &mut clap::Command| {
            command
                .find_subcommand_mut(name)
                .unwrap_or_else(|| panic!("subcommand {name}"))
                .render_long_help()
                .to_string()
        };
        let split = help("split", &mut command);
        for option in [
            "--target",
            "--cwd",
            "--env",
            "--rows",
            "--columns",
            "--right-click",
            "--ratio",
            "--focus",
            "--no-focus",
            "horizontal",
            "vertical",
        ] {
            assert!(
                split.contains(option),
                "split help lacks {option}:\n{split}"
            );
        }
        let workspace = help("workspace", &mut command);
        for sub in [
            "list",
            "info",
            "catalog",
            "export-layout",
            "apply-layout",
            "new",
            "kill",
            "close",
            "rename",
            "reorder",
        ] {
            assert!(
                workspace.contains(sub),
                "workspace help lacks {sub}:\n{workspace}"
            );
        }
        let tab = help("tab", &mut command);
        for sub in [
            "new",
            "next",
            "previous",
            "select",
            "select-id",
            "rename",
            "close",
            "reorder",
        ] {
            assert!(tab.contains(sub), "tab help lacks {sub}:\n{tab}");
        }
        assert!(help("send-keys", &mut command).contains("--keys"));
        let capture = help("capture", &mut command);
        for option in ["--attrs", "--scrollback", "--cells"] {
            assert!(capture.contains(option), "capture help lacks {option}");
        }
        assert!(help("pane-input", &mut command).contains("auto, fux, pane"));
    }
}
