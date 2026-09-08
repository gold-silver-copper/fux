# Recover abandoned checks and recorded managed stops

The zor service resumes persisted `stop_requested` intents for attached managed launches.
It does not infer termination authority from adoption, cancellation, inactivity, lost observation
or process exit. It never creates a new launch or replays a prompt during this recovery.
Verified task outcomes and their sealed evidence remain intact through recovered cleanup.

```sh
zor task recover
zor task recover --after LAST_SELECTED_TASK
```

Before selecting a stop, recovery reconciles abandoned check submissions. Each check runner holds
a shared lock on the private, persistent `check-runners.lock` from before durable submission through
result publication, including publication retries. Recovery holds the journal lock and attempts a
nonblocking exclusive runner lock. If any runner remains, all check recovery is deferred; inspection
and stop recovery still proceed. A continuous stream of overlapping runners can defer reconciliation
indefinitely. The lock descriptor closes on child exec and is released when the runner dies.

When exclusive admission succeeds, all retained `submitted` checks (at most 128) become `uncertain`
in one transaction. No command, signal, or artifact read occurs. Requested artifacts receive explicit
unavailable-capture problems; their IDs remain reserved while pending byte reservations are released.
An existing journal's recovery response includes `recovered_checks`, the number transitioned.
Subsequent recovery is a read when there are no new abandoned checks or pending stops.

A missing runner does not prove its child stopped, even if a timeout has elapsed. Uncertain checks
continue to block removal of a worktree containing their recorded cwd, including forced removal.
Their old IDs cannot execute again. Required uncertain checks cannot satisfy verification. Optional
uncertain checks cease being pending verification blockers, consistent with verification's scope of
selected required evidence; verification does not attest that all subprocesses have terminated.
No operation currently resolves uncertain execution effects or removes that cleanup guard.

Each call attempts at most one pending stop. Results include `selected` (task ID or null),
`pending` (remaining candidate count), and `outcome` (`closed` or `unresolved`) when selected.
An unresolved attempt includes a bounded `problem`. An inspection/storage failure before selection
is an ordinary command error. `closed` requires retained final evidence, not merely an accepted
kill request. Unresolved records remain inspectable and eligible for later reconciliation.

Candidates are ordered by task ID. `--after` selects the next greater ID and wraps to the first;
the service retains this cursor in memory so an unresolved target cannot starve other candidates.
A restart resets the cursor but uses the same durable stop intents. Selection and execution hold
the same Store lock and reuse normal stop ownership, incarnation, stream, pane and process checks.
No replacement target is selected after identity loss.

The existing coordination worker attempts one recovery on startup and then at least one second
after the previous attempt completes. On service startup it also snapshots at most 128 retained
managed launches in Submitting, Uncertain or Attached phases and reconciles at most one per
recovery tick. Prepared launches are never submitted; Closed launches need no sweep. The
snapshot is taken on the first successful journal open. Busy admission leaves its cursor
unchanged. Each selected reconciliation uses the same identity/final-evidence rules as
`task launch-reconcile`; an unavailable target records uncertainty where storage permits and
cannot starve later entries. A storage failure can prevent publishing that observation and
remains visible to ordinary journal operations. The sweep makes one pass, then stops; it does
not poll every retained launch forever. A later explicit reconcile or service restart can retry
unavailable targets. New launches use their ordinary creation/reconciliation path.

Stop recovery and the startup sweep execute serially in the existing coordination worker;
no extra thread, process ownership, launch policy or prompt replay is introduced. Check workers
and the observer remain separate. Recovery
shares the coordination lane with ordinary task requests and can delay their admission under
the existing stop RPC deadlines (individual RPC stages are bounded; native filesystem calls can
still stall). Expired queued requests retain their usual no-execution rule. External CLI callers
may receive journal-busy and should retry. Shutdown permits an already-started recovery to finish,
then stops the worker; it does not retract a persisted or delivered stop.

An absent journal is not initialized by recovery or observer-only startup. Existing journal
validation failures or locks prevent recovery effects; task requests still expose those errors.
Use explicit `task recover` and `task inspect` for diagnostics. This mechanism does not repair
corrupt storage, recreate lost fux processes, remove worktrees, cancel check subprocesses or
submit prepared prompts. Ambiguous creation is reconciled only when retained live/final evidence
permits; otherwise it stays uncertain. Agent-session resume policy remains unfinished.

`tests/verify/zor_recovery.py` exercises a persisted-before-kill crash point, service startup,
stale-target rotation, no storage creation during observer-only startup and cancellation/adoption
isolation. The two-worker workflow also exercises recovery of a verified worker's stop.

`tests/verify/zor_checks.py` kills a task-owned check caller while its command remains alive,
recovers uncertainty without replay, verifies idempotence and cleanup refusal, and confirms that a
live runner prevents recovery without losing its eventual result. Store unit tests cover shared
admission, exclusive exclusion until every runner releases, and malformed lock rejection.

