# Durable task records, version 1

Zor now records agent sessions, tasks, attempts and prepared prompt operations independently of
terminal observation. Receipt-backed line submission is available. A single-check wait API accepts explicit prompt-scoped response claims and observed process exit.
A bounded `--follow` mode is also available. The bundled OpenCode integration supplies native prompt bindings; broader real-agent coverage remains unfinished. The [service API](SERVICE-API.md) exposes
the same one-shot task operations.
Managed command launch and explicit retained-source policy verification are available.

[`task handoff`](HANDOFFS.md) prepares a receipt-backed prompt containing pinned verified
predecessor evidence for another task. Delivery and acknowledgment remain separate from
the receiving task's verification.

[Prompt groups](GROUPS.md) add durable bounded admission and verified prerequisites for
prepared prompts on existing managed tasks. Controllers explicitly call `group-step`;
all group policy stays in zor.

```sh
zor task adopt review-1 --title 'Review parser changes' \
  --instance FUX_INSTANCE --workspace default --pane 1
zor task list
zor task inspect review-1
zor task prepare review-1 --operation prompt-1 --text 'Review parser changes' --timeout-ms 30000
zor task reserve prompt-1       # optional; writes nothing
zor task submit prompt-1
zor task reconcile prompt-1     # receipt lookup only
zor task wait prompt-1          # one bounded evidence check; may return pending
zor task wait prompt-1 --follow --timeout-ms 30000
zor task abandon prompt-1       # explicitly release coordination; input is not retracted
zor task cancel review-1        # cancel task coordination; adopted pane keeps running
```

Use `zor status --start` or fux's listing API to discover the target first. Adoption requires an
explicit fux incarnation; it validates that incarnation and reads the live workspace stream and
pane process identity through fux's API. `--runtime` on adopt selects a different fux runtime.
A global `--agent ID` is an operator-supplied label, not detection evidence. A pane can disappear
after the listing; the stored handle records what was adopted, not continuing proof of liveness.
Submission revalidates it before input. Root PID identity does not prove which foreground
program will consume input; adapter-specific process/reaction correlation remains unfinished.

All commands emit JSON and use the same bounded journal. They can run as CLI transactions or through the service task worker. Commands needing only stored records
continue to work without the fux or zor service. Busy storage returns an error instead of waiting
indefinitely; retry the same operation ID.

## Identity and ownership

A session identifies a target by canonical fux runtime path, server incarnation, workspace,
workspace stream, pane and root PID. Tasks describe logical work; an attempt associates one task
with one session. Tasks adopting the same target and agent label reuse the session, while keeping
separate task and attempt IDs. `ownership: adopted` grants no right to terminate the pane, remove
its directory or clean a worktree. Adoption never reuses a managed session.

The runtime path selects a socket route. Prompt writer exclusion, group admission and
closed-target worktree checks compare the pinned incarnation/workspace/stream/pane/PID
independently of that route, so aliases cannot bypass coordination.

## Managed launch

[Explicit checks](CHECKS.md) retain command evidence for a managed task without equating a passing
command with verified task completion. Declare named requirements before the first check;
reruns use new execution IDs. Task inspection includes compact check summaries and counts
only the latest submitted execution for each required command. [Source snapshots](SOURCES.md) bind
optional committed input to isolated check directories. Checks can atomically capture required
artifacts from those directories with their command evidence. [`task verify TASK SOURCE`](RESULTS.md)
seals a common source and the latest passing required checks with their required artifacts.

[Artifact collection](ARTIFACTS.md) retains bounded exact file bytes under stable IDs. Task
inspection exposes compact artifact metadata; artifact-inspect returns the retained bytes. Declare
named required paths before the first artifact collection or check submission. Explicitly associate
collections using `--requirement`; result reads distinguish missing and retained required outputs.

[`task result`](RESULTS.md) assembles retained task, output, check, artifact and prompt evidence
with explicit blockers and limitations. It does not infer verified completion or refresh evidence.

