use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Internal headless native worker; requires verified managed fux ownership.
    #[command(hide = true)]
    CodexWorker {
        #[arg(long)]
        task: String,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Internal transparent exec used by managed OpenCode integration (no wrapper PTY).
    #[command(hide = true)]
    OpencodeExec {
        #[arg(long)]
        plugin: PathBuf,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Zor task attention and agent evidence, displayed as an ordinary terminal application.
    Dashboard {
        #[arg(long)]
        directory: Option<PathBuf>,
        #[arg(long)]
        once: bool,
        /// Ring the terminal bell when new attention entries appear.
        #[arg(long)]
        bell: bool,
        /// Send desktop notifications for fresh attention transitions.
        #[arg(long, conflicts_with = "once")]
        notify: bool,
        /// Custom notifier executable, receiving title and body as separate arguments.
        #[arg(long, requires = "notify")]
        notification_command: Option<PathBuf>,
    },
    /// Zor-owned git worktrees.
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },
    /// Durable task records; adoption and preparation never send terminal input.
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// Run zor's shared local observation service in the foreground.
    Serve {
        #[arg(long)]
        runtime: Option<PathBuf>,
        /// Private zor service directory, distinct from fux's runtime.
        #[arg(long)]
        directory: Option<PathBuf>,
        #[arg(long, hide = true)]
        daemon_child: bool,
    },
    /// Read the shared service snapshot without starting a new observer.
    Status {
        #[arg(long)]
        directory: Option<PathBuf>,
        /// Start zor's background observation service if no service is listening.
        #[arg(long)]
        start: bool,
    },
    /// Stop zor's observation service, leaving fux and pane processes running.
    Shutdown {
        #[arg(long)]
        directory: Option<PathBuf>,
    },
    /// Discover and passively observe all local fux workspaces as JSON snapshots.
    Watch {
        /// fux runtime directory containing manager.sock (defaults to fux's standard path).
        #[arg(long)]
        runtime: Option<PathBuf>,
        #[arg(long)]
        once: bool,
    },
    Check {
        fixture: PathBuf,
        #[arg(long)]
        agent: Option<String>,
    },
    Agents,
    /// Observe an existing local fux pane without owning its command.
    Observe {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        pane: u32,
        #[arg(long)]
        pid: u32,
    },
    /// Any other first word is a program to run in a pseudoterminal with its observed agent
    /// state published as OSC 7877 (`zor claude`, `zor -- status`); see the top-level flags.
    #[command(external_subcommand)]
    Command(Vec<String>),
}

#[cfg(feature = "wrap")]
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum TitleMode {
    Never,
    #[default]
    Prefix,
    Replace,
}

