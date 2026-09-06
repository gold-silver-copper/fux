#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

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

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the session server in the foreground.
    Serve(ServeArgs),
    /// Attach to an explicit attachment socket (for example a koh gateway proxy socket).
    Attach {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Show the configured prefix and keybindings.
    Bindings,
    /// Manage workspaces on the session server: list, new [NAME], kill NAME.
    Workspace(PassthroughArgs),
    /// Send one raw JSON control request to the workspace control socket.
    Ctl(PassthroughArgs),
    /// Open a pane beside the focused one: [--cwd DIR] [--] [COMMAND...]
    New(PassthroughArgs),
    /// Split the focused pane: horizontal|vertical [--target PANE] [--cwd DIR] [--] [COMMAND...]
    Split(PassthroughArgs),
    /// Move the workspace focus: left|right|up|down|PANE
    Focus(PassthroughArgs),
    /// Close a pane and terminate its process: PANE
    Kill(PassthroughArgs),
    /// Resize the split around a pane: PANE DELTA
    Resize(PassthroughArgs),
    /// Send input bytes to a pane: PANE KEYS (escapes: \n \r \t \e \\ \0 \xHH)
    SendKeys(PassthroughArgs),
    /// Capture a pane's text: PANE [--attrs] [--scrollback LINES] [--rows] [--since SEQ]
    Capture(PassthroughArgs),
    /// List the workspace's tabs and panes as JSON.
    List(PassthroughArgs),
    /// Show the session server's pid, version, runtime directory and limits as JSON.
    Info(PassthroughArgs),
    /// Wait on a pane: PANE quiet MS | pattern REGEX | exit | seq N [--timeout MS]
    Wait(PassthroughArgs),
    /// Tab commands: new [NAME] | next | previous | select INDEX | select-id TAB | rename TAB NAME | close TAB
    Tab(PassthroughArgs),
    /// Stream lifecycle events as JSON lines: [EVENT...]
    Subscribe(PassthroughArgs),
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

#[derive(Debug, Args)]
struct PassthroughArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    arguments: Vec<String>,
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
        Some(Command::Attach { socket }) => {
            let config = fux::config::Config::load()?;
            let code = fux::client::attach(
                &socket,
                &config,
                fux::client::AttachOptions {
                    manager_socket: None,
                },
            )
            .await?;
            Ok(code.map_or(ExitCode::SUCCESS, exit_code))
        }
        Some(Command::Bindings) => {
            let config = fux::config::Config::load()?;
            let bindings = fux::commands::configured_bindings(&config)?;
            println!("Prefix: {}", fux::commands::key_name(bindings.prefix()));
            let mut previous = None;
            for (key, action) in bindings.entries() {
                let group = action.group();
                if previous != Some(group) {
                    println!("\n{}", group.label());
                    previous = Some(group);
                }
                println!("{:8} {}", fux::commands::key_name(key), action.label());
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Workspace(args)) => workspace_command(args.arguments),
        Some(Command::Ctl(args)) => ctl_json(cli.name.as_deref(), args.arguments),
        Some(Command::New(args)) => ctl_alias(cli.name.as_deref(), "new", args.arguments),
        Some(Command::Split(args)) => ctl_alias(cli.name.as_deref(), "split", args.arguments),
        Some(Command::Focus(args)) => ctl_alias(cli.name.as_deref(), "focus", args.arguments),
        Some(Command::Kill(args)) => ctl_alias(cli.name.as_deref(), "kill", args.arguments),
        Some(Command::Resize(args)) => ctl_alias(cli.name.as_deref(), "resize", args.arguments),
        Some(Command::SendKeys(args)) => {
            ctl_alias(cli.name.as_deref(), "send-keys", args.arguments)
        }
        Some(Command::Capture(args)) => ctl_alias(cli.name.as_deref(), "capture", args.arguments),
        Some(Command::List(args)) => ctl_alias(cli.name.as_deref(), "list", args.arguments),
        Some(Command::Info(args)) => ctl_alias(cli.name.as_deref(), "info", args.arguments),
        Some(Command::Wait(args)) => ctl_alias(cli.name.as_deref(), "wait", args.arguments),
        Some(Command::Tab(args)) => ctl_alias(cli.name.as_deref(), "tab", args.arguments),
        Some(Command::Subscribe(args)) => {
            ctl_alias(cli.name.as_deref(), "subscribe", args.arguments)
        }
        None => attach(cli.name.as_deref()).await,
    }
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
        match resolve(&paths, name)? {
            Some(descriptor) => descriptor,
            None => start_server(&paths, name.unwrap_or("default"))?,
        }
    };
    let code = fux::client::attach(
        &descriptor.socket_path,
        &config,
        fux::client::AttachOptions {
            manager_socket: Some(paths.manager_socket.clone()),
        },
    )
    .await?;
    Ok(code.map_or(ExitCode::SUCCESS, exit_code))
}

