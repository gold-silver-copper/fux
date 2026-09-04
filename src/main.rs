#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use koh::client::{ConnectConfig, IdConfig};
use koh::keycmd::{KeyConfig, KeyOp};

#[allow(dead_code)]
mod runtime;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    name: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve(ServeArgs),
    Connect(ConnectArgs),
    Id(KeyFileArgs),
    Key(KeyArgs),
    Ctl(PassthroughArgs),
    New(PassthroughArgs),
    Split(PassthroughArgs),
    Focus(PassthroughArgs),
    Zoom(PassthroughArgs),
    Kill(PassthroughArgs),
    Resize(PassthroughArgs),
    SendKeys(PassthroughArgs),
    Capture(PassthroughArgs),
    List(PassthroughArgs),
    Tab(PassthroughArgs),
    Workspace(PassthroughArgs),
    SetStatus(PassthroughArgs),
    Popup(PassthroughArgs),
    Subscribe(PassthroughArgs),
}

#[derive(Debug, Args)]
struct KeyFileArgs {
    #[arg(long)]
    key_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ConnectArgs {
    server: String,
    #[arg(long)]
    key_file: Option<PathBuf>,
    #[arg(long, conflicts_with = "relay_url")]
    direct: Option<SocketAddr>,
    #[arg(long)]
    relay_url: Option<String>,
    #[arg(long)]
    clipboard: bool,
    #[arg(long, value_name = "CMD")]
    on_bell: Option<String>,
}

#[derive(Debug, Args)]
struct KeyArgs {
    #[command(subcommand)]
    operation: KeyOperation,
    #[arg(long, global = true)]
    key_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Subcommand)]
enum KeyOperation {
    Passwd,
    Info,
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long = "allow")]
    allow: Vec<String>,
    #[arg(long, default_value = "default")]
    name: String,
    #[arg(long, hide = true)]
    daemon: bool,
    #[arg(long, hide = true)]
    startup_channel: Option<String>,
}

#[derive(Debug, Args)]
struct PassthroughArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    arguments: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let daemon = matches!(&cli.command, Some(Command::Serve(args)) if args.daemon);
    if let Err(error) = init_diagnostics(daemon) {
        eprintln!("fux: diagnostics unavailable: {error}");
    }
    match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("fux: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_diagnostics(daemon: bool) -> Result<()> {
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    let writer = if daemon {
        let paths = fux::daemon::DaemonPaths::discover()?;
        paths.prepare()?;
        let log = paths.state_dir.join("daemon.log");
        BoxMakeWriter::new(move || CappedLog::open(&log))
    } else {
        BoxMakeWriter::new(std::io::stderr)
    };
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(writer)
        .try_init()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(())
}