[`task changes-collect`](CHANGES.md) retains committed and index/worktree changed-file evidence;
result reads include its latest observation. Repeated status samples do not establish immutable source.

Use `--worktree ID` instead of `--cwd PATH` to launch in a ready zor-owned worktree and retain
its association with this task's launch/session/attempt. See [WORKTREES.md](WORKTREES.md) for
validation and retry semantics. Exactly one location selector is required.

```sh
zor task start worker-1 --title 'Worker' --instance FUX_INSTANCE \
  --workspace default --cwd /absolute/project -- /path/to/agent
zor task launch-reconcile worker-1
```

For automatic OpenCode hooks, add `--integration opencode` before `--`:

```sh
zor task start worker-1 --title 'Worker' --instance FUX_INSTANCE \
  --workspace default --cwd /absolute/project --integration opencode -- /path/to/opencode
```

The optional integration is part of immutable launch intent. It installs a private zor-owned
plugin through a transparent exec step without adding a wrapper PTY or editing personal
configuration. `start` does not wait for plugin registration; inspect
`launch.integration.producer` before submitting. Configured prompts refuse submission until
registration and an exact prompt-arm acknowledgement succeed. `task register-adapter TASK
--marker LAUNCH_MARKER --producer PRODUCER` is the plugin's registration operation; exact retries
return the retained profile, and a different producer cannot replace it. The full setup,
configuration limits, evidence policy and recovery limitations are in [INTEGRATIONS.md](INTEGRATIONS.md).

Zor records launch intent before asking fux to create a pane. The command runs in a fux-owned
PTY through `/usr/bin/env -- ZOR_LAUNCH_ID=<random marker> ...`; env executes the supplied
argv without a zor wrapper PTY. Empty positional arguments are preserved; the executable must
be non-empty and all arguments remain subject to the existing count/byte/NUL bounds. The request pins both server and workspace lifetime. Successful
live reconciliation records `ownership: managed`, a task, and an attempt. The marker is recovery
metadata, not authentication or proof of agent state.

Retry the same task ID and identical arguments after a lost reply. Once creation may have been
sent, neither `start` nor `launch-reconcile` submits it again. Reconciliation searches for the
unique live pane with the recorded command, cwd and lifetime. If it already closed, zor uses the
returned pane ID or a unique retained creation event after the launch cursor to retrieve final
evidence from the manager. It checks the recorded command, cwd, workspace stream and pane.
`phase: closed` records a finished attempt, optional observed exit code, input sequence and up to
4096 UTF-8 bytes of final output with an explicit truncation flag. A historical PID is null when
no live PID was observed. The task remains open; even exit zero does not establish verified success.
Finished attempts reject prompt preparation.

Missing/expired final evidence, replay gaps, ambiguous creation events and restarted servers
remain uncertain. A known pane ID can recover after workspace deletion; without that ID, lost
workspace event history currently prevents recovery. Explicit resolution of failed or pending
launch records remains unfinished; do not use a new ID as an automatic retry.
`inspect` and `list` expose pending launch records even before a task exists.
For an already attached session, explicit `launch-reconcile` validates its live handle or reads
matching final evidence. Missing live/final evidence sets the attempt to uncertain and records a
launch problem while retaining ownership. Live recovery clears this lifecycle problem. Closure
finishes the existing attempt and retains the originally observed PID. Session/task identities,
task outcome (including cancellation), and prompt receipts/waits are preserved; use prompt wait
or receipt reconciliation separately. Identical `start` retries and `inspect` remain stored reads.
General automatic lifecycle reconciliation remains unfinished; recorded stop intents have
[bounded service recovery](RECOVERY.md).

Cancellation currently releases coordination only, including for managed tasks; it does not
terminate processes or remove resources. Use `zor task stop TASK` to cancel coordination and
close a reconciled managed launch's pane. Adoption grants no stop authority. Zor commits
`stop_requested: true` and cancellation (or preserves an existing Verified outcome) before requesting closure, validates the original handle,
and never substitutes another pane after a restart or observation failure. Success requires
confirmed final evidence (`phase: closed`); errors may follow accepted stop intent or closure.
Inspect and retry the same ID until confirmed. Interrupted callers leave that intent recoverable;
the service resumes pending recorded stops, or `task recover` attempts one explicitly. Prompt
receipt/wait history is retained; verified resolved waits remain resolved. Stop cannot retract
input already queued or processed.

