# Collect retained task evidence

```sh
zor task result worker-1
zor task check-inspect tests-run-1
zor task artifact-inspect report-1
zor task verify worker-1 source-1
```

`task result` assembles one versioned JSON view from a single validated journal generation.
It identifies the task, outcome, attempt, session, agent, associated worktree and managed launch;
includes retained final output when available; and lists compact check/artifact references.
Use check-inspect for full command/output evidence and artifact-inspect for exact retained bytes
and source path. Artifact IDs remain usable after source deletion or worktree removal.

Each named required check has a status (`missing`, `submitted`, `uncertain`, `failed`, `passed`)
and the latest execution ID. Latest means highest submitted journal generation, so a late older
pass cannot replace a newer failure. Diagnostic checks are listed but do not satisfy requirements.
Full required argv remains available through task inspect. Passing a check only describes the
invoked command's observed exit.

Each [required artifact](ARTIFACTS.md) has its declared name/path, status (`missing` or `collected`)
and latest explicitly associated retained artifact ID by journal generation. Optional collections
do not satisfy requirements. Failed collection attempts do not supersede retained bytes; this is
availability of recorded output, not freshness or content validation. Artifact summaries include
requirement names, collection generations and optional check/source IDs, without embedding bytes.

`artifact_captures` separately reports the latest check submission requesting each artifact name.
A pending or failed capture adds an explicit blocker even when older retained bytes are available.
Late publication from an older check cannot override that latest-submission status.

Prompt evidence contains at most 32 summaries, newest first by creation wall-clock timestamp
and then descending operation ID. This ordering is for presentation and is not submission order.
`omitted_count` explicitly reports hidden older summaries. `unresolved_count` and the
`prompt-unresolved` blocker include all prompt records for the attempt, including omitted records.
Summaries expose delivery/wait/release state and caller-reported claims; they omit prompt text,
report tokens and complete receipts. Claims are observations, not authenticated task success.

Blockers identify retained cancellation/failure, unresolved attempts/prompts, missing or nonpassing
required checks, missing required artifacts, and recorded launch/worktree problems. A pending launch without an attached task
still has a result view with null task outcome/attempt and `launch-not-attached`. An empty blocker
list does not establish readiness or success: verification remains `unverified` until an explicit
`task verify TASK SOURCE` operation seals the declared policy evidence.

The optional `integration` summary carries retained kind, producer and registration time,
with `availability: not-probed`. Use `task adapter-status TASK` for a fresh endpoint probe;
neither registration nor a retained prompt claim proves current adapter availability.

The operation does not reconcile, poll fux, rerun checks, read the checkout or collect new artifacts.
Output is the retained launch final record, with its original clipping/exit limits; unavailable
output is explicit. Natural zero exit, an agent response and all passing checks leave verification
unverified. Existing task outcome is reported unchanged. `changed_files` includes the latest
[retained Git observation](CHANGES.md), or explicitly reports `not-collected`; it is not an atomic
snapshot and excludes ignored files. `sources` lists [retained committed inputs](SOURCES.md), and each
check identifies its optional source. `source_check_binding` reports `per-check-optional-retained-git-tree`;
this describes supplied initial files, not a common verified revision across checks. Required artifact
policy is identified as `retained-bytes`. This view is a current journal read, not a persisted full
final result or completion event. [Handoffs](HANDOFFS.md) pin verified predecessor references
in a durable prompt; a frozen full result remains unimplemented.

`task verify TASK SOURCE` requires an open managed task, reconciled attempt/launch evidence,
no unresolved prompt coordination, no outstanding submitted checks, and at least one declared
required check. The latest execution of every required check must finish with exit zero on exactly
the selected retained source, without artifact capture failures. Every required artifact must be
captured by one of those selected checks; both its latest capture request and latest retained bytes
must agree with that selection. Diagnostic checks, manual collections, mixed source revisions and
older successes hidden by newer failures cannot satisfy this policy.

The operation atomically records task/attempt/source, selected check and artifact IDs, timestamp
and generation, sets the task outcome to `verified`, and releases resolved prompt coordination.
The journal validates the seal on every load. Repeating verification with the same source returns
the existing record without mutation; selecting another source is refused. New task evidence and
cancellation are refused after sealing. Explicit managed `task stop` still closes the owned pane
and preserves the verified outcome, allowing subsequent owned-worktree cleanup.

Result verification exposes `status`, `record`, and scope
`declared-checks-and-artifacts-for-retained-source`. It proves only the recorded command outcomes
and captured bytes under the declared policy. It does not prove semantic correctness, dependency
pinning, sandbox isolation, agent termination, or the state of a later live checkout. Verification
does not run commands, poll fux, stop the agent, or collect new files. Later lifecycle observations
can change the current result view without rewriting the sealed evidence selection.

JSON is capped at 256 KiB and fails with an actionable error if it cannot fit. Check/artifact
references are bounded by their journal record limits; prompt text and artifact bytes are not
embedded. Reads retain Store's usual private-directory, locking, journal validation and temporary
file recovery behavior, but do not change the journal generation or evidence records.
