# Explicit task check evidence

```sh
zor task require-check worker-1 unit-tests -- cargo test
zor task check worker-1 unit-tests-run-1 --requirement unit-tests --timeout-ms 300000 -- cargo test
zor task check-inspect unit-tests-run-1
zor task inspect worker-1
```

Checks belong entirely to zor. An explicit argv runs once in the managed task launch's recorded
cwd, without an implicit shell or PTY. The command inherits the invoking CLI environment, or the
service environment for API requests. Linked worktrees must still be ready with matching ownership;
literal cwd checks also refuse paths with retained worktree removal intent. Adopted-only tasks do
not yet support checks. New checks require an open task; retained check IDs remain readable after
task cancellation, pane exit or worktree removal.

The caller chooses a globally unique execution ID. Reusing it with identical task, argv, requirement and timeout
returns history; changed intent fails. The command never runs again under the same ID, including
after caller death. `submitted` means execution intent was synced before spawn and execution may
have begun, not that a process is known to be alive. Caller death or failed result publication can
leave this phase until the service or `task recover` reconciles it. Once all check runners have
released their process-held locks, recovery changes remaining submissions to `uncertain`, records
unavailable artifact captures, and retains their execution IDs without replay. `uncertain` also
records spawn, timeout, output-pressure or runner errors. It establishes neither absence of effects
nor child termination. See [recovery](RECOVERY.md) for admission and cleanup limits.

`finished` retains the invoked process's exit code or signal and bounded stdout/stderr. `passed`
is true for an observed zero exit, false for observed nonzero/signal exit, and null otherwise.
CLI exit zero/API completed means the operation was recorded or read; inspect `passed` for the
command outcome. A passing command is not verified task completion. Use `--source` below to bind a check to retained committed inputs. Without it, an agent
may still change files while a check runs. The check operation leaves the task outcome unchanged.

Before the first check submission, `require-check TASK NAME -- ARGV...` adds an immutable named
requirement to the managed task (at most 32). Names are scoped to the task. An identical declaration
is a read-only retry; changed argv is rejected. The first submitted check, including a diagnostic
check without a requirement, seals the policy against further additions. No removal or replacement
operation is available. `--requirement NAME` must match the declared argv exactly before execution.
To rerun after a fix, choose a new execution ID with the same requirement and command.

Task inspect's `check_policy` exposes `sealed`, `required_count`, and `passed_count`. Only the latest
submitted execution for each requirement counts, ordered by its unique journal `created_generation`.
A pending, uncertain or failing newer execution supersedes an older pass, even when the older
execution finishes later. Diagnostic checks do not satisfy requirements. These counts describe
command evidence; they do not establish artifact freshness or authorize a verified task outcome.

Timeout accepts 1 ms through five minutes, defaulting to 30 seconds in the CLI. The runner uses
a private process group, null stdin and nonblocking captured output with a 256 KiB limit per stream.
Each retained output is at most 4096 UTF-8 bytes, replacing invalid UTF-8 and marking clipped data
with `truncated`. Timeout/output/runner errors terminate the owned unreaped group and reap the
leader. Native spawn/filesystem/wait calls are not hard real-time bounds. A normally finished leader
can have detached descendants that closed captured streams and remain alive; finished is leader-exit
evidence, not proof that all spawned work stopped.

The journal lock is released during execution, so CLI inspection/cancellation and other task
operations remain available. Cancelling/stopping the task does not stop an already submitted check;
its eventual result is retained while cancellation remains in force. The service runs checks on
two dedicated workers with one shared four-request waiting queue, separate from its coordination
worker. Saturation reports task-overloaded. Queued jobs are deadline-checked before execution;
if the task was cancelled before the check starts, it is refused before command effects. Short
journal preflight/publication contention can still return task-busy. These worker limits are per
service instance; CLI checks do not consume its slots.

A started check can outlive the 15-second service reply budget; inspect the same ID later through
CLI/API. Graceful service shutdown discards queued work and waits for already active checks to
publish results, subject to command and native OS limits. Check-process cancellation and durable
asynchronous scheduling independent of client connection lifetimes remain unfinished.

At most 128 check records share the journal, with no current deletion/compaction operation. Before
spawn, each submitted check reserves 64 KiB within the four-MiB journal limit for its result,
including worst-case JSON escaping. Other transactions cannot consume that reservation. Publishing
finished or uncertain evidence releases it. Publication retries only journal contention for up to
three seconds; unsafe/corrupt state is not overwritten. Unfinished check cwd blocks owned worktree
removal even if the original agent pane closed. This does not cover detached check descendants
after an observed leader exit or process cwd changes outside the recorded directory.

Task inspect includes compact check summaries. Check-inspect returns a single check's detailed
evidence. [Explicit verification](RESULTS.md) seals selected evidence. Durable scheduling,
resolution of uncertain execution effects, and history compaction remain unfinished.


Checks can use [`--source ID`](SOURCES.md) to start from retained committed inputs in a fresh
private directory. Source IDs are part of immutable execution intent and appear in summaries.
Commands without this option still use the managed launch cwd. Passing counts do not enforce
one common revision; explicit [`task verify TASK SOURCE`](RESULTS.md) enforces that requirement
and seals the selected policy evidence.


Source-bound checks may request [artifact capture](ARTIFACTS.md) with `--artifact NAME=ID`.
Captured bytes and per-requirement problems publish atomically with command evidence. Their IDs
and storage capacity are reserved before execution. Capture failure does not change the observed
command exit: inspect artifact_problems and result blockers separately from passed. Execution
errors retain a bounded cause chain to explain failures such as an unavailable executable.