Stop uses fux's pane-close behavior, including process-group teardown; it does not ask the agent
to finish its task or save files. It removes no directories or worktrees. Ambiguous launches need
ownership reconciliation before stop. Managed history cannot yet be forgotten. Automatic
resume, worktree cleanup, agent invocation templates and one-command discovery remain unfinished.

Task and prompt IDs are caller-provided 1–64 ASCII letters, digits, hyphens or underscores.
Repeating an identical retained adoption or prompt preparation returns the original identities
and timestamps without another commit. Conflicting intent under the same ID fails. Adoption retries compare the stored original absolute runtime spelling and retained request
before filesystem discovery. They work even if that runtime directory has disappeared. Only a
new adoption resolves the canonical runtime and requires a live pane listing. Another spelling
of the runtime path counts as different intent under the same task ID.

Preparation records text, attempt, creation/deadline, `delivery: prepared`, no receipt, and a
pending wait. **It writes no terminal input.** The deadline is a stored Unix-millisecond intent,
not an active timer or a promise to submit later. Timeouts are 1 ms through 24 hours. Text is
nonempty UTF-8 without control characters; its fux-escaped representation plus Enter must fit
64 KiB. Literal backslashes are escaped, and one carriage return is appended. Multiline text
requires a future explicit adapter paste mode and is rejected. Preparation never runs
automatically when a service restarts.

Only one pending or delivery-uncertain prompt may target the same pane identity, across tasks
and agent labels. The journal lock and validation enforce that invariant. Human input remains
possible through fux; this reservation is only zor's coordination intent, not exclusive PTY
ownership. Delivery, observed reaction, wait outcome and task outcome are separate concepts.
No idle/blocked observation, zero process exit or delivered receipt makes a task verified.
Only explicit `task verify` can produce a verified outcome. Loading a verified task without its
valid sealed evidence selection is rejected. This outcome describes the declared verification
policy; it does not imply agent termination or semantic correctness.

`discard-prepared` only cancels unsent prepared intent with no receipt. Its ID remains retained
and a retry does not reactivate it. `forget` removes a task's records only when it has no prompts,
or all its prompts are discarded and never acquired a receipt. Shared sessions remain while
referenced by another attempt. Forgetting an absent task is harmless. These operations never
signal processes or delete pane/worktree directories. IDs may be reused after explicit forgetting;
idempotency guarantees apply only while the corresponding records remain retained.

## Submission and receipts

`reserve OP` records a fux reservation without sending input. `submit OP` reserves when necessary,
syncs that receipt and then a submitting phase before calling fux input-submit. Both operate on
an already prepared immutable prompt. Each invocation holds the shared journal lock, uses a
six-second total RPC budget and revalidates server/workspace/pane/root-PID identity before input.
Submission rechecks the wall-clock deadline after durable intent and caps its RPC budget to the
remaining lifetime. This bounds the caller; input already accepted by fux may finish afterward.
Native storage operations do not have a hard time bound. A backward clock reading refuses input.

`reconcile OP` only looks up receipts or returns retained evidence; it never reserves or submits. A retained operation is always
reused. Losing a reservation reply is safe to retry because reservation alone cannot type bytes.
Losing a submission reply retains the operation and records uncertain delivery. Missing/expired
receipts, unavailable fux, and a changed incarnation fail visibly with retained uncertainty;
no command silently allocates a replacement. Known delivered/failed evidence and the frozen unsent proof of a recorded adapter retirement
are returned from storage even after server receipt expiry. A failed or partially written input remains exclusive
and is not retried as a fresh operation. Explicit abandonment can release coordination while
retaining uncertain history.

