use clap::Parser;
use std::io::IsTerminal;
use std::process::ExitCode;

mod cli;

fn main() -> ExitCode {
    match run(cli::Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("zor: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: cli::Cli) -> anyhow::Result<u8> {
    if let Some(action) = cli.action {
        return match action {
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
                    cli::WorktreeAction::Inspect { id } => {
                        zor::tasks::worktree::inspect(&root, &id)?
                    }
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
                            runtime: runtime.map(Ok).unwrap_or_else(zor::fux::runtime)?,
                            agent: Some("codex".into()),
                            integration: None,
                        },
                    )?,
                    cli::TaskAction::CodexSubmit {
                        id,
                        operation,
                        text,
                        timeout_ms,
                    } => zor::tasks::codex::state::prepare(
                        &root, &id, &operation, &text, timeout_ms,
                    )?,
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
                            runtime: runtime.map(Ok).unwrap_or_else(zor::fux::runtime)?,
                            agent: cli.agent,
                        },
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
                            runtime: runtime.map(Ok).unwrap_or_else(zor::fux::runtime)?,
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
                    cli::TaskAction::Reserve { operation } => zor::tasks::submit::run(
                        &root,
                        &operation,
                        zor::tasks::submit::Action::Reserve,
                    )?,
                    cli::TaskAction::Submit { operation } => zor::tasks::submit::run(
                        &root,
                        &operation,
                        zor::tasks::submit::Action::Submit,
                    )?,
                    cli::TaskAction::Reconcile { operation } => zor::tasks::submit::run(
                        &root,
                        &operation,
                        zor::tasks::submit::Action::Reconcile,
                    )?,
                    cli::TaskAction::Abandon { operation } => {
                        zor::tasks::abandon(&root, &operation)?
                    }
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
                            message: agent_session.zip(message).map(|(session, id)| {
                                zor::tasks::model::AgentMessage { session, id }
                            }),
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
                            zor::tasks::wait::follow(
                                &root,
                                &operation,
                                timeout_ms.unwrap_or(30000),
                            )?
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
        };
    }
    let sets = zor::rules::bundle::load_all(&cli.rules)?;
    let command = cli
        .command
        .first()
        .cloned()
        .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned()));
    let argv = if cli.command.is_empty() {
        vec!["-l".to_owned()]
    } else {
        cli.command.into_iter().skip(1).collect()
    };
    let agent = cli
        .agent
        .as_deref()
        .map(zor::osc::AgentId::new)
        .transpose()?;
    let title = match cli.title {
        cli::TitleMode::Never => zor::emit::title::Mode::Never,
        cli::TitleMode::Prefix => zor::emit::title::Mode::Prefix,
        cli::TitleMode::Replace => zor::emit::title::Mode::Replace,
    };
    if std::env::var_os("ZOR_PID").is_some() {
        zor::pty::run_transparent(&command, &argv)
    } else {
        zor::pty::run(
            &command,
            &argv,
            zor::pty::Options {
                rule_sets: sets,
                agent,
                no_osc: cli.no_osc,
                title,
                events: cli.events,
                debug: cli.debug,
            },
        )
    }
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
