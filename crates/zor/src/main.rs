// This binary is the CLI output surface: it prints command results as JSON to stdout and
// diagnostics to stderr. The library modules stay strict; only this entrypoint may print.
#![allow(clippy::print_stdout, clippy::print_stderr)]
use std::io::IsTerminal;
use std::process::ExitCode;

mod cli;

fn main() -> ExitCode {
    match run(cli::Cli::parse_args()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("zor: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut cli: cli::Cli) -> anyhow::Result<u8> {
    let Some(action) = cli.action.take() else {
        anyhow::ensure!(
            cli.machine
                .as_deref()
                .is_none_or(|machine| machine.eq_ignore_ascii_case("local")),
            "remote machine selection requires a supported command"
        );
        return wrap(&cli, Vec::new());
    };
    if let (
        Some(selector),
        cli::Action::Dashboard {
            all_machines: false,
            once: false,
            directory,
            bell,
            notify,
            notification_command,
        },
    ) = (cli.machine.as_deref(), &action)
    {
        anyhow::ensure!(
            cli.state_directory.is_none(),
            "machine dashboard observes running services; cannot override --state-directory"
        );
        anyhow::ensure!(
            selector.eq_ignore_ascii_case("local") || directory.is_none(),
            "remote dashboard cannot select a local --directory"
        );
        let catalog = zor::machines::Catalog::load(&zor::machines::Catalog::path(
            cli.machines_file.clone(),
        )?)?;
        let scope = if selector.eq_ignore_ascii_case("local") {
            "local".to_owned()
        } else {
            catalog.resolve(selector)?.id.clone()
        };
        anyhow::ensure!(
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            "dashboard requires a terminal; use dashboard --once for JSON"
        );
        let sources = zor::dashboard::multi::sources(
            directory.clone(),
            catalog,
            cli.koh_binary.clone().unwrap_or_else(|| "koh".into()),
        )?;
        return zor::dashboard::multi::run(
            sources,
            false,
            cli.fux_binary.clone().unwrap_or_else(|| "fux".into()),
            Some(scope),
            zor::dashboard::multi::Reload {
                catalog_path: zor::machines::Catalog::path(cli.machines_file.clone())?,
                directory: directory.clone(),
                koh_binary: cli.koh_binary.clone().unwrap_or_else(|| "koh".into()),
            },
            zor::dashboard::multi::Notices {
                bell: *bell,
                notify: *notify,
                command: notification_command.clone(),
            },
        );
    }
    if let Some(machine) = cli.machine.as_deref()
        && !machine.eq_ignore_ascii_case("local")
    {
        return remote_command(&cli, machine, action);
    }
    match action {
        cli::Action::Machine { action } => {
            use zor::machines::Catalog;
            let path = Catalog::path(cli.machines_file)?;
            let value = match action {
                cli::MachineAction::ResumeIntents => serde_json::to_value(
                    zor::machines::intents::list(&zor::machines::intents::path(&path)?)?,
                )?,
                cli::MachineAction::List => {
                    let catalog = Catalog::load(&path)?;
                    serde_json::json!({"version":catalog.version,"local":{"id":"local","name":"Local"},"machines":catalog.machines})
                }
                cli::MachineAction::Inspect { machine } => {
                    serde_json::to_value(Catalog::load(&path)?.resolve(&machine)?)?
                }
                cli::MachineAction::Add { name, binding } => {
                    let binding = binding.value()?;
                    serde_json::to_value(Catalog::edit(&path, |catalog| {
                        catalog.add(name, binding)
                    })?)?
                }
                cli::MachineAction::Rename { machine, name } => Catalog::edit(&path, |catalog| {
                    let id = catalog.resolve(&machine)?.id.clone();
                    catalog.rename(&id, name)?;
                    serde_json::to_value(catalog.resolve(&id)?).map_err(Into::into)
                })?,
                cli::MachineAction::Remove { machine } => {
                    serde_json::to_value(Catalog::edit(&path, |catalog| catalog.remove(&machine))?)?
                }
                cli::MachineAction::Control {
                    machine,
                    clear,
                    binding,
                } => {
                    let binding = binding.value()?;
                    anyhow::ensure!(
                        clear || binding.is_some(),
                        "provide --endpoint and --key-file, or --clear"
                    );
                    Catalog::edit(&path, |catalog| {
                        catalog.control(&machine, binding)?;
                        serde_json::to_value(catalog.resolve(&machine)?).map_err(Into::into)
                    })?
                }
                cli::MachineAction::Bind {
                    machine,
                    workspace,
                    clear,
                    binding,
                } => {
                    let binding = binding.value()?;
                    anyhow::ensure!(
                        clear || binding.is_some(),
                        "provide --endpoint and --key-file, or --clear"
                    );
                    Catalog::edit(&path, |catalog| {
                        catalog.bind(&machine, workspace, binding)?;
                        serde_json::to_value(catalog.resolve(&machine)?).map_err(Into::into)
                    })?
                }
            };
            println!("{}", serde_json::to_string(&value)?);
            Ok(0)
        }
        cli::Action::Command(command) => wrap(&cli, command),
        cli::Action::CodexWorker { task, argv } => {
            let root = zor::tasks::state_root(cli.state_directory)?;
            zor::tasks::codex::worker::run(&root, &task, argv)
        }
        cli::Action::OpencodeExec { plugin, argv } => {
            zor::tasks::integration::exec_opencode(&plugin, argv)
        }
        cli::Action::Worktree { action } => {
            let root = zor::tasks::state_root(cli.state_directory)?;
            let value = match action {
                cli::WorktreeAction::Remove { id, force } => {
                    zor::tasks::worktree::remove(&root, &id, force)?
                }
                cli::WorktreeAction::Create {
                    id,
                    repo,
                    branch,
                    base,
                } => zor::tasks::worktree::create(
                    &root,
                    zor::tasks::worktree::Create {
                        id,
                        repo,
                        branch,
                        base,
                    },
                )?,
                cli::WorktreeAction::List => zor::tasks::worktree::list(&root)?,
                cli::WorktreeAction::Inspect { id } => zor::tasks::worktree::inspect(&root, &id)?,
                cli::WorktreeAction::Reconcile { id } => {
                    zor::tasks::worktree::reconcile(&root, &id)?
                }
            };
            println!("{}", serde_json::to_string(&value)?);
            Ok(0)
        }
        cli::Action::Task { action } => {
            let root = zor::tasks::state_root(cli.state_directory)?;
            let value = match action {
                cli::TaskAction::CodexStart {
                    id,
                    title,
                    instance,
                    workspace,
                    cwd,
                    worktree,
                    runtime,
                    argv,
                } => zor::tasks::codex::worker::start(
                    &root,
                    zor::tasks::launch::Start {
                        id,
                        title,
                        instance,
                        workspace,
                        cwd,
                        worktree,
                        argv,
                        runtime: runtime.map_or_else(zor::fux::runtime, Ok)?,
                        agent: Some("codex".into()),
                        integration: None,
                    },
                )?,
                cli::TaskAction::CodexSubmit {
                    id,
                    operation,
                    text,
                    timeout_ms,
                } => zor::tasks::codex::state::prepare(&root, &id, &operation, &text, timeout_ms)?,
                cli::TaskAction::CodexInspect { operation } => {
                    zor::tasks::codex::state::inspect(&root, &operation)?
                }
                cli::TaskAction::CodexRecreate {
                    operation,
                    request,
                    timeout_ms,
                } => zor::tasks::codex::state::request_control(
                    &root,
                    &operation,
                    &request,
                    zor::tasks::codex::state::ControlKind::Recreate,
                    timeout_ms,
                )?,
                cli::TaskAction::CodexReconcile {
                    operation,
                    request,
                    timeout_ms,
                } => zor::tasks::codex::state::request_control(
                    &root,
                    &operation,
                    &request,
                    zor::tasks::codex::state::ControlKind::Read,
                    timeout_ms,
                )?,
                cli::TaskAction::CodexInterrupt {
                    operation,
                    request,
                    timeout_ms,
                } => zor::tasks::codex::state::request_control(
                    &root,
                    &operation,
                    &request,
                    zor::tasks::codex::state::ControlKind::Interrupt,
                    timeout_ms,
                )?,
                cli::TaskAction::Recover { after } => {
                    zor::tasks::recovery::resume(&root, after.as_deref())?
                }
                cli::TaskAction::Handoff {
                    id,
                    operation,
                    predecessors,
                    text,
                    timeout_ms,
                } => zor::tasks::handoff::prepare(
                    &root,
                    &id,
                    &operation,
                    predecessors,
                    text,
                    timeout_ms,
                )?,
                cli::TaskAction::Verify { id, source } => {
                    zor::tasks::verify::run(&root, &id, &source)?
                }
                cli::TaskAction::SourceCollect {
                    id,
                    source,
                    revision,
                } => zor::tasks::source::collect(&root, &id, &source, revision)?,
                cli::TaskAction::SourceInspect { source } => {
                    zor::tasks::source::inspect(&root, &source)?
                }
                cli::TaskAction::SourceFile { source, path } => {
                    zor::tasks::source::file(&root, &source, &path)?
                }
                cli::TaskAction::ChangesCollect { id, changes } => {
                    zor::tasks::changes::collect(&root, &id, &changes)?
                }
                cli::TaskAction::ChangesInspect { changes } => {
                    zor::tasks::changes::inspect(&root, &changes)?
                }
                cli::TaskAction::Result { id } => zor::tasks::result::read(&root, &id)?,
                cli::TaskAction::RequireArtifact { id, artifact, path } => {
                    zor::tasks::artifact::require(&root, &id, &artifact, path)?
                }
                cli::TaskAction::ArtifactCollect {
                    id,
                    artifact,
                    path,
                    requirement,
                } => zor::tasks::artifact::collect(&root, &id, &artifact, path, requirement)?,
                cli::TaskAction::ArtifactInspect { artifact } => {
                    zor::tasks::artifact::inspect(&root, &artifact)?
                }
                cli::TaskAction::Check {
                    id,
                    check,
                    timeout_ms,
                    requirement,
                    source,
                    artifacts,
                    argv,
                } => zor::tasks::check::run(
                    &root,
                    zor::tasks::check::Run {
                        task: id,
                        id: check,
                        argv,
                        timeout_ms,
                        requirement,
                        source,
                        artifacts: zor::tasks::check::artifact_arguments(artifacts)?,
                    },
                )?,
                cli::TaskAction::CheckInspect { check } => {
                    zor::tasks::check::inspect(&root, &check)?
                }
                cli::TaskAction::RequireCheck { id, check, argv } => {
                    zor::tasks::check::require(&root, &id, &check, argv)?
                }
                cli::TaskAction::Start {
                    id,
                    title,
                    integration,
                    headless_agent,
                    instance,
                    workspace,
                    cwd,
                    worktree,
                    runtime,
                    argv,
                } => zor::tasks::launch::start(
                    &root,
                    zor::tasks::launch::Start {
                        argv: zor::tasks::headless::argv(
                            headless_agent.as_deref(),
                            argv,
                            integration.is_some(),
                        )?,
                        integration: integration
                            .map(|_| zor::tasks::model::IntegrationKind::Opencode),
                        id,
                        title,
                        instance,
                        workspace,
                        cwd,
                        worktree,
                        runtime: runtime.map_or_else(zor::fux::runtime, Ok)?,
                        agent: cli.agent,
                    },
                )?,
                cli::TaskAction::ResumeStatus { id, operation } => serde_json::to_value(
                    zor::tasks::resume::operation_status(&root, &id, &operation)?,
                )?,
                cli::TaskAction::Resume {
                    id,
                    operation,
                    instance,
                } => zor::tasks::resume::run(&root, &id, &operation, &instance)?,
                cli::TaskAction::LaunchReconcile { id } => {
                    zor::tasks::launch::reconcile(&root, &id)?
                }
                cli::TaskAction::Adopt {
                    id,
                    title,
                    instance,
                    workspace,
                    pane,
                    runtime,
                } => zor::tasks::adopt(
                    &root,
                    zor::tasks::Adopt {
                        id,
                        title,
                        instance,
                        workspace,
                        pane,
                        runtime: runtime.map_or_else(zor::fux::runtime, Ok)?,
                        agent: cli.agent,
                    },
                )?,
                cli::TaskAction::List => zor::tasks::list(&root)?,
                cli::TaskAction::Inspect { id } => zor::tasks::inspect(&root, &id)?,
                cli::TaskAction::Prepare {
                    id,
                    operation,
                    text,
                    timeout_ms,
                } => zor::tasks::prepare(&root, &id, &operation, &text, timeout_ms)?,
                cli::TaskAction::Reserve { operation } => {
                    zor::tasks::submit::run(&root, &operation, zor::tasks::submit::Action::Reserve)?
                }
                cli::TaskAction::Submit { operation } => {
                    zor::tasks::submit::run(&root, &operation, zor::tasks::submit::Action::Submit)?
                }
                cli::TaskAction::Reconcile { operation } => zor::tasks::submit::run(
                    &root,
                    &operation,
                    zor::tasks::submit::Action::Reconcile,
                )?,
                cli::TaskAction::Abandon { operation } => zor::tasks::abandon(&root, &operation)?,
                cli::TaskAction::Cancel { id } => zor::tasks::cancel(&root, &id)?,
                cli::TaskAction::GroupCreate {
                    id,
                    operation,
                    after,
                    concurrency,
                } => zor::tasks::group::create(
                    &root,
                    &id,
                    concurrency,
                    zor::tasks::group::cli_members(operation, after)?,
                )?,
                cli::TaskAction::GroupInspect { id } => zor::tasks::group::inspect(&root, &id)?,
                cli::TaskAction::GroupList => zor::tasks::group::list(&root)?,
                cli::TaskAction::GroupStep { id } => zor::tasks::group::step(&root, &id)?,
                cli::TaskAction::GroupRun { id, directory } => zor::service::run_group(
                    directory,
                    &root,
                    &id,
                    &cli.rules,
                    cli.agent.as_deref(),
                )?,
                cli::TaskAction::GroupPause { id } => {
                    zor::tasks::group::configure(&root, &id, false)?
                }
                cli::TaskAction::GroupCancel { id } => zor::tasks::group::cancel(&root, &id)?,
                cli::TaskAction::Stop { id } => zor::tasks::stop::run(&root, &id)?,
                cli::TaskAction::AdapterStatus { id } => {
                    zor::tasks::integration::status(&root, &id)?
                }
                cli::TaskAction::AdapterCapabilities { agent } => {
                    zor::tasks::capabilities::inspect(&agent)?
                }
                cli::TaskAction::RegisterAdapter {
                    id,
                    marker,
                    producer,
                } => zor::tasks::integration::register(&root, &id, &marker, &producer)?,
                cli::TaskAction::HeartbeatAdapter {
                    id,
                    marker,
                    producer,
                    sequence,
                    observation,
                } => zor::tasks::heartbeat::record(
                    &root,
                    &id,
                    &marker,
                    &producer,
                    sequence,
                    observation
                        .as_deref()
                        .map(zor::tasks::heartbeat::parse)
                        .transpose()?,
                )?,
                cli::TaskAction::BindReport {
                    operation,
                    token,
                    producer,
                    sequence,
                    input_operation,
                    agent_session,
                    message,
                } => zor::tasks::binding::run(
                    &root,
                    &operation,
                    &token,
                    zor::tasks::binding::Bind {
                        producer,
                        sequence,
                        input_operation,
                        message: zor::tasks::model::AgentMessage {
                            session: agent_session,
                            id: message,
                        },
                    },
                )?,
                cli::TaskAction::Report {
                    operation,
                    token,
                    producer,
                    sequence,
                    input_operation,
                    kind,
                    agent_session,
                    message,
                } => zor::tasks::wait::report(
                    &root,
                    &operation,
                    &token,
                    zor::tasks::wait::Report {
                        producer,
                        sequence,
                        input_operation,
                        message: agent_session
                            .zip(message)
                            .map(|(session, id)| zor::tasks::model::AgentMessage { session, id }),
                        kind: match kind {
                            cli::ReportKind::NeedsInput => {
                                zor::tasks::model::ResponseKind::NeedsInput
                            }
                            cli::ReportKind::ResponseObserved => {
                                zor::tasks::model::ResponseKind::ResponseObserved
                            }
                        },
                    },
                )?,
                cli::TaskAction::Wait {
                    operation,
                    follow,
                    timeout_ms,
                } => {
                    if follow {
                        zor::tasks::wait::follow(&root, &operation, timeout_ms.unwrap_or(30000))?
                    } else {
                        zor::tasks::wait::check(&root, &operation)?
                    }
                }
                cli::TaskAction::DiscardPrepared { operation } => {
                    zor::tasks::discard_prepared(&root, &operation)?
                }
                cli::TaskAction::Forget { id } => zor::tasks::forget(&root, &id)?,
            };
            println!("{value}");
            Ok(0)
        }
        cli::Action::Serve {
            runtime,
            directory,
            daemon_child,
        } => {
            if daemon_child {
                zor::service::run_daemon(
                    directory,
                    runtime,
                    cli.state_directory,
                    &cli.rules,
                    cli.agent.as_deref(),
                )
            } else {
                zor::service::run(
                    directory,
                    runtime,
                    cli.state_directory,
                    &cli.rules,
                    cli.agent.as_deref(),
                )
            }
        }
        cli::Action::Status { directory, start } => {
            let response = if start {
                zor::service::ensure(
                    directory,
                    &cli.rules,
                    cli.agent.as_deref(),
                    cli.state_directory,
                )?
            } else {
                zor::service::status(directory)?
            };
            println!("{response}");
            Ok(0)
        }
        cli::Action::Dashboard {
            all_machines,
            directory,
            once,
            bell,
            notify,
            notification_command,
        } => {
            anyhow::ensure!(
                once || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal()),
                "dashboard requires a terminal; use dashboard --once for JSON"
            );
            if all_machines {
                let catalog = zor::machines::Catalog::load(&zor::machines::Catalog::path(
                    cli.machines_file.clone(),
                )?)?;
                let sources = zor::dashboard::multi::sources(
                    directory.clone(),
                    catalog,
                    cli.koh_binary.clone().unwrap_or_else(|| "koh".into()),
                )?;
                return zor::dashboard::multi::run(
                    sources,
                    once,
                    cli.fux_binary.unwrap_or_else(|| "fux".into()),
                    None,
                    zor::dashboard::multi::Reload {
                        catalog_path: zor::machines::Catalog::path(cli.machines_file.clone())?,
                        directory,
                        koh_binary: cli.koh_binary.unwrap_or_else(|| "koh".into()),
                    },
                    zor::dashboard::multi::Notices {
                        bell,
                        notify,
                        command: notification_command,
                    },
                );
            }
            zor::service::ensure(
                directory.clone(),
                &cli.rules,
                cli.agent.as_deref(),
                cli.state_directory,
            )?;
            if once {
                println!(
                    "{}",
                    serde_json::to_string(&zor::dashboard::snapshot(directory)?)?
                );
                Ok(0)
            } else {
                zor::dashboard::run(directory, bell, notify, notification_command)
            }
        }
        cli::Action::Shutdown { directory } => {
            println!("{}", zor::service::shutdown(directory)?);
            Ok(0)
        }
        cli::Action::Run {
            timeout,
            rows,
            columns,
            env,
            cwd,
            workspace,
            argv,
        } => zor::run::run(zor::run::Run {
            timeout_ms: timeout,
            rows,
            columns,
            env: zor::run::env_pairs(env)?,
            cwd,
            workspace,
            argv,
        }),
        cli::Action::Watch { runtime, once } => {
            zor::watch::run(runtime, once, &cli.rules, cli.agent.as_deref())
        }
        cli::Action::Observe { socket, pane, pid } => {
            let sets = zor::rules::bundle::load_all(&cli.rules)?;
            zor::observe::run(&socket, pane, pid, cli.agent.as_deref(), &sets)
        }
        cli::Action::Agents => {
            let sets = zor::rules::bundle::load_all(&cli.rules)?;
            let agents: Vec<_> = sets
                .iter()
                .map(|set| {
                    serde_json::json!({
                        "id":set.id,"process_names":set.process_names,"rules":set.rules.len()
                    })
                })
                .collect();
            println!("{}", serde_json::json!({"agents":agents}));
            Ok(0)
        }
        cli::Action::Check { fixture, agent } => {
            let sets = zor::rules::bundle::load_all(&cli.rules)?;
            check_fixture(&fixture, agent.as_deref(), &sets)
        }
    }
}

/// `zor [flags] <program> [args...]`: run one program in a pseudoterminal, forward every byte,
/// and publish the observed agent state as OSC 7877. Under an outer wrapper (`ZOR_PID`) it
/// only forwards. An empty command wraps `$SHELL -l`.
#[cfg(feature = "wrap")]
fn wrap(cli: &cli::Cli, command: Vec<String>) -> anyhow::Result<u8> {
    let sets = zor::rules::bundle::load_all(&cli.rules)?;
    let program = command
        .first()
        .cloned()
        .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned()));
    let argv = if command.is_empty() {
        vec!["-l".to_owned()]
    } else {
        command.into_iter().skip(1).collect()
    };
    let agent = cli.agent.clone().map(zor::osc::AgentId::new).transpose()?;
    let title = match cli.title {
        cli::TitleMode::Never => zor::emit::title::Mode::Never,
        cli::TitleMode::Prefix => zor::emit::title::Mode::Prefix,
        cli::TitleMode::Replace => zor::emit::title::Mode::Replace,
    };
    if std::env::var_os("ZOR_PID").is_some() {
        zor::pty::run_transparent(&program, &argv)
    } else {
        zor::pty::run(
            &program,
            &argv,
            zor::pty::Options {
                rule_sets: sets,
                agent,
                no_osc: cli.no_osc,
                title,
                events: cli.events.clone(),
                debug: cli.debug,
            },
        )
    }
}