Fux retains receipts for 60 seconds from reservation, scoped to its server incarnation. Its
monotonic expiry timestamp is evidence, not a Unix timestamp. Queued input may outlive receipt
retention. Delivered proves only the reported byte count reached the PTY writer/flush boundary;
it does not establish application processing or a fresh response. Successful delivery leaves
`wait: pending`. No durable exactly-once application guarantee is made.

Human input between reservation and submission causes fux to reject the older reservation.
Zor keeps its operation and exposes the conflict; it never silently creates another reservation.
Input after submission can still weaken later response correlation. Separate zor state directories
have separate coordination locks; use one shared state directory for cooperating writers.

## Prompt-scoped reports and wait checks

Preparation generates a random 128-bit `report_token`, retained with the immutable prompt.
An explicit integration can report one response event using its prompt ID, that token, the fux
input operation from the accepted receipt, a producer lifetime ID and a nonzero producer sequence:

```sh
zor task report prompt-1 --token PREPARED_REPORT_TOKEN \
  --producer ADAPTER_LIFETIME_ID --sequence 1 --input-operation FUX_INPUT_OPERATION \
  --kind response-observed
zor task wait prompt-1
```

The other report kind is `needs-input`. These are **integration claims**, not authenticated proof
of processing, current agent state or task success. Tokens distinguish prompt lifetimes and
prevent an old report from accidentally satisfying a new prompt, including after caller ID reuse.
They are visible in the private journal/JSON API and are not authentication against another process
running as the same user. The producer ID is currently caller-asserted provenance; adapters must
use a new ID when their producer restarts. Configured OpenCode launches register one current
producer lifetime and require native binding. [Clean producer replacement](INTEGRATIONS.md)
retains up to16 prior lifetimes, fences old input/events and preserves unresolved uncertainty.
[Separate heartbeat observations](INTEGRATIONS.md) carry expiring current state for the
dashboard without rewriting these reports. Native event-stream gap recovery remains unfinished.

A report is accepted only after zor has recorded queued/delivered input for that exact operation,
while coordination is active and before the prompt's Unix-millisecond deadline. An integration
that races receipt persistence must retry the same event after reconciliation. One immutable
response event is retained per prompt. Exact retries return it without refreshing its receipt time;
conflicting producer, sequence, operation or kind fails. This is a one-event contract, not a
replay stream or current-state heartbeat. The report token is never automatically injected into
terminal input. The configured OpenCode adapter receives the specific token/operation over its
private endpoint before input submission; other adapters must arrange their own delivery.

### Native message bindings for managed prompts

An integration that has already acquired the exact prompt token and input operation can
record the native user message consumed by its application:

```sh
zor task bind-report prompt-1 --token PREPARED_REPORT_TOKEN \
  --producer ADAPTER_LIFETIME_ID --sequence 1 --input-operation FUX_INPUT_OPERATION \
  --agent-session NATIVE_SESSION --message NATIVE_USER_MESSAGE
zor task report prompt-1 --token PREPARED_REPORT_TOKEN \
  --producer ADAPTER_LIFETIME_ID --sequence 2 --input-operation FUX_INPUT_OPERATION \
  --agent-session NATIVE_SESSION --message NATIVE_USER_MESSAGE --kind response-observed
```

`bind-report` requires an open task with an attached managed launch and an unresolved,
possibly submitted prompt. It reads the retained fux operation's current status and verifies
the pinned server/workspace/pane/PID and input sequence, then atomically records the binding
and refreshed receipt. This handles a durable Submitting/Uncertain record after a lost submit
reply without submitting or reserving input. Unsent, failed, missing/expired receipts, adopted
panes, intervening input, cancelled/resolved coordination and expired prompt deadlines cannot
acquire a new binding. The fux checks share a four-second budget under the journal lock; native
filesystem calls are not a hard wall-clock guarantee. Busy callers retry the same identity.