Managed adapters also support [clean producer replacement](INTEGRATIONS.md) at their private
endpoint. This registration handoff preserves the live fux target and old reports, fences the
retired producer, and leaves unresolved prompts uncertain until explicit abandonment. It does
not replay input or restore an abruptly lost process/socket. The replacement starts with unknown
native ancestry and needs a new managed prompt to establish current state.

Groups explicitly enabled with `task group-run` resume service advancement after restart.
A separate bounded scheduler retains original prompt operations and receipts; it does not infer
verification or extend preparation deadlines. Paused/manual groups stay paused. Submission
failures pause advancement with a diagnostic, subject to the documented uncommitted-failure
window. See [GROUPS.md](GROUPS.md) for admission, retry and cancellation semantics.


## Original fux server replaced

`zor task launch-reconcile TASK` first checks the pinned original target and retained final
records. If those cannot establish lifecycle, a bounded same-user live listing can establish
that the workspace endpoint now belongs to a different fux incarnation. The attempt becomes
`lost`, with its original session, pane/process identity, launch and prompt receipts retained.
An unavailable endpoint alone remains `uncertain`. Previously established loss persists through
later outages; access to the exact original live target can restore active observation.

Lost ownership is not an exit receipt. The launch remains attached to its historical identity,
without fabricated final output; existing cleanup guards remain. No replacement pane is adopted,
created, closed or written to. Existing reserved input cannot be submitted to a replacement
server. Inspection and results expose the lost attempt and recovery problem.

Explicit native OpenCode recreation is available:

```sh
zor task launch-reconcile worker
zor task resume worker --operation resume-1 --instance NEW_FUX_INSTANCE
```

The resume command authorizes one new attempt, using the current launch's retained runtime,
workspace, cwd, original argv, native session binding and HOME/XDG namespace. It requires an
open managed task, reconciled Lost/Finished attempt, absent original root process, registered
OpenCode storage metadata and a native binding from that producer. Group membership pins its
attempt and prevents resume; submitted/uncertain checks also prevent it. Existing conflicting
native session-selection flags and option terminators are refused. Missing database/configuration remains an application
startup problem, not evidence that a different native session may be substituted.

The operation ID is durable before any creation request. Prepared intent can be explicitly
retried; Submitting/Uncertain intent is reconciled by marker/live/final evidence without another
creation request. Retry the same `task resume` command after uncertainty. `launch-reconcile
OPERATION` can reconcile pending creation; service startup also considers already-submitted
resume intents, but never submits Prepared intent. A cancelled task cannot submit Prepared
intent. If creation already happened, reconciliation still records the owned process and preserves
the task's cancellation, so explicit stop can address it.

Attachment atomically archives the previous launch under the operation ID, retains its original
session/attempt and prompt/evidence records, and advances the original task to a new attempt.
Task title, creation time, outcome and verification policies persist. Old input is never copied
or replayed. Only current-attempt evidence can satisfy verification; historical unresolved input
is retained without blocking current-attempt verification. Repeating a completed operation returns
the current task without creating a pane. Use a new operation ID for a later authorized resume.
Lost historical launches still do not supply exit evidence or expand worktree cleanup authority.


The journal can retain historical managed launches separately from a task's current launch.
A historical launch has an explicit `task` owner; its session and attempt retain their original
source, check, artifact and change associations. A managed session belongs to exactly one
attempt. Current task requirement summaries count only current-attempt evidence; retained
history cannot satisfy a new attempt's verification. Task policy remains sealed once evidence
exists. Historical launch IDs are inspectable evidence, not stop or reconciliation targets,
and service recovery excludes them. Bounds remain 128 current launches and 128 sessions,
512 attempts/total launches, plus the existing journal byte limit.

The model fixture separately constructs a second attempt without starting a process. The real
`capture_zor_resume.py` fixture exercises the explicit command through native resume and a fresh
required check. These evidence scopes are distinct.


Managed OpenCode registration now retains `integration.storage_environment`: the five exact
HOME/XDG config/data/state/cache settings reported by the running adapter, with null for an
absent XDG variable. This is agent-side metadata; it is not copied from the controller's own
environment. Native session/message identity remains in the prompt's immutable report binding.
No secrets, inline provider config or arbitrary environment variables are collected. Paths must
be absolute and control-free, with at most 2048 total UTF-8 bytes; the adapter also bounds encoded
metadata to 3072 bytes inside the existing 4096-byte reply limit. Missing/unusable metadata
(including an OpenCode test-home override) remains null. It does not disable ordinary observation.
A producer replacement preserves metadata only if it matches the original registered namespace;
a change makes it unavailable for the rest of that launch.

The real OpenCode storage trace confirms normal HOME/XDG metadata across a plugin reload.
It does not establish that a session database still exists, preserve provider credentials, or
authorize recreation. Resume explicitly uses retained settings, unsets recorded absent variables and the unsupported
`OPENCODE_TEST_HOME` override, preserves cwd and selects the recorded native session. The explicit
resume operation supplies task authorization; passive reconciliation alone does not.
