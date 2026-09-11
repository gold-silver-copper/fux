# Observation contract, version 1

zor observes agent activity and emits presentation data. It does not provide transport admission,
remote access, workspace management, or proof of which process produced terminal bytes.

## OSC reports

The protocol-only library is `zor::osc`. `PROTOCOL_VERSION` is 1. Producers emit:

```text
ESC ] 7877;v=1;state=blocked;agent=example;seq=12;visible=blocker;exited=0 ESC \
```

`v`, `state`, and `seq` are required. Unknown versions, missing versions, duplicate known fields,
invalid enum values, invalid percent escapes, and invalid UTF-8 are rejected. Unknown extension
fields within version 1 are ignored. The state is `working`, `blocked`, `idle`, or `none`; an agent
identifier is required for non-`none` states and forbidden for `none`. Sequence numbers are u64
values scoped to one producer lifetime; consumers must not treat them as globally unique IDs.

Agent identifiers are limited to 64 ASCII letters, digits, dots, underscores, or hyphens. Optional
messages are at most 128 decoded UTF-8 bytes, percent-encoded on the wire. The complete encoded
report, including optional OSC framing and unknown fields, is limited to 1024 bytes. `parse`
accepts a complete BEL/ST-terminated OSC or the joined payload delivered by a terminal parser.
`format` emits ST termination. Terminal parsers also need bounded buffering before calling `parse`.

## JSON Lines

The optional event output uses the same `v: 1` schema marker. `t` selects `state`, `agent`, or `exit`;
records retain their documented timestamps, agent, sequence, visible flags, and lifecycle fields.
`ts` is Unix time in seconds (a floating-point value). State records contain `state`, `seq`, and
optional `previous`, `agent`, `pid`, `code`, `title`, `visible`, and `exited` fields. `visible` lists
currently detected `idle`, `blocker`, and/or `working` evidence; it can differ from the interpreted
state while a transition settles. `exited` denotes an observed agent lifecycle exit, not termination
of the wrapper. Agent records announce a detected agent/PID or `agent: null` when none remains.
Exit records contain the wrapped command's final code, using 128 plus signal number for signals.
Unknown fields may be ignored within version 1; consumers must reject unsupported versions.
Every record is one JSON object plus a newline and is limited to 2048 encoded bytes. Oversized
records are rejected. A stalled event sink drops bounded pending records rather than blocking the
wrapped terminal stream. Events may be lost; they are observations, not an authoritative audit log.

## Trust and consumers

Any pane process can forge OSC reports. Successful parsing establishes schema validity only, not
agent identity or authenticity. zor displays this as observed/self-reported state and owns its
agent-specific interpretation rules. Consumers must not authorize access based on these reports.

Without zor, panes continue normally. Invalid or unsupported reports are ignored by the consumer.
Observer/rule/sink failures must preserve byte forwarding, terminal queries, resize, signals,
exit status, and child cleanup. The `zor wrap` passthrough integration suite (feature `wrap`) covers these contracts.


## Local multiplexer observer

`zor observe --socket PATH --pane ID --pid PID` observes a pane already owned by fux. Optional global `--rules DIR` and `--agent ID` arguments may precede `observe`. The observer does not spawn or signal the pane command and does not own its PTY.

The adapter reads bounded newline-delimited JSON `list` and `capture` responses through the local control socket and verifies the kernel peer UID. Replies have a 1 MiB cap and a two-second absolute read deadline. Captures request at most 128 KiB and are sampled every 100 ms. Pane identity is checked against both its ID and original PID on every sample; removal or replacement ends observation. The initial listing pins the server incarnation,
and every later listing and capture includes that precondition; a restarted server cannot
reuse a pane/PID to continue an old observation. Title, progress, and dimensions are read as bounded data from the coherent capture.

Detection and the existing state machine remain in zor. Reports use the OSC v1 schema above, one report per newline on stdout. A consumer must apply a report-size limit before parsing, reject malformed output, and keep observer backpressure independent of pane input/output. Closing or killing the observer must not terminate the observed command.

The fux control adapter sends and verifies the four-byte `FUX\n` preface before each RPC. A mismatch or stalled preface ends that sampling attempt without sending a command. Preface reads use an absolute two-second deadline. This is an independent wire consumer, not a fux library dependency.