fn resolve(
    paths: &fux::daemon::DaemonPaths,
    name: Option<&str>,
) -> Result<Option<fux::daemon::Descriptor>> {
    match fux::daemon::manager_request(
        &paths.manager_socket,
        &fux::daemon::ManagerRequest::Resolve {
            name: name.map(str::to_owned),
        },
    ) {
        Ok(fux::daemon::ManagerReply::Attach { descriptor }) => Ok(Some(descriptor)),
        Ok(fux::daemon::ManagerReply::Failed { message }) => bail!("session server: {message}"),
        Ok(fux::daemon::ManagerReply::Names { .. }) => bail!("unexpected manager reply"),
        Err(error) if no_server(&error) => Ok(None),
        Err(error) => Err(error.context(
            "cannot use the existing session server; if it is older than this fux, save your work in it and restart it",
        )),
    }
}

/// No session server is listening (as opposed to one that answered badly).
fn no_server(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
    })
}

fn status(failed: bool) -> ExitCode {
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn start_server(paths: &fux::daemon::DaemonPaths, name: &str) -> Result<fux::daemon::Descriptor> {
    let executable = std::env::current_exe()?;
    let mut child = fux::daemon::ServerChild::spawn(&paths.runtime_dir, &executable, name)?;
    let deadline = std::time::Instant::now() + fux::daemon::STARTUP_TIMEOUT;
    loop {
        // READY may arrive before the manager answers; keep polling until the deadline either way.
        child.poll()?;
        if let Ok(Some(descriptor)) = resolve(paths, Some(name)) {
            // A reply from this exact child proves readiness even if the READY frame raced.
            let _ = child.confirm(descriptor.pid);
            return Ok(descriptor);
        }
        if std::time::Instant::now() >= deadline {
            bail!("session server startup timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn workspace_command(arguments: Vec<String>) -> Result<ExitCode> {
    let paths = fux::daemon::DaemonPaths::discover()?;
    let action = arguments.first().map(String::as_str).unwrap_or("list");
    let reply = match action {
        "list" => {
            fux::daemon::manager_request(&paths.manager_socket, &fux::daemon::ManagerRequest::List)?
        }
        "new" => {
            paths.prepare()?;
            let _startup = fux::daemon::StartupLock::acquire(&paths.runtime_dir)?;
            let name = arguments.get(1).cloned();
            if let Some(name) = &name {
                fux::ids::validate_workspace_name(name)?;
            }
            match fux::daemon::manager_request(
                &paths.manager_socket,
                &fux::daemon::ManagerRequest::Resolve { name: name.clone() },
            ) {
                Ok(reply) => reply,
                Err(error) if no_server(&error) => {
                    let descriptor = start_server(&paths, name.as_deref().unwrap_or("default"))?;
                    fux::daemon::ManagerReply::Attach { descriptor }
                }
                Err(error) => return Err(error),
            }
        }
        "kill" => fux::daemon::manager_request(
            &paths.manager_socket,
            &fux::daemon::ManagerRequest::Kill {
                name: arguments
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("workspace kill requires a name"))?
                    .clone(),
            },
        )?,
        _ => bail!("workspace requires list, new [NAME], or kill NAME"),
    };
    println!("{}", serde_json::to_string(&reply)?);
    Ok(status(matches!(
        reply,
        fux::daemon::ManagerReply::Failed { .. }
    )))
}

fn ctl_json(workspace: Option<&str>, arguments: Vec<String>) -> Result<ExitCode> {
    let input = arguments.join(" ");
    if input.is_empty() {
        bail!("ctl requires one JSON request");
    }
    let request = fux::proto::control::decode_request_frame(input.as_bytes())?;
    send_control(workspace, request)
}

fn ctl_alias(workspace: Option<&str>, command: &str, arguments: Vec<String>) -> Result<ExitCode> {
    let request = alias_request(command, &arguments)?;
    send_control(workspace, request)
}

fn control_path(workspace: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FUX_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let paths = fux::daemon::DaemonPaths::discover()?;
    Ok(paths.control_socket(workspace.unwrap_or("default"))?)
}

fn send_control(
    workspace: Option<&str>,
    request: fux::proto::control::Request,
) -> Result<ExitCode> {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;
    let socket = control_path(workspace)?;
    let mut stream = UnixStream::connect(&socket)
        .map_err(|error| anyhow::anyhow!("connecting to {}: {error}", socket.display()))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    fux::proto::socket::negotiate_client(&mut stream)?;
    fux::proto::control::write_frame(&mut stream, &request)?;
    let mut stdout = std::io::stdout().lock();
    let mut print_line = |frame: &[u8]| -> std::io::Result<()> {
        stdout.write_all(frame)?;
        stdout.write_all(b"\n")?;
        stdout.flush()
    };
    if matches!(request, fux::proto::control::Request::Subscribe { .. }) {
        let accepted =
            fux::daemon::read_json_frame(&mut stream, std::time::Duration::from_secs(30))?;
        print_line(&accepted)?;
        stream.set_read_timeout(None)?;
        loop {
            let frame =
                fux::daemon::read_json_frame(&mut stream, std::time::Duration::from_secs(86_400))?;
            print_line(&frame)?;
        }
    }
    let frame = fux::daemon::read_json_frame(&mut stream, std::time::Duration::from_secs(30))?;
    let reply: fux::proto::control::Reply = serde_json::from_slice(&frame)?;
    print_line(&frame)?;
    Ok(status(matches!(
        reply,
        fux::proto::control::Reply::Failed { .. }
    )))
}

fn alias_request(command: &str, args: &[String]) -> Result<fux::proto::control::Request> {
    use fux::ids::{PaneId, TabId};
    use fux::layout::Axis;
    use fux::proto::control::{EventKind, FocusTarget, Request, TabAction};
    let id = 1;
    let get = |index: usize, name: &str| {
        args.get(index)
            .ok_or_else(|| anyhow::anyhow!("{command} requires {name}"))
    };
    let number = |index: usize, name: &str| -> Result<u32> { Ok(get(index, name)?.parse()?) };
    let request = match command {
        "new" => {
            let (cwd, env, rows, columns, argv) = parse_pane_options(args)?;
            Request::New {
                id,
                cwd,
                argv,
                env,
                rows,
                columns,
            }
        }
        "split" => {
            let (axis, rest) = match args.first().map(String::as_str) {
                Some("horizontal" | "h") => (Axis::Horizontal, args.get(1..).unwrap_or_default()),
                Some("vertical" | "v") => (Axis::Vertical, args.get(1..).unwrap_or_default()),
                _ => bail!("split requires horizontal|vertical followed by options and a command"),
            };
            let (target, rest) = parse_target(rest)?;
            let (cwd, env, rows, columns, argv) = parse_pane_options(rest)?;
            Request::Split {
                id,
                axis,
                target: target.map(PaneId),
                cwd,
                argv,
                env,
                rows,
                columns,
            }
        }
        "focus" => {
            let target = match get(0, "a target")?.as_str() {
                "left" => FocusTarget::Left,
                "right" => FocusTarget::Right,
                "up" => FocusTarget::Up,
                "down" => FocusTarget::Down,
                value => FocusTarget::Pane(PaneId(value.parse()?)),
            };
            Request::Focus { id, target }
        }
        "kill" => Request::Kill {
            id,
            pane: PaneId(number(0, "a pane id")?),
        },
        "resize" => Request::Resize {
            id,
            pane: PaneId(number(0, "a pane id")?),
            delta: get(1, "a delta")?.parse()?,
        },
        "send-keys" => {
            let pane = PaneId(number(0, "a pane id")?);
            let rest = args.get(1..).unwrap_or_default();
            let (notation, payload) = if rest.first().map(String::as_str) == Some("--keys") {
                (
                    fux::proto::control::KeyNotation::Keys,
                    rest.get(1..).unwrap_or_default().join(" "),
                )
            } else {
                (
                    fux::proto::control::KeyNotation::Escapes,
                    rest.first().cloned().unwrap_or_default(),
                )
            };
            if payload.is_empty() {
                bail!("send-keys requires keys");
            }
            Request::SendKeys {
                id,
                pane,
                keys: payload,
                notation,
            }
        }
        "capture" => {
            let pane = PaneId(number(0, "a pane id")?);
            let options = parse_capture_options(args.get(1..).unwrap_or_default())?;
            Request::Capture {
                id,
                pane,
                attrs: options.attrs,
                scrollback: options.scrollback,
                max_bytes: fux::proto::control::MAX_CAPTURE_BYTES,
                format: if options.rows {
                    fux::proto::control::CaptureFormat::Rows
                } else {
                    fux::proto::control::CaptureFormat::Text
                },
                since: options.since,
            }
        }
        "list" => Request::List { id },
        "info" => Request::Info { id },
        "wait" => {
            use fux::proto::control::WaitUntil;
            let pane = PaneId(number(0, "a pane id")?);
            let mut timeout_ms = 30_000;
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            if let Some(position) = rest.iter().position(|a| a == "--timeout") {
                timeout_ms = rest
                    .get(position + 1)
                    .ok_or_else(|| anyhow::anyhow!("--timeout requires milliseconds"))?
                    .parse()?;
                rest.drain(position..=position + 1);
            }
            let until = match rest.first().map(String::as_str) {
                Some("quiet") => WaitUntil::Quiet {
                    ms: rest
                        .get(1)
                        .ok_or_else(|| anyhow::anyhow!("quiet requires ms"))?
                        .parse()?,
                },
                Some("pattern") => WaitUntil::Pattern {
                    regex: rest
                        .get(1)
                        .ok_or_else(|| anyhow::anyhow!("pattern requires a regex"))?
                        .clone(),
                },
                Some("exit") => WaitUntil::Exit,
                Some("seq") => WaitUntil::Seq {
                    value: rest
                        .get(1)
                        .ok_or_else(|| anyhow::anyhow!("seq requires a value"))?
                        .parse()?,
                },
                _ => bail!("wait requires quiet MS | pattern REGEX | exit | seq N"),
            };
            Request::Wait {
                id,
                pane,
                until,
                timeout_ms,
            }
        }
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
                "select-id" => TabAction::SelectId {
                    tab: TabId(number(1, "a tab id")?),
                },
                "rename" => TabAction::Rename {
                    tab: TabId(number(1, "a tab id")?),
                    name: get(2, "a name")?.to_owned(),
                },
                "close" => TabAction::Close {
                    tab: TabId(number(1, "a tab id")?),
                },
                _ => bail!("tab requires new, next, previous, select, select-id, rename or close"),
            };
            Request::Tab { id, action }
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
        _ => bail!("unknown control command {command}"),
    };
    request.validate()?;
    Ok(request)
}

fn parse_target(args: &[String]) -> Result<(Option<u32>, &[String])> {
    if args.first().map(String::as_str) == Some("--target") {
        let target = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("--target requires a pane id"))?
            .parse()?;
        return Ok((Some(target), args.get(2..).unwrap_or_default()));
    }
    Ok((None, args))
}

type PaneOptions = (
    Option<PathBuf>,
    Vec<(String, String)>,
    Option<u16>,
    Option<u16>,
    Vec<String>,
);

fn parse_pane_options(args: &[String]) -> Result<PaneOptions> {
    let mut cwd = None;
    let mut env = Vec::new();
    let mut rows = None;
    let mut columns = None;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        match argument.as_str() {
            "--" => {
                return Ok((
                    cwd,
                    env,
                    rows,
                    columns,
                    args.get(index + 1..).unwrap_or_default().to_vec(),
                ));
            }
            "--cwd" => {
                let directory = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("--cwd requires a directory"))?;
                cwd = Some(std::path::absolute(directory)?);
                index += 2;
            }
            "--env" => {
                let pair = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("--env requires NAME=VALUE"))?;
                let (name, value) = pair
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("--env requires NAME=VALUE"))?;
                env.push((name.to_owned(), value.to_owned()));
                index += 2;
            }
            "--rows" => {
                rows = Some(
                    args.get(index + 1)
                        .ok_or_else(|| anyhow::anyhow!("--rows requires a count"))?
                        .parse()?,
                );
                index += 2;
            }
            "--columns" => {
                columns = Some(
                    args.get(index + 1)
                        .ok_or_else(|| anyhow::anyhow!("--columns requires a count"))?
                        .parse()?,
                );
                index += 2;
            }
            _ => {
                return Ok((
                    cwd,
                    env,
                    rows,
                    columns,
                    args.get(index..).unwrap_or_default().to_vec(),
                ));
            }
        }
    }
    Ok((cwd, env, rows, columns, Vec::new()))
}