#[derive(Debug, Subcommand)]
pub enum WorktreeAction {
    /// Remove an owned checkout; force permits dirty files, never active managed launches.
    Remove {
        id: String,
        #[arg(long)]
        force: bool,
    },
    Create {
        id: String,
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        branch: String,
        #[arg(long, default_value = "HEAD")]
        base: String,
    },
    List,
    Inspect {
        id: String,
    },
    Reconcile {
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TaskAction {
    /// Coordinate prepared prompts for existing managed tasks, admitting up to concurrency.
    GroupCreate {
        id: String,
        #[arg(long, required = true)]
        operation: Vec<String>,
        #[arg(long)]
        after: Vec<String>,
        #[arg(long, default_value_t = 2)]
        concurrency: usize,
    },
    GroupInspect {
        id: String,
    },
    GroupList,
    /// Admit or reconcile one group operation using its retained input identity.
    GroupStep {
        id: String,
    },
    /// Enable durable service advancement, starting zor's service when absent.
    GroupRun {
        id: String,
        #[arg(long)]
        directory: Option<PathBuf>,
    },
    /// Stop selecting automatic steps; an already-selected submission may finish.
    GroupPause {
        id: String,
    },
    /// Cancel group coordination; does not retract input, stop workers or remove worktrees.
    GroupCancel {
        id: String,
    },
    /// Resume one already-recorded managed stop request; never create cleanup intent.
    Recover {
        #[arg(long)]
        after: Option<String>,
    },
    /// Prepare a durable prompt with pinned verified predecessor evidence; submit separately.
    Handoff {
        id: String,
        #[arg(long)]
        operation: String,
        #[arg(long = "from", required = true)]
        predecessors: Vec<String>,
        #[arg(long)]
        text: String,
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
    },
    /// Seal required check/artifact evidence for one source and finalize the task outcome.
    Verify {
        id: String,
        source: String,
    },
    /// Retain bounded committed Git file bytes for isolated check input.
    SourceCollect {
        id: String,
        source: String,
        #[arg(long, default_value = "HEAD")]
        revision: String,
    },
    SourceInspect {
        source: String,
    },
    SourceFile {
        source: String,
        path: String,
    },
    /// Retain committed and working-tree changed-file evidence.
    ChangesCollect {
        id: String,
        changes: String,
    },
    ChangesInspect {
        changes: String,
    },
    /// Assemble retained output, artifact and check evidence without inferring success.
    Result {
        id: String,
    },
    /// Require a named relative artifact path before any collection or check submission.
    RequireArtifact {
        id: String,
        artifact: String,
        path: String,
    },
    /// Retain exact bytes of a bounded file beneath the managed task cwd.
    ArtifactCollect {
        id: String,
        artifact: String,
        path: String,
        #[arg(long)]
        requirement: Option<String>,
    },
    ArtifactInspect {
        artifact: String,
    },
    /// Require this exact check command; configure before any task check is submitted.
    RequireCheck {
        id: String,
        check: String,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Run an explicit check once and retain its evidence independently of task success.
    Check {
        id: String,
        check: String,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
        #[arg(long)]
        requirement: Option<String>,
        #[arg(long)]
        source: Option<String>,
        /// Capture a required artifact after command exit; repeat NAME=ID up to eight times.
        #[arg(long = "artifact", value_name = "NAME=ID")]
        artifacts: Vec<String>,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    CheckInspect {
        check: String,
    },
    /// Launch an app-server command as a managed headless native Codex worker.
    CodexStart {
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        instance: String,
        #[arg(long)]
        workspace: String,
        #[arg(
            long,
            required_unless_present = "worktree",
            conflicts_with = "worktree"
        )]
        cwd: Option<PathBuf>,
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        runtime: Option<PathBuf>,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Queue native literal input durably; only the owning worker may send it.
    CodexSubmit {
        id: String,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        text: String,
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
    },
    CodexInspect {
        operation: String,
    },
    /// Reconcile native history by identity; never resubmit input.
    CodexReconcile {
        operation: String,
        #[arg(long)]
        request: String,
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
    },
    /// Recreate the owned native process with the same persisted thread; never replay input.
    CodexRecreate {
        operation: String,
        #[arg(long)]
        request: String,
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
    },
    /// Request native turn interruption, independently of task cancellation.
    CodexInterrupt {
        operation: String,
        #[arg(long)]
        request: String,
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
    },
    /// Launch a command in a fux-owned pane with durable zor ownership intent.
    Start {
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long, value_parser = ["opencode"])]
        integration: Option<String>,
        /// Noninteractive single-prompt policy; argv must be EXECUTABLE PROMPT.
        #[arg(long, conflicts_with = "integration")]
        headless_agent: Option<String>,
        #[arg(long)]
        instance: String,
        #[arg(long)]
        workspace: String,
        #[arg(
            long,
            required_unless_present = "worktree",
            conflicts_with = "worktree"
        )]
        cwd: Option<PathBuf>,
        /// Launch in a ready worktree owned by this zor state directory.
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        runtime: Option<PathBuf>,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Explicitly resume a native OpenCode session as a new attempt; never replay a prompt.
    Resume {
        id: String,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        instance: String,
    },
    /// Find an ambiguously created pane; never create a replacement.
    LaunchReconcile {
        id: String,
    },
    Adopt {
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        instance: String,
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        pane: u32,
        #[arg(long)]
        runtime: Option<PathBuf>,
    },
    List,
    Inspect {
        id: String,
    },
    Prepare {
        id: String,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        text: String,
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
    },
    /// Reserve input without writing bytes.
    Reserve {
        operation: String,
    },
    /// Submit using the durable reservation; delivery does not imply completion.
    Submit {
        operation: String,
    },
    /// Inspect receipt evidence without submitting input.
    Reconcile {
        operation: String,
    },
    /// Release prompt coordination; queued input may still arrive.
    Abandon {
        operation: String,
    },
    /// Cancel task coordination without stopping its adopted pane or retracting input.
    Cancel {
        id: String,
    },
    /// Cancel coordination and close only the pane owned by this managed launch.
    Stop {
        id: String,
    },
    /// Probe registered adapter and pane identity without sending input or changing task state.
    AdapterStatus {
        id: String,
    },
    /// Read implemented adapter guarantees without probing providers or changing state.
    AdapterCapabilities {
        #[arg(long)]
        agent: String,
    },
    /// Record a registered producer heartbeat without changing prompt or task evidence.
    HeartbeatAdapter {
        id: String,
        #[arg(long)]
        marker: String,
        #[arg(long)]
        producer: String,
        #[arg(long)]
        sequence: u64,
        #[arg(long)]
        observation: Option<String>,
    },
    /// Register the producer of an explicitly configured managed integration.
    RegisterAdapter {
        id: String,
        #[arg(long)]
        marker: String,
        #[arg(long)]
        producer: String,
    },
    /// Bind a managed prompt receipt to a native agent message. Never sends input.
    BindReport {
        operation: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        producer: String,
        #[arg(long)]
        sequence: u64,
        #[arg(long)]
        input_operation: u64,
        #[arg(long)]
        agent_session: String,
        #[arg(long)]
        message: String,
    },
    /// Record a prompt-scoped integration claim; not proof of task success.
    Report {
        operation: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        producer: String,
        #[arg(long)]
        sequence: u64,
        #[arg(long)]
        input_operation: u64,
        #[arg(long, value_enum)]
        kind: ReportKind,
        #[arg(long, requires = "message")]
        agent_session: Option<String>,
        #[arg(long, requires = "agent_session")]
        message: Option<String>,
    },
    /// Check response/exit evidence without sending input.
    Wait {
        operation: String,
        /// Poll until an outcome, uncertainty or the caller deadline.
        #[arg(long)]
        follow: bool,
        /// Caller budget for --follow (default 30000 ms); separate from prompt deadline.
        #[arg(long, requires = "follow")]
        timeout_ms: Option<u64>,
    },
    DiscardPrepared {
        operation: String,
    },
    Forget {
        id: String,
    },
}