Listing revisions are cache-invalidation hints. Capture (`format:"cells"`, `max_bytes`
131072) supplies coherent cells, dimensions, cursor, title and progress from fux's own grid;
zor expands the run-length cells into rows without emulating a terminal, and never subtracts
borders or joins an old listing's geometry with newer cells. Unchanged revisions (`if_revision`)
skip capture. Truncated captures clear the observation to
`none` without stopping the observer; later complete captures restore detection. No report is ingested or displayed as agent
state by fux; zor and its consumers own agent presentation.

## Automatic observation

`zor watch` discovers fux's manager and scans its workspaces, emitting one JSON snapshot per
iteration. `--once` performs one scan; `--runtime /absolute/path/to/fux` overrides the runtime
directory containing `manager.sock`. The default follows fux's XDG/macOS path rules. It does not
create/adopt managed tasks, signal panes, or change focus. This foreground command is the
foreground watcher. The separate [zor service API](SERVICE-API.md) shares this registry through
`zor serve` and `zor status`; `zor status --start` starts it in the background when absent.
The separate [task model](TASKS.md) owns persistence and prompt correlation; passive observations
never satisfy prompt waits merely because a screen looks idle.

Snapshots contain `observations`, explicit `removed` identities, per-workspace/manager `problems`,
`scan_duration_ms`, `event_streams` and `event_failures`. The last two count currently subscribed
workspaces and cumulative event/setup failures in this observer lifetime; `--once` reports zero
for both because it does not subscribe. A removal means observation was lost or the pane is absent, not that a
process exited successfully. A recovered/recreated identity cannot inherit a previous verdict.
Handles include server incarnation, workspace name and stream, pane ID, and root PID; caches also
check detected foreground PID, selected agent and capture revision. Discovery pins one server
incarnation within a scan; a later scan may rediscover a replacement and reports removed old handles.

Each observation reports capture revision, controller input sequence, selected agent, detected
PID, matched rule, raw observed state, a bounded diagnostic, and `age_upper_bound_ms`. That age is
a conservative upper bound since the capture revision was checked during the current scan; it
excludes downstream output/consumer buffering. Unchanged captures reuse a prior verdict only
after the listing confirms its revision and process/agent identity. These are raw observations,
not hysteresis-filtered task states, prompt replies, or completion evidence. No matched rule means
`unknown`, consistent with the evaluator’s unknown fallback. Inaccessible or unsupported
foreground processes remain unknown. Explicit `--agent` chooses an interpretation for this watch
only; it grants no lifecycle ownership.

The registry holds at most 128 panes. Each scan shares a two-second socket I/O budget, and workspace
order rotates so an unreachable early workspace does not permanently starve later workspaces.
Budget/capacity exhaustion is reported and unobserved identities are removed from the current
snapshot. Continuous watch and the service retain up to 64 same-user fux subscriptions,
using each listing's incarnation and cursor as the replay boundary. Unfiltered events must
advance by exactly one sequence in the same stream. EOF, gaps, duplicates, reordered cursors,
malformed or oversized frames discard continuity and require a fresh listing and
subscription. An event kind this zor does not know is ignored, not a failure: its cursor still
counts toward continuity, so fux may add event kinds without breaking observation. They never infer an agent state or task outcome. The service marks affected
observations unknown while refreshing and preserves their original evidence ages. Failure
counts survive successful resynchronization; a new observer lifetime resets them.

Events wake a full scan after a 100 ms coalescing window anchored to the first dirty event;
continuous output cannot move that deadline forward. A fallback scan is due three seconds
from scan start for silent foreground-process changes and new workspace discovery. Discovery
and subscription admission rotate separately, and reads rotate among streams. Even if setup
exhausts the rescan interval, the observer performs one bounded nonblocking event drain before
starting another scan. An event currently invalidates every cached observation in its workspace,
so unchanged siblings can be recaptured. Quiet fallback scans and unchanged other workspaces
reuse capture revisions. Selective pane refresh and broader scaling measurements remain open.