The binding stores producer, sequence, input operation, native session/message, and bound
time. It inherits its immutable managed target through the prompt's attempt/session. Native
IDs and producer IDs use the existing 1–64 ASCII letter/digit/hyphen/underscore grammar. A
native message cannot bind two retained prompts in the same managed/native session, even
under a new producer ID. Each producer is confined to one managed/native session; each new
binding or bound response must advance its retained producer sequence. Do not mix unbound
claims into a producer lifetime used for bindings. A restart needs a fresh producer ID; it
cannot rebind an already bound prompt. History remains bounded by the 1,024-prompt/four-MiB
journal limits and is removed only through the existing explicit task-forgetting policy.
Forgetting ends this retention guarantee; it is not permanent native-message deduplication.

After binding, reports must supply the exact native session/message, producer and input
operation, with a later producer sequence. Omitting ancestry or substituting an old message
fails. An unbound prompt rejects reports that supply native ancestry. Exact binding/report
retries return their original immutable records, even after later events, release, deadline
or process exit; they do not refresh evidence or require a live fux endpoint. Journal loading
revalidates binding relationships and rejects conflicting native identities or sequences.

This remains attributed application evidence. A caller must obtain and retain the specific
prompt identity before delivery, rather than resolving a delayed hook against the current
prompt or matching repeated text. Automatic token delivery/arming and real-agent hook wiring
are not implemented by this operation. A native message ID alone proves neither processing
nor final response; adapters must handle tool continuation and errors. Human input can race
binding publication, so `wait` still verifies the current input boundary independently.

`wait OP` performs one bounded check and returns the prompt record as JSON. It never submits
input, sleeps awaiting a response, spawns an observer, or treats terminal text as completion.
Callers may poll a pending result. The journal admits one active transaction; concurrent callers
receive a busy error and retry, and no retained waiter queue is allocated. Receipt reconciliation
has a six-second RPC budget followed by at most four seconds for live/final evidence. Native
filesystem operations are not hard-time-bounded. The service exposes the single check; a shared blocking waiter API remains unfinished.

A semantic report satisfies this check only after PTY delivery is known and a current capture has
the receipt's input sequence, with matching server/workspace/pane/root-PID identity. Ordinary human
input after submission weakens correlation and produces `uncertain`, even if a report exists.
A fast response needs no intermediate working state. Old idle/blocked text, redraws, silence and
reports scoped to another prompt cannot satisfy the check. Passive screen adapters are not yet
connected to semantic waits.

If live evidence is unavailable, a matching retained fux final record can establish `process-exited`
and retain its exit code in `wait_exit_status`. Missing/expired final evidence or closure without an
observed exit status stays uncertain. Process exit is distinct from response and verified success;
a matching final record can prove exit even if receipt reconciliation failed. It does not resolve
ambiguous delivery. Final screen/artifact collection remains unfinished.

Without a response or exit, deadline expiry yields `timed-out`; input already accepted may still
finish. Timeout retains writer exclusion until explicit abandonment. Observation errors yield
`uncertain` plus a bounded `wait_problem`; later checks may reconcile recoverable uncertainty.
A backward clock reading refuses semantic evaluation. Known unsent preparation/reservation is an
error, including when reconciliation restores an uncertain operation to reserved.

`needs-input`, `response-observed`, `process-exited`, `timed-out` and `cancelled` are retained
historical wait outcomes. Later receipt refreshes cannot erase them. They do not describe the
agent's current screen or verify the task. Abandonment/task cancellation can explicitly replace
coordination with cancelled while retaining its report and delivery/exit history. Task outcomes
remain open until a separate cancellation or future result policy changes them.

### Blocking CLI waits

`wait OP --follow [--timeout-ms N]` polls until a terminal wait result, uncertainty, or its caller
budget expires. The caller budget defaults to 30 seconds and accepts 1 ms through 24 hours;
`--timeout-ms` requires `--follow`. It is separate from the prompt deadline. The JSON envelope is
`{"stop":"terminal|uncertain|caller-deadline","prompt":{...}}`. The prompt is the last successfully
observed record and may lag current state when a busy journal prevents a final read. Neither caller
timeout nor a killed waiter cancels the prompt or turns it into a task timeout.

