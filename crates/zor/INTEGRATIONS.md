# Agent integrations and evidence

Agent hooks, prompt correlation, adapter lifetime, and report policy belong to zor.
Fux supplies terminal input receipts and pane/process identity. An application hook
does not make a task verified; verification still requires the task's declared checks.

## OpenCode 1.18.29

`tests/fixtures/agents/opencode-1.18.29/events.json` records the real OpenCode TUI in a
real fux pane on Darwin, driven by a local deterministic provider. Two fixed prompts
were submitted through distinct fux input operations in one OpenCode session. Both
receipts reached delivered, with input sequences 1 and 2 and complete byte counts.
The event probe is instrumentation, not an installed zor integration.

| Observed boundary | Evidence | Consequence for the adapter |
|---|---|---|
| User input consumed | Two `chat.message` calls with distinct `output.message.id`; neither supplies `input.messageID` | Bind using the output message identity and its session |
| Response ancestry | Assistant updates name the consumed user message in `parentID` | A later event must retain its original prompt binding |
| Intermediate completion | Second prompt has two distinct assistant IDs with completed time and `finish: stop` | Those fields alone cannot identify the final response |
| Tool continuation | First of those messages owns a completed read of the disposable fixture; another model request follows | Track tool-bearing messages and continuation |
| Final text | Later message contains `FIXTURE RESPONSE`, has no tool part, and precedes session idle | This fixture demonstrates one final response path, not a universal completion rule |