enum CappedLog {
    File(std::fs::File),
    Sink(std::io::Sink),
}
impl CappedLog {
    fn open(path: &std::path::Path) -> Self {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut options = std::fs::OpenOptions::new();
        options
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW);
        let Ok(file) = options.open(path) else {
            return Self::Sink(std::io::sink());
        };
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let private = file.metadata().is_ok_and(|metadata| {
            metadata.is_file()
                && metadata.permissions().mode() & 0o077 == 0
                && path
                    .parent()
                    .and_then(|parent| std::fs::metadata(parent).ok())
                    .is_some_and(|parent| parent.uid() == metadata.uid())
        });
        if !private {
            return Self::Sink(std::io::sink());
        }
        if file
            .metadata()
            .is_ok_and(|metadata| metadata.len() >= 1024 * 1024)
        {
            let _ = file.set_len(0);
        }
        Self::File(file)
    }
}
impl std::io::Write for CappedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::File(file) => std::io::Write::write(file, bytes),
            Self::Sink(sink) => std::io::Write::write(sink, bytes),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::File(file) => std::io::Write::flush(file),
            Self::Sink(sink) => std::io::Write::flush(sink),
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Some(Command::Id(args)) => {
            koh::client::run_id(IdConfig {
                key_file: args.key_file,
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Key(args)) => {
            let op = match args.operation {
                KeyOperation::Passwd => KeyOp::Passwd,
                KeyOperation::Info => KeyOp::Info,
            };
            koh::keycmd::run(KeyConfig {
                op,
                key_file: args.key_file,
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Connect(args)) => {
            let config = fux::config::Config::load()?;
            let exit = fux::client::connect_workspace(
                ConnectConfig {
                    server: args.server,
                    key_file: args.key_file,
                    direct: args.direct,
                    relay_url: args.relay_url,
                    clipboard: args.clipboard,
                    bell_command: args.on_bell,
                },
                prefix_bytes(&config.prefix)?,
                (config.notifications.remote_clients
                    || std::env::var_os("TERMUX_VERSION").is_some())
                .then_some(config.notifications.clone()),
            )
            .await?;
            Ok(exit.map_or(ExitCode::SUCCESS, exit_code))
        }
        Some(Command::Serve(args)) => serve(args).await,
        Some(Command::Ctl(args)) => ctl_json(cli.name.as_deref(), args.arguments),
        Some(Command::New(args)) => ctl_alias(cli.name.as_deref(), "new", args.arguments),
        Some(Command::Split(args)) => ctl_alias(cli.name.as_deref(), "split", args.arguments),
        Some(Command::Focus(args)) => ctl_alias(cli.name.as_deref(), "focus", args.arguments),
        Some(Command::Zoom(args)) => ctl_alias(cli.name.as_deref(), "zoom", args.arguments),
        Some(Command::Kill(args)) => ctl_alias(cli.name.as_deref(), "kill", args.arguments),
        Some(Command::Resize(args)) => ctl_alias(cli.name.as_deref(), "resize", args.arguments),
        Some(Command::SendKeys(args)) => {
            ctl_alias(cli.name.as_deref(), "send-keys", args.arguments)
        }
        Some(Command::Capture(args)) => ctl_alias(cli.name.as_deref(), "capture", args.arguments),
        Some(Command::List(args)) => ctl_alias(cli.name.as_deref(), "list", args.arguments),
        Some(Command::Tab(args)) => ctl_alias(cli.name.as_deref(), "tab", args.arguments),
        Some(Command::Workspace(args)) => workspace_alias(args.arguments),
        Some(Command::SetStatus(args)) => {
            ctl_alias(cli.name.as_deref(), "set-status", args.arguments)
        }
        Some(Command::Popup(args)) => ctl_alias(cli.name.as_deref(), "popup", args.arguments),
        Some(Command::Subscribe(args)) => {
            ctl_alias(cli.name.as_deref(), "subscribe", args.arguments)
        }
        None => attach(cli.name.as_deref()).await,
    }
}

fn exit_code(code: u32) -> ExitCode {
    match u8::try_from(code) {
        Ok(code) => ExitCode::from(code),
        Err(_) => ExitCode::FAILURE,
    }
}

fn ctl_json(workspace: Option<&str>, arguments: Vec<String>) -> Result<ExitCode> {
    let input = arguments.join(" ");
    if input.is_empty() {
        bail!("ctl requires one JSON request");
    }
    let request = fux::control::decode_request_frame(input.as_bytes())?;
    if let fux::control::Request::Workspace { id, action } = request {
        return workspace_control_action(id, action);
    }
    send_control(workspace, request)
}

fn ctl_alias(workspace: Option<&str>, command: &str, arguments: Vec<String>) -> Result<ExitCode> {
    let request = alias_request(command, &arguments)?;
    send_control(workspace, request)
}

fn send_control(workspace: Option<&str>, request: fux::control::Request) -> Result<ExitCode> {
    let socket = control_path(workspace)?;
    if matches!(request, fux::control::Request::Subscribe { .. }) {
        runtime::subscribe(&socket, &request, std::io::stdout())?;
        return Ok(ExitCode::SUCCESS);
    }
    let reply = runtime::request(&socket, &request)?;
    println!("{}", serde_json::to_string(&reply)?);
    Ok(if matches!(reply, fux::control::Reply::Failed { .. }) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn control_path(workspace: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FUX_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let paths = fux::daemon::DaemonPaths::discover()?;
    let root = paths
        .runtime_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid runtime directory"))?;
    Ok(fux::control::control_socket_path(
        root,
        workspace.unwrap_or("default"),
    )?)
}

fn alias_request(command: &str, args: &[String]) -> Result<fux::control::Request> {
    use fux::control::{Axis, EventKind, FocusTarget, Request, TabAction, WorkspaceAction};
    let id = 1;
    let get = |index: usize, name: &str| {
        args.get(index)
            .ok_or_else(|| anyhow::anyhow!("{command} requires {name}"))
    };
    let number = |index: usize, name: &str| -> Result<u32> { Ok(get(index, name)?.parse()?) };
    let request = match command {
        "new" => {
            let (cwd, argv) = parse_cwd_and_argv(args)?;
            Request::New {
                id,
                cwd,
                argv,
                env: Default::default(),
            }
        }
        "split" => {
            let (axis, rest) = match args.first().map(String::as_str) {
                Some("horizontal" | "h") => (Axis::Horizontal, args.get(1..).unwrap_or_default()),
                Some("vertical" | "v") => (Axis::Vertical, args.get(1..).unwrap_or_default()),
                _ => bail!("split requires horizontal|vertical followed by a command"),
            };
            let (target, argv) = parse_target_and_argv(rest)?;
            Request::Split {
                id,
                axis,
                target,
                argv,
                env: Default::default(),
            }
        }
        "focus" => {
            let target = match get(0, "a target")?.as_str() {
                "left" => FocusTarget::Left,
                "right" => FocusTarget::Right,
                "up" => FocusTarget::Up,
                "down" => FocusTarget::Down,
                value => FocusTarget::Pane(value.parse()?),
            };
            Request::Focus { id, target }
        }
        "zoom" => Request::Zoom {
            id,
            pane: args.first().map(|value| value.parse()).transpose()?,
        },
        "kill" => Request::Kill {
            id,
            pane: number(0, "a pane id")?,
        },
        "resize" => Request::Resize {
            id,
            pane: number(0, "a pane id")?,
            delta: get(1, "a delta")?.parse()?,
        },
        "send-keys" => {
            let pane = number(0, "a pane id")?;
            let keys = get(1, "keys")?.to_owned();
            let _ = fux::control::decode_key_bytes(&keys)?;
            Request::SendKeys { id, pane, keys }
        }
        "capture" => {
            let pane = number(0, "a pane id")?;
            let (attrs, scrollback) = parse_capture_options(args.get(1..).unwrap_or_default())?;
            Request::Capture {
                id,
                pane,
                attrs,
                scrollback,
                max_bytes: fux::control::MAX_CAPTURE_BYTES,
            }
        }
        "list" => Request::List { id },
        "tab" => {
            let action = match get(0, "an action")?.as_str() {
                "new" => TabAction::New {
                    name: args.get(1).cloned(),
                },
                "next" => TabAction::Next,
                "previous" | "prev" => TabAction::Previous,
                "select" => TabAction::Select {
                    index: number(1, "an index")?,
                },
                _ => bail!("invalid tab action"),
            };
            Request::Tab { id, action }
        }
        "workspace" => {
            let action = match get(0, "an action")?.as_str() {
                "list" => WorkspaceAction::List,
                "new" => WorkspaceAction::New {
                    name: get(1, "a name")?.to_owned(),
                },
                "kill" => WorkspaceAction::Kill {
                    name: get(1, "a name")?.to_owned(),
                },
                _ => bail!("invalid workspace action"),
            };
            Request::Workspace { id, action }
        }
        "set-status" => Request::SetStatus {
            id,
            segment: get(0, "a segment")?.to_owned(),
            text: get(1, "text")?.to_owned(),
        },
        "popup" => {
            let (rows, cols, argv) = parse_popup_options(args)?;
            Request::Popup {
                id,
                rows,
                cols,
                argv,
                env: Default::default(),
            }
        }
        "subscribe" => {
            let events = args
                .iter()
                .map(|value| {
                    serde_json::from_value::<EventKind>(serde_json::Value::String(value.clone()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Request::Subscribe { id, events }
        }
        _ => bail!("unknown control alias {command}"),
    };
    request.validate()?;
    Ok(request)
}

fn parse_cwd_and_argv(args: &[String]) -> Result<(Option<PathBuf>, Vec<String>)> {
    let mut cwd = None;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        match argument.as_str() {
            "--" => return Ok((cwd, args.get(index + 1..).unwrap_or_default().to_vec())),
            "--cwd" => {
                cwd =
                    Some(PathBuf::from(args.get(index + 1).ok_or_else(|| {
                        anyhow::anyhow!("--cwd requires a directory")
                    })?));
                index += 2;
            }
            _ => return Ok((cwd, args.get(index..).unwrap_or_default().to_vec())),
        }
    }
    Ok((cwd, Vec::new()))
}

fn parse_target_and_argv(args: &[String]) -> Result<(Option<u32>, Vec<String>)> {
    let mut target = None;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        match argument.as_str() {
            "--" => return Ok((target, args.get(index + 1..).unwrap_or_default().to_vec())),
            "--target" => {
                target = Some(
                    args.get(index + 1)
                        .ok_or_else(|| anyhow::anyhow!("--target requires a pane id"))?
                        .parse()?,
                );
                index += 2;
            }
            _ => return Ok((target, args.get(index..).unwrap_or_default().to_vec())),
        }
    }
    Ok((target, Vec::new()))
}

fn parse_capture_options(args: &[String]) -> Result<(bool, u32)> {
    let mut attrs = false;
    let mut scrollback = 0;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        match argument.as_str() {
            "--attrs" => {
                attrs = true;
                index += 1;
            }
            "--scrollback" => {
                scrollback = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("--scrollback requires a line count"))?
                    .parse()?;
                index += 2;
            }
            value => bail!("unknown capture option {value}"),
        }
    }
    Ok((attrs, scrollback))
}

fn parse_popup_options(args: &[String]) -> Result<(Option<u16>, Option<u16>, Vec<String>)> {
    let mut rows = None;
    let mut cols = None;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        match argument.as_str() {
            "--" => {
                return Ok((
                    rows,
                    cols,
                    args.get(index + 1..).unwrap_or_default().to_vec(),
                ));
            }
            "--size" => {
                let size = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("--size requires COLSxROWS"))?;
                let (width, height) = size
                    .split_once('x')
                    .ok_or_else(|| anyhow::anyhow!("popup size must be COLSxROWS"))?;
                cols = Some(width.parse()?);
                rows = Some(height.parse()?);
                index += 2;
            }
            _ => {
                return Ok((rows, cols, args.get(index..).unwrap_or_default().to_vec()));
            }
        }
    }
    Ok((rows, cols, Vec::new()))
}

async fn serve(args: ServeArgs) -> Result<ExitCode> {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    let config_path = fux::config::default_path()?;
    let live = Arc::new(runtime::LiveConfig::load(config_path)?);
    let config = live.snapshot();
    let paths = fux::daemon::DaemonPaths::discover()?;
    paths.prepare()?;
    let _manager = fux::daemon::ManagerLock::bind(&paths)?;
    let transferred = match &args.startup_channel {
        Some(channel) => {
            let mut bytes = fux::daemon::receive_startup_secret(channel)?;
            if bytes.len() != 64 {
                bytes.fill(0);
                bail!("invalid startup key bundle");
            }
            let mut client = [0_u8; 32];
            let mut server = [0_u8; 32];
            client.copy_from_slice(
                bytes
                    .get(..32)
                    .ok_or_else(|| anyhow::anyhow!("invalid client key"))?,
            );
            server.copy_from_slice(
                bytes
                    .get(32..)
                    .ok_or_else(|| anyhow::anyhow!("invalid server key"))?,
            );
            bytes.fill(0);
            let client_secret = iroh::SecretKey::from_bytes(&client);
            let server_secret = iroh::SecretKey::from_bytes(&server);
            client.fill(0);
            server.fill(0);
            Some((client_secret, server_secret))
        }
        None => None,
    };
    let (client_secret, server_secret) = if let Some(pair) = transferred {
        pair
    } else {
        let client_key = koh::transport_iroh::default_key_path("client")?;
        (
            koh::transport_iroh::load_or_create_secret_key(&client_key)?,
            koh::transport_iroh::load_or_create_secret_key(&paths.key(&args.name)?)?,
        )
    };
    let local_id = koh::transport_iroh::format_endpoint_id(&client_secret.public());
    let explicit_allow = args
        .allow
        .into_iter()
        .chain(config.remote_allow_ids.clone())
        .collect::<BTreeSet<_>>();
    let mut daemon = fux::daemon::Daemon::new(
        paths.clone(),
        std::process::id(),
        explicit_allow,
        local_id,
        monotonic_ms(),
    )?;
    let initial =
        create_served_workspace(&mut daemon, &paths, &config, &args.name, server_secret).await;
    let initial = match initial {
        Ok(value) => value,
        Err(error) => {
            if let Some(channel) = &args.startup_channel {
                let _ = fux::daemon::report_startup(channel, Some(&error.to_string()));
            }
            return Err(error);
        }
    };
    let descriptor = initial.descriptor.clone();
    let hook_registry = Arc::new(std::sync::Mutex::new(vec![Arc::clone(&initial.hooks)]));
    let event_registry = Arc::new(std::sync::Mutex::new(vec![initial.events.clone()]));
    let control_registry = Arc::new(std::sync::Mutex::new(vec![initial.control.clone()]));
    let mut workspaces = vec![initial];
    _manager.listener().set_nonblocking(true)?;
    let reload_shutdown = tokio_util::sync::CancellationToken::new();
    let reload_task = tokio::spawn(runtime::reload_on_sighup(
        Arc::clone(&live),
        Some(Arc::clone(&hook_registry)),
        Some(Arc::clone(&event_registry)),
        Some(Arc::clone(&control_registry)),
        reload_shutdown.clone(),
    ));
    if let Some(channel) = &args.startup_channel {
        fux::daemon::report_startup(channel, None)?;
    }
    if !args.daemon {
        eprintln!(
            "fux: serving {} at {}",
            descriptor.name, descriptor.endpoint_id
        );
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => { result?; break; }
            signal = terminate.recv() => {
                if signal.is_some() { break; }
            }
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {
                let now = std::time::Instant::now();
                let empty: Vec<_> = workspaces
                    .iter_mut()
                    .filter_map(|workspace| {
                        if !workspace.control.is_empty() {
                            workspace.empty_since = None;
                            return None;
                        }
                        let empty_since = *workspace.empty_since.get_or_insert(now);
                        runtime::terminal_workspace_retirement_due(
                            empty_since,
                            workspace.control.attached_clients(),
                            now,
                        )
                        .then(|| workspace.descriptor.name.clone())
                    })
                    .collect();
                for name in empty {
                    let _ = daemon.kill(&name);
                    if let Some(index) = workspaces.iter().position(|workspace| workspace.descriptor.name == name) {
                        let workspace = workspaces.remove(index);
                        hook_registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|hooks| !Arc::ptr_eq(hooks, &workspace.hooks));
                        event_registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|events| !events.same_instance(&workspace.events));
                        control_registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|control| !control.same_instance(&workspace.control));
                        workspace.shutdown();
                    }
                }
                if workspaces.is_empty() { break; }
                if let Ok((mut stream, _)) = _manager.listener().accept() {
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
                    let reply = match runtime::read_manager_request(&mut stream) {
                        Ok(runtime::ManagerRequest::List) => runtime::ManagerReply::Pick { names: daemon.names() },
                        Ok(runtime::ManagerRequest::Kill { name }) => {
                            let result = daemon.kill(&name);
                            if result.is_ok() {
                                if let Some(index) = workspaces.iter().position(|workspace| workspace.descriptor.name == name) {
                                    let workspace = workspaces.remove(index);
                                    hook_registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|hooks| !Arc::ptr_eq(hooks, &workspace.hooks));
                                    event_registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|events| !events.same_instance(&workspace.events));
                                    control_registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|control| !control.same_instance(&workspace.control));
                                    workspace.shutdown();
                                }
                                runtime::ManagerReply::Pick { names: daemon.names() }
                            } else { runtime::ManagerReply::Failed { message: "workspace not found".into() } }
                        }
                        Ok(runtime::ManagerRequest::Resolve { name, mut server_key }) => {
                            let result = match daemon.resolve(name.as_deref()) {
                                Ok(fux::daemon::Resolution::Attach(descriptor)) => Ok(descriptor),
                                Ok(fux::daemon::Resolution::Pick(names)) => { let _ = runtime::write_manager_reply(&mut stream, &runtime::ManagerReply::Pick { names }); continue; }
                                Ok(fux::daemon::Resolution::Create(name)) => {
                                    let bytes = server_key.as_mut().ok_or_else(|| anyhow::anyhow!("new workspace requires a provisioned server key"));
                                    match bytes.and_then(|bytes| { if bytes.len() != 32 { bail!("invalid server key") } let mut raw = [0_u8;32]; raw.copy_from_slice(bytes); bytes.fill(0); Ok(iroh::SecretKey::from_bytes(&raw)) }) {
                                        Ok(secret) => create_served_workspace(&mut daemon, &paths, &live.snapshot(), &name, secret).await.map(|workspace| { let descriptor = workspace.descriptor.clone(); hook_registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(Arc::clone(&workspace.hooks)); event_registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(workspace.events.clone()); control_registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(workspace.control.clone()); workspaces.push(workspace); descriptor }),
                                        Err(error) => Err(error),
                                    }
                                }
                                Err(error) => Err(error.into()),
                            };
                            match result { Ok(descriptor) => runtime::ManagerReply::Attach { descriptor }, Err(error) => runtime::ManagerReply::Failed { message: error.to_string() } }
                        }
                        Err(error) => runtime::ManagerReply::Failed { message: error.to_string() },
                    };
                    let _ = runtime::write_manager_reply(&mut stream, &reply);
                    if workspaces.is_empty() { break; }
                }
            }
        }
    }
    reload_shutdown.cancel();
    let _ = reload_task.await;
    for workspace in workspaces {
        workspace.shutdown();
    }
    drop(daemon);
    Ok(ExitCode::SUCCESS)
}