Listing/capture use the existing two-second RPC budget; discovery plus subscription setup
share a four-second budget. Publication age includes both stages. Missing subscriptions or
incomplete workspace scans make affected observations unknown. The client limits each event
frame to 64 KiB (a stricter limit than fux's generic 1 MiB frame ceiling), buffered bytes to
64 KiB plus one 8 KiB read per stream, and each drain round to 256 KiB of new reads and 32
parse/read iterations per stream. Native JSON allocation is additionally bounded by these
frame limits; this is not a fixed total RSS claim. Polls check stop/reload at most every 100 ms
between bounded rounds. Native process inspection, filesystem calls and parsing are not hard
real-time guarantees. An unread watch stdout can pause this optional process; eventual fux
subscription overflow/disconnection then requires resynchronization without affecting fux.

`tests/verify/zor_events.py` uses real fux and zor through a disposable counting/fault proxy.
It checks quiet capture reuse and periodic discovery, event-triggered refresh before the fallback,
input-sequence updates, disconnect/gap and duplicate-cursor recovery, preserved pane identity,
and SIGHUP reload. Gap/duplicate replies are injected by the fixture, not claims about actual
fux event loss. Unit tests cover fragmentation, size bounds, identity/order, buffered replay,
interruption, and draining a healthy stream after repeated stalled subscription setup. These
are local correctness checks, not a herdr comparison or a broad latency/scaling benchmark.

The shared fux client bounds connect, protocol negotiation, and response reads, checks peer UID,
and rejects incompatible control prefaces. On macOS process arguments/environment come from native
inspection; there is no subprocess `ps` fallback. Inaccessible native environment data therefore
cannot supply an override. Built-in rules cover only the recorded Codex sign-in, Claude theme-selection and OpenCode startup input screens. See
[fixture provenance](tests/fixtures/agents/README.md) for versions and missing coverage; custom
synthetic rules prove protocol behavior, not support for a real agent release.


## Rule configuration and reload

Built-in rules load first. External rules load from `$XDG_CONFIG_HOME/zor/rules`, falling back to `$HOME/.config/zor/rules` when XDG
is absent or relative. Explicit `--rules` directories apply afterward in command-line order;
TOML files within each directory apply in sorted filename order. A later set with the same agent
ID replaces the entire earlier set. `zor agents` emits JSON describing the sets actually loaded,
including process names and rule counts; it does not claim those rules have real-agent coverage.

Send SIGHUP to a running `zor watch` to reload the collection. Zor validates all candidate files
before publishing them. On failure it keeps the prior collection and generation, reports a `rules`
problem in snapshots, and continues observing. On success it increments `rules_generation`, clears
that problem and invalidates cached verdicts even when terminal revisions are unchanged. Publish
individual rule file updates with atomic rename to avoid transient partial-file validation errors.
The signal handler only sets a flag; loading occurs in the observer loop. The registration is
removed when that loop returns.

Agent IDs follow the OSC identifier contract. Aliases and effective process names are bounded to
64 entries per list and 256 bytes per name without controls; rule IDs are bounded to 128 bytes.
Existing file-count, file-size, regex and gate-complexity limits still apply. Broad built-in
agent coverage remains unfinished; the active Codex, Claude and OpenCode rules establish only their documented startup coverage.

Unmatched screens evaluate to `unknown`, never implicit idle. Both `zor wrap` and the single-pane
observer clear an earlier state immediately by publishing OSC `state=none` (without an agent
field, as required by that wire schema). Internal process identity is retained, and a subsequent
matched rule can recover without rediscovery. Unknown cancels pending idle confirmation;
heartbeats cannot preserve a previous blocked/working/idle verdict. `watch` reports `unknown`
with its separate handle and agent fields. An explicitly matched `skip` rule retains its existing
transcript-view semantics and is distinct from missing evidence.

These are passive observation semantics. Managed native integrations publish separate
zor-owned heartbeat claims; [the dashboard](DASHBOARD.md) correlates them with the passive
snapshot's pane/process identity and input sequence. Configured native evidence takes
precedence, with missing, stale or mismatched claims producing unknown. The raw `watch`,
`status` and service snapshot interfaces remain passive. Current state and retained prompt
reports have separate lifetimes; neither establishes verified task completion.

The terminal boundary filter tracks valid UTF-8 across chunks: continuation bytes are not C1
control introducers or terminators, and an incomplete character is not an injection boundary.
Malformed UTF-8 prefixes do not hide following C1 controls. All encoded string payload bytes
still count against the control-string bound. A small parser adapter completes split characters
before the normal bulk path to avoid vte 0.15's partial-character lookahead dropping following
text; it retains only UTF-8 state, not an additional output buffer.