The tagged [prompt loop](https://github.com/anomalyco/opencode/blob/v1.18.29/packages/opencode/src/session/prompt.ts)
explicitly permits tool continuation even when a provider returns stop. It invokes
`chat.message` before saving the user message, so the hook is not proof of durable
application acceptance. The tagged [plugin dispatcher](https://github.com/anomalyco/opencode/blob/v1.18.29/packages/opencode/src/plugin/index.ts)
awaits message hooks but invokes event hooks without awaiting their returned promises.
A production adapter therefore needs bounded, ordered reporting of its own; an async
event callback does not supply ordering or durable delivery automatically.

Reproduce with a trusted OpenCode binary and a new output directory:

```sh
python3 zor/tools/capture_opencode_events.py --fux target/debug/fux \
  --opencode /absolute/path/to/opencode --output /tmp/new-opencode-events
python3 zor/tools/test_opencode_events.py
```

The harness uses temporary HOME/XDG/configuration, explicit OpenCode configuration,
an environment allowlist, a loopback mock provider, and only the read tool on an owned
fixture. It disables the default plugins, model-list fetching, and auto-update through
OpenCode configuration flags. This is not an OS network or credential-store sandbox.
Each observation is bounded to 30 seconds; provider I/O has three-second inactivity
timeouts and request/event size caps. Those are not an absolute whole-process deadline.
Only task-owned processes are stopped. Validated publication requires zero fux exit,
provider-thread shutdown, exact consumed prompts, final delivered receipts, and the
correlated tool/text evidence. Failure leaves a diagnostic when writable. Binary,
harness, and instrumentation hashes are recorded; temporary paths are redacted.

The local provider uses fixed text and a deliberately unusual tool-call stop reason.
It is not a real model, authenticated cloud request, approval prompt, passive detector
fixture, or zor end-to-end task. No readiness manifest was broadened by this capture.
The validator's mutated traces test rejection of old-parent responses, missing or
unrelated final text, absent final continuation, failed reads, and intervening/partial
input. They are synthetic negative tests, not additional real-agent executions.

## Managed OpenCode adapter

`zor task start ... --integration opencode -- /path/to/opencode` installs the bundled
zor plugin for that launch. The real command runs directly in fux's PTY. A transparent
exec step appends the plugin to inherited `OPENCODE_CONFIG_CONTENT`; it preserves its
other properties and plugins and does not edit configuration files. Inline configuration
must be a JSON object of at most 131072 bytes, with an array-valued `plugin` when present;
JSONC inline configuration is currently unsupported. `OPENCODE_CONFIG` and
`OPENCODE_CONFIG_DIR` remain inherited.

The plugin creates a private local socket and registers a random producer lifetime
against the managed launch marker and live pane identity. Registration is asynchronous:
`start` may return before it completes. `task inspect` exposes
`launch.integration.producer`; prompt submission refuses until registration succeeds.
Before typing, zor persists the prompt's producer/input-operation arm, delivers that
specific operation/token/text to the plugin, and requires an exact acknowledgement.
A lost acknowledgement can retry the identical unconsumed arm and input operation.
A changed or already consumed arm is rejected. Tokens never enter terminal input.

After clean adapter disposal, a newly instantiated plugin can bind the same endpoint and
register a new random producer through the existing `register-adapter` CLI/service action.
Registration verifies the same pinned live fux target before and after a same-user endpoint
hello naming the candidate, within one two-second I/O budget. It releases the journal lock
during I/O and rechecks launch, target and registration identity before the atomic handoff.
A competing registration or stop requires retry; endpoint availability alone cannot replace
the worker or change its target. Same-producer retries return retained registration without
refreshing time. A retired producer cannot register again.

`launch.integration.retired` retains up to16 prior producer IDs with registration/retirement
timestamps. At that bound, new registrations fail explicitly; there is no silent recycling.
Old bindings and reports remain immutable and their exact retries remain readable. New
binding/report events and input submissions for retired arms are rejected. Nonterminal unresolved
armed prompts become Uncertain and continue excluding other writers until explicitly abandoned;
existing terminal waits retain their recorded outcomes.
`task wait` preserves this uncertainty. Inspect the receipt, then use `task abandon OPERATION`
when releasing coordination is intended. Registration never replays input, clears the task,
restarts its process or assumes that accepted input stopped executing.

The new producer starts with no native prompt ancestry and no borrowed heartbeat freshness.
Its next managed prompt establishes a new binding. Historical reports can still resolve
their original waits. Retired unsent arms need no RPC to the replacement; abandonment
does not fabricate a disarm acknowledgement. The replacement endpoint rejects requests
naming the retired producer. Heartbeat files remain one per launch and the new pulse replaces the
retired lifetime's retained pulse only after normal identity/input checks.

The plugin never unlinks an existing socket. Clean disposal/reinstantiation is supported;
a stale socket after abrupt failure still requires explicit diagnosis, and this does not
add an OpenCode reload command or restore a worker after fux/process loss. Producer IDs,
endpoint hello and local journal capabilities are not authentication against same-user code.

`zor task adapter-status TASK` (service action `adapter-status` with `id`) probes the
registered endpoint without arming, sending input, replacing the producer, or changing
the journal. It verifies the pinned fux server/workspace stream/pane/PID, requires a
same-user protocol-v1 hello from the recorded producer, and verifies the target again.
Both target reads and hello share a two-second I/O budget. This is endpoint availability
at the recorded observation time; `reachable` does not mean the agent is idle, prompt-capable,
or healthy, and does not refresh prompt evidence or establish a heartbeat.

| Availability | Meaning |
|---|---|
| `not-configured` | The retained task has no managed integration |
| `not-registered` | An attached integration has not registered its producer yet |
| `inactive` | The managed launch is not attached or has a recorded stop request |
| `reachable` | Pinned target checks and the matching hello succeeded |
| `unavailable` | A target or adapter check failed; `stage` and bounded `problem` identify it |
| `changed` | The journal changed during the probe; retry for a new sample |

Replies include producer/registration identity, initial/current journal generations,
start/end timestamps and monotonic duration. The operation releases the journal lock
during I/O; any concurrent generation change makes the result inconclusive, even when
it concerns another task. Journal contention or invalid storage is an operation error.
The two-second I/O budget does not bound filesystem calls or OS scheduling. A cancelled
task can still have a reachable adapter because coordination cancellation does not stop
its process. Retained `task result` integration summaries say `availability: not-probed`;
registration time is historical identity evidence, not current liveness.

The bundled OpenCode adapter separately sends `heartbeat-adapter` every two seconds,
with at most one bounded CLI child outstanding. Busy/failed pulses are skipped; the next
tick uses a newer heartbeat sequence. This sequence is independent of native binding/report
order. Disposal stops the timer and awaits the outstanding bounded child. Pulses continue
while a root question or permission blocks the agent; freshness does not imply readiness.

CLI form: `zor task heartbeat-adapter TASK --marker LAUNCH_MARKER --producer PRODUCER
--sequence NUMBER`. A newer pulse requires the registered lifetime, an attached launch
without stop intent, and a matching live fux target. Target I/O has a two-second budget
and releases the journal lock; launch identity, stop intent and sequence are rechecked
before recording. Same-sequence retry returns the original pulse without refreshing it;
older sequences fail. A historical exact retry can succeed without a live endpoint and
does not constitute a new liveness sample.

Heartbeats occupy separate private atomic files under the state directory's `heartbeats/`,
one file of at most1024 bytes per retained managed launch (at most128). They do not rewrite
the task journal, change its generation, or alter prompts, waits, claims or verification.
The journal generation therefore cannot be used as a cache key for heartbeat freshness;
clients must evaluate a new heartbeat view when they need current age.
Symlinks, hardlinks, oversized files and invalid identities are rejected. Corrupt heartbeat
files remain intact and yield an unavailable freshness view; retained task results remain
readable. Files remain bounded by retained launch history; there is no heartbeat history log.

Both `adapter-status` and `task result` include a separate `heartbeat` view with sequence,
receipt time, age, six-second TTL and `scope: producer-freshness-only`. Status is current,
expired, never-seen, inactive, unavailable, clock-rollback or clock-changed. Age uses the
larger of wall and monotonic elapsed time; negative elapsed time or more than one second
of disagreement prevents a current verdict. Linux uses BOOTTIME; macOS uses MONOTONIC.
Clock-reset detection is conservative, not a guaranteed machine-boot identity mechanism.
The endpoint probe independently checks the fux incarnation; a retained freshness view
alone does not prove that a worker still exists. Only a newer pulse refreshes the record.

These timestamps describe receipt of a producer claim. They are not authenticated against
same-user code, and do not replace passive detection or immutable prompt-response evidence.
Clean producer replacement follows the registration contract above; abrupt endpoint recovery
and native event-gap recovery remain unfinished.

A pulse may also carry `--observation JSON`, with `state` (`unknown`, `working`,
`blocked`, or `idle`), prompt `operation`, `input_operation`, and native `message`
(`session`, `id`). Zor requires the retained managed prompt binding, registered producer,
matching receipt and current generic fux input sequence. It rechecks journal identity after
target I/O. Same-sequence retry must contain the identical observation; it never refreshes
age. The pulse records the receipt's `input_sequence` alongside this optional claim.
Receipt time is sampled before I/O so probe delay consumes freshness too.

The adapter maintains current state separately from the immutable prompt report. Bound
root assistant activity can show working; a question or permission for its latest known
assistant can show blocked. A reply clears that blocker to unknown. A validated current
plain response can show idle, including after an earlier NeedsInput report, without
rewriting that report or verifying the task. Unarmed root input clears current ancestry.
Late binding callbacks cannot restore older state or discard the newer current assistant;
late message/part mutations invalidate idle. Async message reads cannot overwrite a newer
observation epoch. These are native-hook observations; event loss and same-user forgery
are not eliminated by a heartbeat.
Later terminal input, including an interactive answer, invalidates the old input correlation;
continued native state then remains unknown until a new managed input establishes a binding.
The synthetic reply/continuation test holds the generic input sequence unchanged and does
not establish automatic re-correlation after a human terminal answer.

`zor dashboard` and `dashboard --once` merge these claims with passive pane identity and
input evidence. A configured integration takes precedence; absent, expired, invalid or
input-mismatched native evidence becomes unknown, even when passive rules say idle.
Unconfigured panes retain passive detection. Both the producer's six-second TTL and the
dashboard's five-second evidence bound apply. Evidence includes producer, sequence, claim,
freshness, correlation and the secondary passive state. Raw `status`, `watch` and service
`snapshot` remain passive; `overview` supplies the bounded integration records used by
the dashboard. A retained `task result` pulse is not re-correlated to later live input:
its `current` status describes producer receipt age only. Use the merged dashboard view
for the current state assessment. All of this state and policy belongs to zor.

The root `chat.message` hook snapshots the arm before awaiting native session lookup.
It requires the exact single text part, consumes the arm, and calls `bind-report` with
that prompt's native session and user-message ID. Repeated text does not select a prompt.
Subsequent reports retain this immutable ancestry. Root permissions associated with a
known assistant message report `needs-input`. Root `question.asked` events use the same
session and assistant-message ancestry to report `needs-input`; questions without a known
tool message, child-session questions and already-reported prompts produce no new claim.
The tagged [question tool](https://github.com/anomalyco/opencode/blob/v1.18.29/packages/opencode/src/tool/question.ts)
supplies the native tool message/call identity, and the
[question service](https://github.com/anomalyco/opencode/blob/v1.18.29/packages/opencode/src/question/index.ts)
publishes the request before awaiting an answer. Zor does not answer or dismiss it.
A response requires session idle plus
the latest matching completed stop message and a fresh, bounded native-message read
showing nonempty text, no error/summary and no tool/subtask. Stop or silence alone is
insufficient. Native message reads handle tool parts arriving before message updates
and text that is later removed. These reports are attributed observations, not proof
of durable application processing or verified task success.

The adapter bounds connections to eight, request bytes to 131072, queued jobs to 32,
arm/native-message history to 1024, and active assistant records to 256. Socket requests
have a two-second timer. Successful native API bodies are limited to 131072 bytes with
a two-second abort signal; CLI output is limited to 65536 bytes and each child gets a
6.5-second timer. Busy/not-ready retries use a ten-second admission window (a final
call can extend past it). These budgets do not guarantee OS scheduling or process reaping.
The native SDK controls its own error-response parsing. Node is needed to run the adapter
unit test and combined development gate; the production plugin uses OpenCode's runtime.

`task abandon OP` can retire an unsent arm without restarting the managed agent. Zor
persists `arm.input_started` in the Submitting transaction before any input-submit RPC;
status reconciliation never clears it. Abandonment commits released/cancelled coordination
first. Only an arm with `input_started: false` is eligible: the initial retirement verifies
the live target and a Reserved receipt, then persists `disarm_requested` before sending an
exact disarm request. A matching acknowledgement records `disarmed`. Wrong/lost replies
leave cancellation and retirement intent durable; retry `abandon` for the same operation.
Recorded intent retries do not need the receipt to remain available, but still require the
pinned live target and producer. Completed retries are retained reads. `reconcile` returns
the frozen unsent proof once retirement intent exists, not a fresh receipt observation.

The adapter keeps a bounded tombstone even if the original arm never arrived. Delayed arm
retries and chat callbacks cannot reactivate it, and an old disarm retry cannot clear a
newer arm. Retirement does not cancel the fux reservation or remove terminal text. Reserved
is point-in-time evidence, not protection against an independent controller submitting the
same fux operation. Zor permanently disables further input for that released prompt.
Possibly submitted arms are never retired, including when reconciliation later says Reserved.
A receipt lost before the initial proof, a changed target or a lost adapter can still prevent
retirement. In that case the old unconsumed arm continues to block later arms.

Clean producer replacement can fence a lost arm under the registration contract above.
Abrupt socket recovery and safe history recycling remain unfinished. Diagnose a stale endpoint
or explicitly stop/start an owned task when necessary; never blindly replay an uncertain prompt.
Content-addressed plugin assets and
stale socket paths have no automatic garbage collection yet. Registration uses a private
same-user endpoint and a launch marker, not authentication against other same-user code.
Child sessions cannot consume root arms; child permissions/questions, questions without
tool ancestry, transformed
input/attachments, structured-output completion, and broader agent/version coverage are
unsupported. Event loss can prevent a report; no completion is inferred from that loss.

Reproduce automatic registration, unsent arm retirement, plain response, tool continuation,
and a root permission request with a trusted OpenCode 1.18.29 binary:

```sh
python3 zor/tools/capture_opencode_integration.py --fux target/debug/fux \
  --zor zor/target/debug/zor --opencode /absolute/path/to/opencode \
  --output /tmp/new-opencode-integration
node zor/tools/test_opencode_adapter.mjs
```

Use `--blocker question --output /tmp/new-opencode-question` to exercise a root question
instead of the final permission request. `tests/fixtures/agents/opencode-1.18.29/question.json`
records this separate three-prompt run with the real TUI and local synthetic provider.
Its evidence pins the matching user message, assistant message, tool call, exact question
and options, running tool state, delivered input, `needs-input` report and visible question UI.
Before cleanup there is no reply/rejection event or completed question tool. The trace
records an explicit pre-cleanup event boundary and retains later shutdown events;
stopping OpenCode can reject its pending question and end the tool with an error.
No answer is submitted;
the task remains open. This establishes blocker reporting, not answering, continued
execution after an answer or producer recovery.
The current question trace also records successful `adapter-status` probes after
registration and while the question is visible. Both report `reachable`, illustrating
that endpoint availability is independent of the pending `needs-input` prompt claim.
Its final heartbeat carries the bound root blocker and input sequence; the dashboard
shows the same fresh, correlated native claim while the task remains Open. Both owned
services exit cleanly. This real trace proves the unanswered blocker path; synthetic
adapter tests separately cover subsequent reply/activity/idle transitions and callback races.

`tests/fixtures/agents/opencode-1.18.29/integration.json` retains the reviewed three-prompt
run following an injected pre-submit crash boundary. `cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- verify-opencode-integration`
checks its evidence and adversarial
mutations. This harness uses the same disposable configuration and local synthetic provider approach
as the event probe. It retains an independent probe plugin in inherited inline config,
exercises real zor prepare/submit/wait operations, and records native bindings, receipts,
reports, blocker UI and cleanup. Before input, it injects arm intent into its private
journal, obtains a real adapter acknowledgement without journaling that acknowledgement,
then uses real `task abandon` to retire it. A late arm retry is rejected and input stays zero;
all three subsequent prompts use the same live OpenCode process. This is a synthetic durable
crash point, not an actual killed caller or a demonstrated OS crash. No permission is approved.
The task stays open.
The adapter unit test supplies synthetic ordering, truncation, ancestry, repeated-input,
permission/question and socket faults. Evidence mutation tests reject foreign question
sessions/messages/calls, absent ancestry, altered question text, missing UI/running tool,
answered/rejected questions, completed tools and unbound reports.
The required real-fux binding fixture separately injects
wrong and dropped arm acknowledgements, proving no input before an exact acknowledgement.
It also covers unsent retirement, lost/wrong disarm replies, retained proof retries, and the
monotonic attempted-input barrier. The Node tests cover retirement tombstones and delayed
chat callbacks. Dropped network replies and delayed hook faults use synthetic adapters/events;
the real OpenCode run covers the explicitly injected durable crash point above.
Neither fixture establishes authenticated model quality, broad passive detection coverage,
abrupt producer restart recovery, or general superiority over another product.

`--reload-adapter --blocker question` adds a separate, test-only clean plugin reload after
the first response. `tests/fixtures/agents/opencode-1.18.29/producer-reload.json` records
the real OpenCode SDK and hooks across two producers, unchanged pinned target and hook PID,
no input during reload, preserved first report, and subsequent tool-response/question bindings
under the replacement. Instrumentation wraps the plugin factory and delegates its native hooks;
it adds no production reload endpoint. The native agent uses the same synthetic local provider.
This demonstrates controlled plugin disposal/reinstantiation, not upstream automatic hot reload,
unanswered-prompt reconstruction or abrupt crash recovery. `tests/verify/zor_producers.py`
separately runs the bundled plugin with a synthetic SDK in one real fux-owned Node process;
it covers pending and unsent arms, writer exclusion, explicit abandonment, retired event/input
rejection, new heartbeat ancestry, preserved historical reports, live endpoint collision and
bounded lifetime retention. Both fixtures leave the task Open.