struct ServedWorkspace {
    descriptor: fux::daemon::Descriptor,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    control_task: std::thread::JoinHandle<()>,
    hooks: std::sync::Arc<runtime::LiveHooks>,
    events: runtime::EventHub,
    control: fux::host::WorkspaceControl,
    empty_since: Option<std::time::Instant>,
}

impl ServedWorkspace {
    fn shutdown(self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        self.hooks.shutdown();
        self.control.shutdown();
        let _ = self.control_task.join();
    }
}

async fn create_served_workspace(
    daemon: &mut fux::daemon::Daemon,
    paths: &fux::daemon::DaemonPaths,
    config: &fux::config::Config,
    name: &str,
    server_secret: iroh::SecretKey,
) -> Result<ServedWorkspace> {
    use std::sync::{Arc, Mutex};
    let (session, control) = fux::host::WorkspaceHost::shared(
        config.default_command.argv.clone(),
        config.history.scrollback_lines as usize,
        Some(config.zor_path.clone()),
    )?;
    let session = Arc::new(Mutex::new(Some(session)));
    let descriptor = daemon
        .create_or_find_async(name, |_key, allow| {
            let session = Arc::clone(&session);
            async move {
                fux::daemon::bind_workspace_endpoint_with_secret(
                    server_secret,
                    &allow,
                    fux::FUX_ALPN,
                    fux::daemon::NetworkProfile::Default,
                    move || {
                        session
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                            .ok_or_else(|| anyhow::anyhow!("workspace session already initialized"))
                    },
                )
                .await
            }
        })
        .await?;
    let finish = (|| {
        let runtime_root = paths
            .runtime_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid runtime directory"))?;
        let socket = fux::control::bind_control_socket(runtime_root, name)?;
        control.configure_bindings(config, socket.path().to_owned())?;
        let events = runtime::EventHub::with_notifications(config.notifications.clone());
        control.set_event_sink(Arc::new(events.clone()));
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let control_task = runtime::serve_control(
            socket,
            Arc::new(runtime::WorkspaceControlHandler::new(
                control.clone(),
                events.clone(),
                name.to_owned(),
            )),
            events.clone(),
            Arc::clone(&shutdown),
        )?;
        let hooks = Arc::new(runtime::LiveHooks::new(
            &config.hooks,
            std::env::vars_os(),
            Arc::new(runtime::ProcessHookCommand),
        ));
        Ok(ServedWorkspace {
            descriptor,
            shutdown,
            control_task,
            hooks,
            events,
            control: control.clone(),
            empty_since: None,
        })
    })();
    if finish.is_err() {
        control.shutdown();
        let _ = daemon.kill(name);
    }
    finish
}