At most 32 follow callers are admitted per state directory using owned/private empty regular
`waiter-N.lock` files and nonblocking flock. These files are retained; they must not be manually
unlinked while callers run. The OS releases locks on return or process death. Admission requires
brief journal access and may return busy; the CLI does not create an unbounded admission queue.
Once admitted, journal contention is retried within the caller budget. Other storage errors fail
visibly. Separate state directories have separate limits.

Polling sleeps up to 250 ms and never holds the journal lock while sleeping. Receipt and live/final
RPC deadlines are capped to the caller budget. Exhausting that budget during an RPC returns
`caller-deadline` without downgrading prompt/delivery state because of that timeout. Independently
observed failures can still yield uncertainty. Existing native filesystem/time-bound qualifications
apply; an active evidence transaction may briefly make cancellation/report writers return busy.
A caller must retry those busy writes. Cancellation is then observed at a subsequent check.

The waiter pins attempt, creation time and report token, including across check/reconciliation
lock boundaries. It refuses changed or missing identities rather than following a reused caller ID.
This mode polls local contracts; shared observation coalescing, service waiters and measured
multi-pane overhead remain unfinished.

## Explicit cancellation

For a configured OpenCode arm that this journal never attempted to submit, `abandon` also
tries safe retirement after committing cancellation. It verifies a live Reserved receipt,
persists retirement intent, and requires the adapter's exact acknowledgement. Errors may follow
successful cancellation; retry the same `abandon` to finish retirement. Once intent is recorded,
retries retain that unsent proof without a new receipt lookup. They still check the live target;
completed retirement is an offline retained read. `arm.input_started` is monotonic and persisted
before input-submit, so a later Reserved receipt never authorizes retirement of possibly in-flight
input. Retirement neither cancels a fux reservation nor retracts typed text. See
[INTEGRATIONS.md](INTEGRATIONS.md) for limits and tombstone behavior.

`abandon OP` marks prompt coordination released and its wait cancelled. It preserves submitted
and partial-write evidence, text and deadline. Eligible unsent adapter retirement may refine
an earlier arming failure to the proven Reserved receipt. This explicitly
allows a new prompt for the target: queued bytes from the old operation may still arrive, so the
operator must account for the terminal's current state before submitting new input. Abandonment
never sends Ctrl-C, closes a pane, cancels a fux input operation or claims task completion.

`cancel TASK` atomically marks an open task cancelled and releases all its prompt coordination.
It leaves the adopted session and attempt records intact, never signals its process, and refuses
new prompt preparation for that task. Existing prepared IDs replay their retained cancelled state;
they never reactivate. Cancelling an already cancelled task and abandoning an already released
prompt without pending adapter retirement return retained state without a new journal generation. Other final task outcomes are not
overwritten by cancellation.

Reserve and submit reject cancelled/released prompt IDs even if a receipt already says delivered.
Read-only reconciliation remains available and preserves released/cancelled coordination through
receipt refreshes or lookup failures; recorded retirement instead returns its frozen unsent proof.
Writer exclusion applies until explicit release, then permits
another prompt without allowing a late reconciliation to reacquire the target. All callers share
the journal lock, so cancellation and submission are serialized; cancellation does not interrupt
an in-flight holder of that lock, and a busy caller must retry.

History with any reservation or submission remains in the journal after abandonment/cancellation;
`forget` still refuses to erase it. A cancelled task with no receipt history can be forgotten.
Bounded history compaction and managed-resource cancellation remain unfinished.

## Private storage and recovery

The default directory is `$XDG_STATE_HOME/zor`, falling back to `$HOME/.local/state/zor` when
XDG is absent or relative. `zor --state-directory /absolute/path task ...` selects a separate
store. This is distinct from zor's runtime socket directory and is shared by task CLI processes
using that state directory. The service [recovers](RECOVERY.md) abandoned check submissions
and recorded managed stops from this store; other automatic task reconciliation remains unfinished.

