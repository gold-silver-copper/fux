> zor now lives in the fux repository as `crates/zor` (one workspace, one CI and gate with fux).
> The standalone repository at https://github.com/gold-silver-copper/zor is historical.

# zor

`zor` owns agent observation and task coordination over fux's generic multiplexer API.
Its library can parse and format the OSC 7877 wire protocol without default features or the
CLI runtime. The PTY wrapper that publishes observed agent state without fux is the
`zor wrap <command>` subcommand behind the off-by-default `wrap` Cargo feature
(a default `cargo install zor` includes it; fux builds zor with `--no-default-features --features cli`, which has no `wrap` subcommand).

[Durable prompt groups](GROUPS.md) coordinate bounded admission and verified dependencies
across existing managed tasks through explicit steps or opt-in `task group-run` service advancement. Agent orchestration
remains entirely in zor; fux supplies generic pane and input operations.

Managed OpenCode integrations publish bounded producer heartbeats. `task adapter-status`
and `task result` show their freshness separately from endpoint availability, agent readiness
and task verification; see [integrations](INTEGRATIONS.md).
`zor dashboard` and `dashboard --once` combine native agent state with current pane/input
evidence, preserve unknown state after integration loss, and keep task outcomes separate.
Raw `status` and `watch` remain passive observations.
Clean adapter reloads can register a replacement producer on the same live pane, preserving
old reports and leaving unresolved input uncertain until explicit reconciliation.

## Install

```sh
cargo install zor
```

## Usage

```text
zor --rules <dir> …                     # later rule sets replace earlier ids
zor --agent <id> …                      # force one rule set
zor check <fixture.txt> [--agent id]
zor agents
zor serve [--directory path] [--runtime path]  # shared foreground observation service
zor status [--start] [--directory path]       # read, optionally starting the shared service
zor shutdown [--directory path]              # stop zor, preserving fux panes
zor dashboard [--once] [--bell] [--notify] [--directory path] # task/group attention and agent evidence
zor watch [--once] [--runtime path]     # discover fux panes, emit JSON observations
```

The local [service API](SERVICE-API.md) shares observations across controllers without taking
ownership of fux panes. Continuous observation uses generic fux events with bounded coalescing,
explicit resynchronization and periodic discovery for silent changes. The [dashboard](DASHBOARD.md) runs as an ordinary terminal application,
with a JSON mode, evidence details, attention filtering and generic pane focus. `zor status --start`
starts the service in the background when absent;
`zor shutdown` stops only zor. The [durable task CLI](TASKS.md) supports managed command launch, adoption, durable prompt preparation,
receipt-backed line submission, read-only reconciliation, and explicit coordination cancellation. Single-check and bounded blocking waits support explicit prompt-scoped reports and
process-exit evidence. Managed prompts can bind native message ancestry to their input receipt;
subsequent reports must match that binding. The [service API](SERVICE-API.md) exposes the durable one-shot task operations.
The optional `task start --integration opencode` adapter registers and binds prompts automatically.
Use `task adapter-status TASK` for a read-only probe of its registered endpoint and pinned pane;
retained result summaries explicitly leave availability unprobed.
The [integration evidence](INTEGRATIONS.md) describes response, permission and question waits tested with real
OpenCode 1.18.29 through fux using a local synthetic provider, plus configuration and recovery limits.
Broader agent coverage and completed task workflows remain unfinished.
The [worktree CLI](WORKTREES.md) creates owned git checkouts with durable intent and recovery.

Bundled rules recognize recorded Codex 0.153.4 sign-in/trust, working, input-ready and command-approval
screens; Claude Code 2.1.263 theme-selection, working and input-ready manual-mode screens; and OpenCode 1.18.29 startup
input, permission and question screens. Coverage is limited to the recorded layouts and modes.
Unmatched screens are unknown; no task completion is inferred. See the
[coverage inventory](tests/fixtures/agents/README.md) before relying on detection.

External input is bounded: zor loads at most 256 rule files, each at most 1 MiB, and accepts
fixtures up to 4 MiB.

```sh
cargo run -- agents
cargo run -- wrap -- your-command   # PTY wrapper (feature `wrap`, on by default)
```

See `DESIGN.md` for the architecture and full protocol contract.

## Protocol-only use

```toml
zor = { version = "=0.1.0", default-features = false }
```

The versioned wire and trust contract is documented in [OBSERVATION-CONTRACT.md](OBSERVATION-CONTRACT.md).

The protocol-only surface is `zor::osc::{AgentId, Flags, Report, State, format, parse}`.

## License

MIT

`zor task adapter-capabilities --agent opencode` (also `claude` or `codex`) is a
read-only JSON contract for implemented adapter guarantees. The service equivalent is
`{"action":"adapter-capabilities","agent":"codex"}` in a normal task request.
It does not probe executables, accounts, native sessions or journal state. Unknown
agents fail with `unsupported-agent`; known unavailable operations have explicit reasons.
Use `task adapter-status ID` for an OpenCode managed endpoint diagnostic.

OpenCode exposes managed native correlation/state/response and conditional native-session
recreation. Recreation requires an open task with a reconciled lost/finished attempt,
original-process absence and retained session/storage identity; stop intent prevents it.
Claude exposes generic managed launch and passive discovery/state fallback. OpenCode and
Claude do not implement native turn interruption. No adapter state establishes verified
task completion.

Codex has a managed app-server stdio adapter:

- `task codex-start ID --title TITLE --instance INSTANCE --workspace WORKSPACE --cwd DIR -- EXECUTABLE app-server --stdio`
  launches a persistent native thread inside an owned fux pane with approval prompts disabled.
- `task codex-submit ID --operation OP --text TEXT` retains literal input and native
  correlation. `task codex-inspect OP` reads retained evidence, without probing availability.
  Its `evidence_age_ms` measures time since accepted correlated evidence. Evidence loses
  freshness after five seconds, worker retirement, or a backwards clock change; completed
  output remains historical. Duplicate events and interrupt acknowledgements do not renew it.
- `task codex-reconcile OP --request REQUEST` reads native history after missing events.
  Reusing identical input or control intent never grants a duplicate send or extends its deadline.
- `task codex-interrupt OP --request REQUEST` interrupts the correlated native turn;
  acknowledgement is separate from observed interruption.
- `task codex-recreate OP --request REQUEST` stops the provider and resumes the same thread
  and materialized storage under the live owned wrapper. It does not restart a lost wrapper
  or replay input. Empty threads without written rollout storage cannot be recreated.

These controls are also available through the task service API. Coordination cancellation,
native interruption, owned-worker stop and task verification remain separate operations.
Native input-required evidence reports a blocker; answering native requests is not implemented.
The existing dashboard consumes a bounded native summary for the current task attempt.
It joins the exact pane identity and requires both recent native evidence and a current
pane observation. Blockers request attention; expired or invalidated evidence shows
unknown. Native response state never changes the recorded task outcome.
Account-free fixtures exercise production logic and real-fux lifecycle behavior. Installed
initialization/storage metadata were checked separately; live model turns and materialized
real-provider resume remain unverified.