fn monotonic_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| {
            value.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

async fn attach(name: Option<&str>) -> Result<ExitCode> {
    let paths = fux::daemon::DaemonPaths::discover()?;
    paths.prepare()?;
    if let Some(descriptor) = resolve_through_manager(&paths, name)? {
        return connect_descriptor(descriptor).await;
    }
    let descriptor = start_workspace(&paths, name.unwrap_or("default"))?;
    connect_descriptor(descriptor).await
}

async fn connect_descriptor(descriptor: fux::daemon::Descriptor) -> Result<ExitCode> {
    let config = fux::config::Config::load()?;
    let paths = fux::daemon::DaemonPaths::discover()?;
    let mut descriptor = descriptor;
    loop {
        let outcome = fux::client::connect_workspace_with_picker(
            koh::client::ConnectConfig {
                server: descriptor.endpoint_id.clone(),
                key_file: None,
                direct: Some(descriptor.direct_addr.into()),
                relay_url: None,
                clipboard: matches!(
                    config.clipboard,
                    fux::config::ClipboardPolicy::WriteOnly
                        | fux::config::ClipboardPolicy::ReadWrite
                ),
                bell_command: None,
            },
            prefix_bytes(&config.prefix)?,
            std::env::var_os("TERMUX_VERSION")
                .is_some()
                .then_some(config.notifications.clone()),
            true,
        )
        .await?;
        match outcome {
            fux::client::ConnectOutcome::Exited(exit) => {
                return Ok(exit.map_or(ExitCode::SUCCESS, exit_code));
            }
            fux::client::ConnectOutcome::WorkspacePicker => {
                let name = choose_workspace(&paths, &descriptor.name)?;
                descriptor = resolve_through_manager(&paths, Some(&name))?
                    .ok_or_else(|| anyhow::anyhow!("workspace manager disappeared"))?;
            }
        }
    }
}

fn choose_workspace(paths: &fux::daemon::DaemonPaths, current: &str) -> Result<String> {
    let runtime::ManagerReply::Pick { names } =
        runtime::manager_request(&paths.manager_socket, &runtime::ManagerRequest::List)?
    else {
        bail!("workspace manager did not return a workspace list")
    };
    if names.is_empty() {
        bail!("no workspaces are available")
    }
    choose_from_names(&names, current)
}

fn choose_from_names(names: &[String], current: &str) -> Result<String> {
    use std::io::{BufRead as _, Write as _};
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    writeln!(tty, "\nworkspaces:")?;
    for (index, name) in names.iter().enumerate() {
        writeln!(
            tty,
            "  {}. {}{}",
            index + 1,
            name,
            if name == current { " (current)" } else { "" }
        )?;
    }
    write!(tty, "select workspace [1-{}]: ", names.len())?;
    tty.flush()?;
    let mut selection = String::new();
    std::io::BufReader::new(tty.try_clone()?).read_line(&mut selection)?;
    runtime::select_workspace(names, &selection)
}

fn resolve_through_manager(
    paths: &fux::daemon::DaemonPaths,
    name: Option<&str>,
) -> Result<Option<fux::daemon::Descriptor>> {
    let first = runtime::manager_request(
        &paths.manager_socket,
        &runtime::ManagerRequest::Resolve {
            name: name.map(str::to_owned),
            server_key: None,
        },
    );
    match first {
        Ok(runtime::ManagerReply::Attach { descriptor }) => Ok(Some(descriptor)),
        Ok(runtime::ManagerReply::Pick { names }) => {
            let selected = choose_from_names(&names, "")?;
            resolve_existing_workspace(paths, &selected).map(Some)
        }
        Ok(runtime::ManagerReply::Failed { .. }) => {
            let workspace = name.unwrap_or("default");
            let secret = koh::transport_iroh::load_or_create_secret_key(&paths.key(workspace)?)?;
            let mut request = runtime::ManagerRequest::Resolve {
                name: Some(workspace.to_owned()),
                server_key: Some(secret.to_bytes().to_vec()),
            };
            let reply = runtime::manager_request(&paths.manager_socket, &request);
            let runtime::ManagerRequest::Resolve {
                server_key: Some(raw),
                ..
            } = &mut request
            else {
                bail!("missing server key")
            };
            raw.fill(0);
            match reply? {
                runtime::ManagerReply::Attach { descriptor } => Ok(Some(descriptor)),
                runtime::ManagerReply::Pick { names } => {
                    let selected = choose_from_names(&names, "")?;
                    resolve_existing_workspace(paths, &selected).map(Some)
                }
                runtime::ManagerReply::Failed { message } => {
                    bail!("manager request failed: {message}")
                }
            }
        }
        Err(_) => Ok(None),
    }
}

fn resolve_existing_workspace(
    paths: &fux::daemon::DaemonPaths,
    name: &str,
) -> Result<fux::daemon::Descriptor> {
    match runtime::manager_request(
        &paths.manager_socket,
        &runtime::ManagerRequest::Resolve {
            name: Some(name.to_owned()),
            server_key: None,
        },
    )? {
        runtime::ManagerReply::Attach { descriptor } => Ok(descriptor),
        runtime::ManagerReply::Failed { message } => bail!("manager request failed: {message}"),
        runtime::ManagerReply::Pick { .. } => bail!("manager returned another picker response"),
    }
}

fn workspace_alias(arguments: Vec<String>) -> Result<ExitCode> {
    let paths = fux::daemon::DaemonPaths::discover()?;
    let action = arguments.first().map(String::as_str).unwrap_or("list");
    let reply = match action {
        "list" => runtime::manager_request(&paths.manager_socket, &runtime::ManagerRequest::List)?,
        "new" => {
            let name = arguments
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("workspace new requires a name"))?;
            let secret = koh::transport_iroh::load_or_create_secret_key(&paths.key(name)?)?;
            let mut request = runtime::ManagerRequest::Resolve {
                name: Some(name.clone()),
                server_key: Some(secret.to_bytes().to_vec()),
            };
            let reply = runtime::manager_request(&paths.manager_socket, &request);
            let runtime::ManagerRequest::Resolve {
                server_key: Some(raw),
                ..
            } = &mut request
            else {
                bail!("missing server key")
            };
            raw.fill(0);
            reply?
        }
        "kill" => runtime::manager_request(
            &paths.manager_socket,
            &runtime::ManagerRequest::Kill {
                name: arguments
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("workspace kill requires a name"))?
                    .clone(),
            },
        )?,
        _ => bail!("workspace requires list, new, or kill"),
    };
    println!("{}", serde_json::to_string(&reply)?);
    Ok(if matches!(reply, runtime::ManagerReply::Failed { .. }) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn workspace_control_action(id: u64, action: fux::control::WorkspaceAction) -> Result<ExitCode> {
    let paths = fux::daemon::DaemonPaths::discover()?;
    let result = match action {
        fux::control::WorkspaceAction::List => {
            runtime::manager_request(&paths.manager_socket, &runtime::ManagerRequest::List)
        }
        fux::control::WorkspaceAction::New { name } => {
            let secret = koh::transport_iroh::load_or_create_secret_key(&paths.key(&name)?)?;
            let mut request = runtime::ManagerRequest::Resolve {
                name: Some(name),
                server_key: Some(secret.to_bytes().to_vec()),
            };
            let result = runtime::manager_request(&paths.manager_socket, &request);
            let runtime::ManagerRequest::Resolve {
                server_key: Some(bytes),
                ..
            } = &mut request
            else {
                bail!("missing server key")
            };
            bytes.fill(0);
            result
        }
        fux::control::WorkspaceAction::Kill { name } => runtime::manager_request(
            &paths.manager_socket,
            &runtime::ManagerRequest::Kill { name },
        ),
    };
    let reply = match result {
        Ok(runtime::ManagerReply::Failed { message }) => fux::control::Reply::Failed {
            id,
            error: fux::control::ReplyError {
                code: fux::control::ErrorCode::Internal,
                message,
            },
        },
        Ok(runtime::ManagerReply::Pick { names }) => fux::control::Reply::Completed {
            id,
            result: fux::control::CommandResult::Listing {
                workspaces: names
                    .into_iter()
                    .map(|name| fux::control::WorkspaceSummary {
                        name,
                        focused: false,
                        tabs: Vec::new(),
                    })
                    .collect(),
            },
        },
        Ok(runtime::ManagerReply::Attach { descriptor }) => fux::control::Reply::Completed {
            id,
            result: fux::control::CommandResult::Listing {
                workspaces: vec![fux::control::WorkspaceSummary {
                    name: descriptor.name,
                    focused: true,
                    tabs: Vec::new(),
                }],
            },
        },
        Err(error) => fux::control::Reply::Failed {
            id,
            error: fux::control::ReplyError {
                code: fux::control::ErrorCode::Internal,
                message: error.to_string(),
            },
        },
    };
    println!("{}", serde_json::to_string(&reply)?);
    Ok(if matches!(reply, fux::control::Reply::Failed { .. }) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn start_workspace(
    paths: &fux::daemon::DaemonPaths,
    name: &str,
) -> Result<fux::daemon::Descriptor> {
    use fux::daemon::{DaemonSpawner as _, SpawnTicket as _};
    let executable = std::env::current_exe()?;
    let mut spawner = fux::daemon::ProcessDaemonSpawner::new(paths.runtime_dir.clone());
    let mut ticket = spawner.spawn(fux::daemon::SpawnRequest {
        executable,
        args: vec![
            "serve".into(),
            "--daemon".into(),
            "--name".into(),
            name.into(),
        ],
        environment: fux::daemon::sanitized_environment(std::env::vars_os()),
        stdin: fux::daemon::StdioPolicy::Null,
        stdout: fux::daemon::StdioPolicy::Null,
        stderr: fux::daemon::StdioPolicy::Null,
        error_channel: true,
    })?;
    let client_path = koh::transport_iroh::default_key_path("client")?;
    let client = koh::transport_iroh::load_or_create_secret_key(&client_path)?;
    let server = koh::transport_iroh::load_or_create_secret_key(&paths.key(name)?)?;
    let mut bundle = Vec::with_capacity(64);
    bundle.extend_from_slice(&client.to_bytes());
    bundle.extend_from_slice(&server.to_bytes());
    let transfer = ticket.send_secret(&bundle);
    bundle.fill(0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut startup_error = transfer.err().map(|error| error.to_string());
    loop {
        if startup_error.is_none() {
            startup_error = ticket.try_error();
        }
        // A winning child must first disarm its cleanup ticket through READY. A losing child may
        // reconnect immediately: dropping its still-armed ticket is precisely the desired reap.
        if (ticket.is_ready() || startup_error.is_some())
            && let Ok(Some(descriptor)) = resolve_through_manager(paths, Some(name))
        {
            return Ok(descriptor);
        }
        if std::time::Instant::now() >= deadline {
            if let Some(error) = startup_error {
                bail!("daemon startup failed: {error}");
            }
            bail!("daemon startup timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn prefix_bytes(value: &str) -> Result<Vec<u8>> {
    if let Some(letter) = value.strip_prefix("C-")
        && letter.len() == 1
        && let Some(byte) = letter.bytes().next()
        && byte.is_ascii()
    {
        return Ok(vec![byte.to_ascii_uppercase() & 0x1f]);
    }
    Ok(fux::control::decode_key_bytes(value)?)
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn daemon_log_is_private_and_truncated_at_one_mib() -> Result<()> {
        let root = std::env::temp_dir().join(format!("fux-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let path = root.join("daemon.log");
        std::fs::write(&path, vec![b'x'; 1024 * 1024])?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let mut log = CappedLog::open(&path);
        log.write_all(b"fresh")?;
        log.flush()?;
        assert!(std::fs::metadata(&path)?.len() < 1024 * 1024);
        assert_eq!(std::fs::metadata(&path)?.permissions().mode() & 0o077, 0);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