#[derive(Default)]
struct CaptureOptions {
    attrs: bool,
    scrollback: u32,
    rows: bool,
    since: Option<u64>,
}

fn parse_capture_options(args: &[String]) -> Result<CaptureOptions> {
    let mut options = CaptureOptions::default();
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        match argument.as_str() {
            "--attrs" => {
                options.attrs = true;
                index += 1;
            }
            "--rows" => {
                options.rows = true;
                index += 1;
            }
            "--scrollback" => {
                options.scrollback = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("--scrollback requires a line count"))?
                    .parse()?;
                index += 2;
            }
            "--since" => {
                options.since = Some(
                    args.get(index + 1)
                        .ok_or_else(|| anyhow::anyhow!("--since requires a sequence"))?
                        .parse()?,
                );
                // `since` only means anything for row captures.
                options.rows = true;
                index += 2;
            }
            value => bail!("unknown capture option {value}"),
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_build_validated_requests() {
        let split = alias_request(
            "split",
            &[
                "vertical".into(),
                "--target".into(),
                "3".into(),
                "--cwd".into(),
                "/tmp".into(),
                "--".into(),
                "sh".into(),
                "-l".into(),
            ],
        );
        assert!(matches!(
            split,
            Ok(fux::proto::control::Request::Split { axis: fux::layout::Axis::Vertical, target: Some(fux::ids::PaneId(3)), argv, .. }) if argv == ["sh", "-l"]
        ));
        assert!(alias_request("resize", &["1".into(), "0".into()]).is_err());
        assert!(alias_request("popup", &[]).is_err());
        assert!(alias_request("tab", &["close".into(), "2".into()]).is_ok());
        assert!(alias_request("subscribe", &["pane.closed".into()]).is_ok());
        assert!(alias_request("subscribe", &["agent.state".into()]).is_err());
    }

    #[test]
    fn daemon_log_is_private_and_truncated_at_one_mib() -> Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
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