#[derive(Clone, Debug, ValueEnum)]
pub enum ReportKind {
    NeedsInput,
    ResponseObserved,
}

/// `zor [FLAGS] <program> [args...]` wraps a program; `zor [FLAGS] <subcommand> ...` runs one
/// of zor's own commands; `zor [FLAGS] -- <program> [args...]` always wraps, even when the
/// program is named like a subcommand; bare `zor` wraps `$SHELL -l`.
#[derive(Debug, Parser)]
#[command(version, about, trailing_var_arg = true, subcommand_required = false)]
pub struct Cli {
    /// Private durable task state (defaults to XDG_STATE_HOME/zor or HOME/.local/state/zor).
    #[arg(long)]
    pub state_directory: Option<PathBuf>,
    #[arg(long)]
    pub rules: Vec<PathBuf>,
    #[arg(long)]
    pub agent: Option<String>,
    /// Wrapper: also write event lines to a unix socket or fifo (`-` selects fd 3).
    #[cfg(feature = "wrap")]
    #[arg(long)]
    pub events: Option<PathBuf>,
    /// Wrapper: how to touch the wrapped program's OSC 0/2 window title.
    #[cfg(feature = "wrap")]
    #[arg(long, value_enum, default_value_t = TitleMode::Prefix)]
    pub title: TitleMode,
    /// Wrapper: never emit the state OSC; title updates only.
    #[cfg(feature = "wrap")]
    #[arg(long)]
    pub no_osc: bool,
    /// Wrapper: dump matched rules and machine events to stderr.
    #[cfg(feature = "wrap")]
    #[arg(long)]
    pub debug: bool,
    #[command(subcommand)]
    pub action: Option<Action>,
}

impl Cli {
    /// Parses the process arguments. A standalone `--` before the first non-flag token forces
    /// everything after it to be the wrapped program, so `zor -- status` wraps a program named
    /// `status` instead of running zor's `status` command.
    pub fn parse_args() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let mut index = 1;
        while let Some(arg) = args.get(index) {
            if arg == "--" {
                let mut cli = Self::parse_from(args.iter().take(index));
                let command: Vec<String> = args.iter().skip(index + 1).cloned().collect();
                if cli.action.is_none() && !command.is_empty() {
                    cli.action = Some(Action::Command(command));
                }
                return cli;
            }
            if !arg.starts_with('-') {
                break;
            }
            // Long flags that take a value consume the next token unless written as --flag=value.
            let takes_value = matches!(
                arg.as_str(),
                "--state-directory" | "--rules" | "--agent" | "--events" | "--title"
            );
            index += if takes_value { 2 } else { 1 };
        }
        Self::parse()
    }
}
