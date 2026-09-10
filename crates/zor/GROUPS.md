# Durable prompt groups

Zor coordinates bounded prompt admission for existing managed tasks. Fux continues to own
ordinary panes and terminal input; group membership, dependencies and verification live only
in zor. Groups do not launch workers. A controller can call `group-step` to advance one
eligible prepared operation, or opt into durable automatic advancement with `group-run`.

Prepare prompts on managed tasks first, then create a group:

```sh
zor task prepare alpha --operation alpha-work --text 'Implement the parser' --timeout-ms 60000
zor task prepare beta --operation beta-work --text 'Implement the tests' --timeout-ms 60000
zor task prepare gamma --operation gamma-work --text 'Review the results' --timeout-ms 60000
zor task group-create workers --concurrency 2 \
  --operation alpha-work --operation beta-work --operation gamma-work \
  --after gamma-work=alpha --after gamma-work=beta
zor task group-step workers
zor task group-inspect workers
zor task group-list
```

Each `--after OPERATION=TASK` requires a retained verification for that task before the
operation can be admitted. Dependencies do not supply predecessor artifacts to the prompt;
use the separate [handoff](HANDOFFS.md) operation when that evidence should be embedded.
Declare task check/artifact policy before executing checks and use [verification](RESULTS.md)
to seal the results. Neither an agent response nor a successful process exit verifies a task.

For automatic advancement:

```sh
zor task group-run workers
zor task group-pause workers
```

`group-run` starts zor's service when absent and enables only the named group. `--directory`
selects the service endpoint; global rules/agent options configure a newly started service.
An existing service must use the same task state directory, or the command fails before
enabling the group. The structured service actions are `group-run` and `group-pause`, each
with `id`. Pause also works directly against the task journal while the service is absent.
Repeated enable/pause with unchanged state is read-only.

One separate scheduler advances at most one eligible operation per tick, waiting one second
after the prior attempt finishes. It rotates across runnable groups; capacity/dependency waits
consume no submission turn. Group admission and member rotation persist before I/O. The
service resumes enabled groups after restart, using the original operation, input receipt and
preparation deadline. It does not generate prompts, verify results, launch replacements or
extend deadlines. Pausing stops new selection; an already-selected submission can finish.
Cancel the group to disable further submission under the existing cancellation checks.

Submission errors or receipts other than Queued/Delivered pause automatic advancement and
record a bounded `group.run_problem`. Inspect/reconcile the retained operation before enabling
the group again. Journal Busy before submission retries on a later tick. If recording a failure
pause is temporarily blocked, the scheduler retains it in memory and retries that write before
another submission; a newer explicit pause/resume generation supersedes the older diagnostic.
A service crash before the pause commits can lose that diagnostic. Restart never creates a
replacement prompt or replaces a retained receipt to bypass ambiguity; ordinary initial
reservation, receipt-retention and uncertain-delivery rules still apply when no receipt persisted.
Expired or otherwise ineligible members show attention without being submitted; they do not
gain a new deadline merely because automatic mode remains enabled.

Concurrency counts admitted tasks until their task verification and original prompt's
Delivered receipt are both recorded. Existing processes are not counted or paused. A failed
check, lost worker, timeout or uncertainty retains the slot and needs controller attention.
Repair prompts on an admitted task are allowed; independently verify the repaired result to
release capacity. Direct reserve/submit calls cannot bypass admission, including through an
adopted alias or another socket route to the same pinned pane identity.

`group-step` commits admission and its rotation cursor before submitting through zor's
existing prompt/receipt protocol. It can return `submission.status: unresolved` after that
commit; inspect its bounded problem and retained prompt evidence. Retry uses the original
operation and receipt, with the same uncertainty and retention limits as ordinary submission.
A fresh step call may choose the next eligible member; step itself has no request-ID replay
cache. Concurrent callers share the journal lock and cannot exceed admission capacity.
An error, including journal Busy during the final inspection, can arrive after admission
or submission committed. Inspect the group and original operation before retrying a step.
If that member was admitted, reconcile/submit its retained operation ID to finish its input;
another `group-step` may select a different member instead.
Restarting the zor service preserves the group and workers; enabled automatic groups resume,
while manual groups await another step.
Queued prompts keep their original preparation deadlines. Admission and restart do not
extend them, create replacement prompts, or imply exactly-once application processing.

```sh
zor task group-cancel workers
```

Cancellation persists the group cancellation and releases prompts belonging to member
attempts. It cancels unresolved coordination while preserving recorded response/exit evidence
and already verified waits. It preserves task outcomes and disables future submission
of the original group operations, and never retracts input, stops workers or removes worktrees.
Other explicitly prepared work can proceed after coordination is released. Stop managed
tasks and remove owned worktrees separately using their normal reconciliation checks.

Each cancellation call also attempts retirement of at most one released, never-started
integration arm, using the existing bounded adapter retirement protocol. Cancellation stays
committed if retirement fails. The response includes `retirement_pending_count`, up to eight
pending operation IDs, and the attempted `retirement` result. Repeat `group-cancel` to finish
cleanup; a durable rotation cursor prevents an unavailable arm from starving other arms.
Possibly submitted arms are never retired. With no pending retirement, repeated cancellation
is read-only. See [TASKS.md](TASKS.md) for receipt expiry and retirement uncertainty.

The journal retains at most 32 groups with 1..8 members each; concurrency is 1..member count.
Members have distinct tasks and original operations, with at most eight distinct prerequisite
task IDs per member. Active groups cannot share a pinned target. Missing prerequisites,
self-dependencies and cycles across active groups are rejected atomically. Retrying creation
with the same canonical plan returns the current view; changing the plan under its ID fails.
Group records have no deletion or compaction operation yet and share the 4 MiB journal bound.

Inspection reports retained coordination state, not a live worker-health probe. Member states
include queued, dependencies-pending, active, verification-required, needs-attention and
verified. Group state is active, needs-attention, complete or cancelled. `active_count` retains
the admitted-but-unverified count even after cancellation; it is not a live process count.
Use task inspection, result collection and adapter status for their respective evidence.
`group.automatic` is durable scheduling intent, not a claim that the service is currently alive.
`run_generation` identifies the most recent explicit mode transition. Failure diagnostics
remain visible until a new run is enabled; complete groups have no eligible submissions.
The [dashboard](DASHBOARD.md) also shows group scheduling intent, counts, failure details and
pending retirement. Incomplete paused groups and unresolved retirement require attention;
cancelled groups become quiet only after retirement finishes. Group rows have no pane target
and do not equate automatic intent with service health.

The required real-fux fixture `tests/verify/zor_groups.py` covers three isolated managed
workers, concurrency and verified prerequisites, concurrent steps, failed-check repair,
service SIGKILL/restart, alternate socket routes, cancellation and owned cleanup. The binding
fixture covers dropped adapter retirement replies and cancellation retry without extra input.
These use scripted workers and a mock adapter; they do not establish real-agent quality or
comparative superiority over herdr.
The required `zor_group_scheduler.py` variant exercises the same workflow through automatic
service startup/advancement, failure pausing and fair progress, durable restart, pause while a
dependency becomes ready, explicit resume, wrong-state service rejection and owned cleanup.
No task journal is created for observer-only service startup when none exists. Idle scheduler
ticks read bounded retained state but do not rewrite a journal with no eligible work.