The directory must be owned and private (0700); journal and lock files must be private owned
regular files with a single hard link. Symlinks, FIFOs, unsafe permissions, malformed data,
unsupported versions, duplicate record IDs and invalid relationships are rejected rather than
silently reset. A persistent `journal.lock` uses nonblocking exclusive flock; it is never removed
to avoid splitting concurrent callers across different lock inodes.

A mutation clones and validates the current bounded state, syncs the state directory ancestry,
writes and syncs a new private file, atomically renames it to `journal.json`, then syncs the
directory. Ancestry synchronization includes user-owned ancestors and the first differently owned
parent so newly created first-use directories are durable before acknowledging a commit; traversal
is capped at 256 directories. Failed validation leaves
the previous generation intact. A failure after rename reports uncertain durability; retry the
same operation ID or inspect the journal instead of assuming rollback. Durability relies on the
filesystem honoring sync and rename. There is no claim of transactional effects across fux and
zor or durable exactly-once application processing.

After a valid committed journal is loaded under the lock, startup discards recognized private
`.journal-<32 hex digits>.tmp` candidates left before rename. Their contents are never treated as
a committed task. Other files remain untouched. Recovery scans at most 256 directory entries
and accepts at most 16 bounded candidate files; unexpected excess/unsafe candidates fail visibly.
Deletion is directory-synced before new mutations. Corrupt committed state is preserved, including
its candidates, for explicit recovery rather than replaced with an empty journal.

Limits are 128 tasks, 128 sessions, 512 attempts, 1024 prompt records, and 4 MiB encoded journal
size. There is no automatic eviction of active/uncertain records. Unused adoption-only or
discarded-preparation tasks can be forgotten to release space. Exhaustion fails before replacing
the committed file. Only explicit submission sends stored prompt text; journal loading never executes it or
reconstructs task success from it. The private files are coordination records, not authenticated proof against another process
running as the same OS user.

## Verification

Unit tests cover lock exclusion, committed-state reopening, invalid-mutation rollback, recovery
of a partial candidate, preservation of unrelated files, and rejection of corrupt/unsafe files.
The combined real fux/zor suite runs `tests/verify/zor_tasks.py` with disposable HOME/XDG paths.
It verifies live/stale adoption, replay/conflict behavior, shared-target writer exclusion across
processes, persisted preparations, corrupt-state preservation, and metadata-only forgetting while
the pane PID and controller input sequence remain unchanged during metadata-only operations.
A task-owned forwarding socket then drops a real submission reply: reconciliation recovers its
receipt and repeated submit calls leave input_sequence unchanged. Further cases exercise expired
receipt evidence, human interference, a target check that crosses the submission deadline,
and a killed submitting CLI whose replacement reconciles without repeating the input.
Cancellation cases preserve receipt evidence, do not type or stop the pane, do not reactivate
released prompts during reconciliation, allow a new prompt after explicit abandonment, reject
new preparation for a cancelled task, and retain submitted history against forgetting.
Unit coverage keeps partial-failure uncertainty exclusive.
Further real-fux checks use manually supplied synthetic integration reports: pre-submission and
wrong-token/operation rejection, immutable retries, fast responses without working, previous-prompt
report rejection, intervening human input, timeout retained after synthetic delayed/lost receipt
evidence, unsent reconciliation, and a real worker exit with status 17. These do not establish
real-agent adapter guarantees. Follow-mode coverage checks separate caller deadlines,
delayed reports, cancellation, killed-waiter recovery, and stalled listing/status calls without
coordination mutation. Unit tests enforce the 32-slot limit, private files and retained lock inodes. These are coordination contracts,
not real-agent response/completion coverage or a demonstrated advantage over herdr.


`zor task resume TASK --operation OPERATION --instance INSTANCE` explicitly recreates a supported
native OpenCode session as a new attempt of the same task. The [recovery contract](RECOVERY.md)
defines eligibility, retained storage/native metadata, retry boundaries, history and cleanup
limits. Old prompts are not resubmitted. Use a fresh prompt operation after successful attachment.