/// This build has no PTY wrapper: a program name is an error, not a silent no-op.
#[cfg(not(feature = "wrap"))]
fn wrap(_cli: &cli::Cli, command: Vec<String>) -> anyhow::Result<u8> {
    anyhow::bail!(
        "the PTY wrapper is not compiled into this zor (build with the `wrap` feature); \
         {} is not a zor subcommand",
        command.first().map_or("an empty command", String::as_str)
    )
}

fn check_fixture(
    path: &std::path::Path,
    forced: Option<&str>,
    sets: &[zor::rules::RuleSet],
) -> anyhow::Result<u8> {
    let source =
        zor::rules::bundle::read_bounded_utf8(path, zor::rules::bundle::MAX_FIXTURE_BYTES)?;
    let mut agent = forced.map(str::to_owned);
    let mut title = String::new();
    let mut progress = None;
    let mut expected = None;
    let mut matched = None;
    let mut body = Vec::new();
    for line in source.lines() {
        if let Some(value) = line.strip_prefix("# agent: ") {
            if agent.is_none() {
                agent = Some(value.to_owned());
            }
        } else if let Some(value) = line.strip_prefix("# title: ") {
            title = value.to_owned();
        } else if let Some(value) = line.strip_prefix("# progress: ") {
            let mut fields = value.split(':');
            progress = fields
                .next()
                .and_then(|state| state.parse().ok())
                .zip(fields.next().and_then(|percent| percent.parse().ok()))
                .map(|(state, percent)| zor::rules::view::Progress { state, percent });
        } else if let Some(value) = line.strip_prefix("# expect: ") {
            expected = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("# matched: ") {
            matched = Some(value.to_owned());
        } else if !line.starts_with('#') {
            body.push(line.to_owned());
        }
    }
    let id = agent.ok_or_else(|| anyhow::anyhow!("fixture has no agent"))?;
    let set = sets
        .iter()
        .find(|set| set.id == id)
        .ok_or_else(|| anyhow::anyhow!("no loaded rule set for {id}"))?;
    let view = FixtureView::new(body, title, progress);
    let verdict = zor::rules::evaluate(set, &view);
    let state = format!("{:?}", verdict.state).to_lowercase();
    let rule = verdict.rule.as_deref().unwrap_or("none");
    println!("{state} {rule}");
    Ok(
        if expected.as_deref() == Some(state.as_str()) && matched.as_deref() == Some(rule) {
            0
        } else {
            1
        },
    )
}

struct FixtureView {
    lines: Vec<String>,
    text: String,
    title: String,
    progress: Option<zor::rules::view::Progress>,
}
impl FixtureView {
    fn new(
        lines: Vec<String>,
        title: String,
        progress: Option<zor::rules::view::Progress>,
    ) -> Self {
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        Self {
            lines,
            text,
            title,
            progress,
        }
    }
}
impl zor::rules::view::ScreenView for FixtureView {
    fn lines(&self) -> impl Iterator<Item = std::borrow::Cow<'_, str>> {
        self.lines
            .iter()
            .map(|line| std::borrow::Cow::Borrowed(line.as_str()))
    }
    fn text(&self) -> &str {
        &self.text
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn progress(&self) -> Option<zor::rules::view::Progress> {
        self.progress
    }
    fn size(&self) -> (u16, u16) {
        (u16::try_from(self.lines.len()).unwrap_or(u16::MAX), 0)
    }
}

/// Remote selection is handled before any local task store or service auto-start path.
fn remote_command(cli: &cli::Cli, selector: &str, action: cli::Action) -> anyhow::Result<u8> {
    use anyhow::Context;
    enum Read {
        Status,
        Dashboard,
        List,
        Inspect(String),
        Result(String),
        Supervise(String, zor::tasks::supervise::Action),
        ResumeStatus {
            id: String,
            operation: String,
        },
        Resume {
            id: String,
            operation: String,
            instance: String,
        },
    }
    let read = match action {
        cli::Action::Status {
            directory: None,
            start: false,
        } => Read::Status,
        cli::Action::Dashboard {
            directory: None,
            once: true,
            notify: false,
            notification_command: None,
            ..
        } => Read::Dashboard,
        cli::Action::Task {
            action: cli::TaskAction::ResumeStatus { id, operation },
        } => Read::ResumeStatus { id, operation },
        cli::Action::Task {
            action:
                cli::TaskAction::Resume {
                    id,
                    operation,
                    instance,
                },
        } => Read::Resume {
            id,
            operation,
            instance,
        },
        cli::Action::Task {
            action: cli::TaskAction::List,
        } => Read::List,
        cli::Action::Task {
            action: cli::TaskAction::Inspect { id },
        } => Read::Inspect(id),
        cli::Action::Task {
            action: cli::TaskAction::Result { id },
        } => Read::Result(id),
        cli::Action::Task {
            action: cli::TaskAction::Cancel { id },
        } => Read::Supervise(id, zor::tasks::supervise::Action::Cancel),
        cli::Action::Task {
            action: cli::TaskAction::Stop { id },
        } => Read::Supervise(id, zor::tasks::supervise::Action::Stop),
        cli::Action::Task {
            action: cli::TaskAction::LaunchReconcile { id },
        } => Read::Supervise(id, zor::tasks::supervise::Action::Reconcile),
        _ => anyhow::bail!(
            "this command is not yet supported for a remote machine; no local fallback was attempted"
        ),
    };
    anyhow::ensure!(
        cli.state_directory.is_none(),
        "remote commands cannot select a local --state-directory"
    );
    let catalog =
        zor::machines::Catalog::load(&zor::machines::Catalog::path(cli.machines_file.clone())?)?;
    let machine = catalog.resolve(selector)?;
    let binding = machine
        .control
        .as_ref()
        .context("machine has no control binding; use `zor machine control`")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut gateway = zor::machines::connection::Gateway::start(
        binding,
        cli.koh_binary
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("koh")),
        deadline.min(std::time::Instant::now() + std::time::Duration::from_secs(4)),
    )?;
    let client = gateway.client()?;
    let result = (|| -> anyhow::Result<serde_json::Value> {
        let capabilities = client.capabilities(deadline)?;
        anyhow::ensure!(
            capabilities.features.contains(match &read {
                Read::Dashboard => "overview-v1",
                Read::Status => "snapshot-v1",
                Read::Supervise(..) => "task-supervise-v1",
                Read::Resume { .. } => "task-resume-v1",
                Read::ResumeStatus { .. } => "task-resume-status-v1",
                _ => "task-read-v1",
            }),
            "remote service does not support this read operation"
        );
        let machine_id = machine.id.clone();
        let machine = serde_json::json!({"id":machine.id,"name":machine.name});
        if let Read::ResumeStatus { id, operation } = &read {
            let instance = &capabilities.service_instance;
            let value = client.task_resume_status(instance, id, operation, deadline)?;
            return Ok(
                serde_json::json!({"machine":machine,"service_instance":instance,"value":value}),
            );
        }
        if let Read::Resume {
            id,
            operation,
            instance: fux_instance,
        } = &read
        {
            anyhow::ensure!(
                capabilities.features.contains("task-read-v1"),
                "service lacks guarded task inspection"
            );
            let instance = &capabilities.service_instance;
            let expected = client.task_inspect(instance, id, deadline)?.expected()?;
            zor::machines::intents::record(
                &zor::machines::intents::path(&zor::machines::Catalog::path(
                    cli.machines_file.clone(),
                )?)?,
                zor::machines::intents::ResumeIntent {
                    machine: machine_id,
                    endpoint: binding.endpoint.clone(),
                    service_instance: instance.clone(),
                    operation: operation.clone(),
                    fux_instance: fux_instance.clone(),
                    expected: expected.clone(),
                },
            )?;
            let value =
                client.task_resume(instance, &expected, operation, fux_instance, deadline)?;
            return Ok(
                serde_json::json!({"machine":machine,"service_instance":instance,"operation":operation,"value":value}),
            );
        }
        if let Read::Supervise(id, action) = &read {
            anyhow::ensure!(
                capabilities.features.contains("task-read-v1"),
                "service lacks guarded task inspection"
            );
            let instance = &capabilities.service_instance;
            let inspection = client.task_inspect(instance, id, deadline)?;
            inspection.check_action(*action)?;
            let expected = inspection.expected()?;
            let value = client.task_supervise(instance, &expected, *action, deadline)?;
            return Ok(
                serde_json::json!({"machine":machine,"service_instance":instance,"value":value}),
            );
        }
        if let Read::List | Read::Inspect(_) | Read::Result(_) = &read {
            let instance = &capabilities.service_instance;
            let value = match &read {
                Read::List => serde_json::to_value(client.task_list(instance, deadline)?)?,
                Read::Inspect(id) => {
                    serde_json::to_value(client.task_inspect(instance, id, deadline)?)?
                }
                Read::Result(id) => {
                    serde_json::to_value(client.task_result(instance, id, deadline)?)?
                }
                _ => anyhow::bail!("invalid task read"),
            };
            return Ok(
                serde_json::json!({"machine":machine,"service_instance":instance,"value":value}),
            );
        }
        if matches!(read, Read::Dashboard) {
            let view = client.view(deadline)?;
            anyhow::ensure!(
                view.service_instance == capabilities.service_instance,
                "service restarted during read; refresh required"
            );
            Ok(serde_json::json!({"machine":machine,"view":view}))
        } else {
            let snapshot = client.snapshot(deadline)?;
            anyhow::ensure!(
                snapshot.service_instance == capabilities.service_instance,
                "service restarted during read; refresh required"
            );
            Ok(serde_json::json!({"machine":machine,"snapshot":snapshot}))
        }
    })();
    if result.is_err() {
        // The status writer follows the socket close. A bounded refresh may reveal its reason;
        // failure to obtain it stays unknown, never an inferred authorization verdict.
        for _ in 0..5 {
            let _ = gateway.poll();
            if gateway.latest().is_some_and(|status| {
                matches!(
                    status.state,
                    zor::machines::connection::State::Unauthorized
                        | zor::machines::connection::State::SessionExpired
                        | zor::machines::connection::State::Rejected
                        | zor::machines::connection::State::Unavailable
                )
            }) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let value = result.with_context(|| {
        format!(
            "machine {} ({}), transport {}",
            machine.name,
            machine.id,
            gateway.transport_description()
        )
    })?;
    println!("{}", serde_json::to_string(&value)?);
    Ok(0)
}
