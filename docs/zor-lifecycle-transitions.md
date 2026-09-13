# Zor durable lifecycle transitions

The journal lock serializes orchestration. `Store::transaction` changes a candidate,
validates it, writes and syncs a private temporary file, atomically replaces the
journal, then syncs its directory. External effects run only after a successful
intent commit. A directory-sync error after replacement is an uncertain commit:
the visible journal remains authoritative when the caller reopens it.

`tasks/lifecycle.rs` and its `attachment` and `worktree` modules own the transitions
below. Wire authentication, live process verification, filesystem identity and Git
observations stay at their respective effect boundaries. A transition method does
not itself prove that an external process or checkout exists or has disappeared.

| Operation | Durable intent before effect | Result publication | Interruption and retry |
| --- | --- | --- | --- |
| Launch creation | Prepared → Submitting before split | Pin the returned pane ID; attach the exact launch to a new session and attempt | Never resend a Submitting/Uncertain launch; recover the original via retained launch/event/final evidence |
| Managed attachment | Existing launch intent and immutable server identity | Create session, attempt and task association together; set Attached or Closed with final evidence | Commit precedes creation-pin release; release failure reconciles the same pane, including exit between attachment and release |
| Group admission | Pin task/attempt/prompt membership; commit admitted plus cursor before submit | Shared delivery transition publishes the stable operation receipt | Service restart selects retained candidates; cancelled groups cannot authorize new submissions |
| Group cancellation | Commit cancelled, disable automatic scheduling and release prompt coordination before disarm | Retain monotonic input-started and receipt evidence; persist bounded retirement cursor | Retry disarms only unreleased native arms; never retract delivered terminal bytes |
| Native session resume | Explicit stable resume operation and previous-attempt authorization | Archive previous launch identity and install the newly attached attempt in one journal transaction | Recover the retained resume intent; never choose another native session or replay prompts implicitly |
| Prompt delivery | Durable reservation, then Submitting plus monotonic arm input-started flag | Publish receipt for the same operation/expiry and nondecreasing evidence | Status can prove a lost submit was still Reserved; retry only that operation; unknown/expired evidence remains uncertain |
| Managed stop | Cancel coordination, retaining Verified outcome when applicable, and set stop_requested together | Kill acceptance is insufficient; only matching final evidence closes the launch | Retry/recovery resolves the retained target; adopted and historical launches cannot grant stop authority |
| Worktree allocation | Record bounded reserved parent path before mkdir | Pin verified private directory device/inode; Allocating → Prepared | Explicit reconciliation may pin only an empty private directory at the exact reserved location |
| Worktree creation | Prepared → Creating before git worktree add | Verify registration, branch, repository and checkout identity; record Ready | Reconcile Creating/Uncertain without repeating add; reject replaced directories and unrelated repositories |
| Worktree removal | Verify ownership, active use and dirty/force policy; commit Removing and immutable force intent before git worktree remove | Record Removed only after both checkout absence and registration absence | Reconcile without repeating remove, changing force intent, pruning registrations or recursively deleting paths |

Terminal observations do not set task success. Receipt publication does not erase
response, cancellation or release evidence. An unavailable endpoint cannot erase
previous proof of server replacement. Retained workspace/stream values identify
launch origin; manager routing determines the current location without replacing
the process identity or expanding cleanup authority.

## Regression coverage

- `tasks::lifecycle` unit tests reject repeated attachment, changed creation pane,
  adopted stop authority, receipt replacement/regression and worktree identity
  replacement. They preserve task outcomes, cancellation and removal force intent.
- `zor-launch` exercises lost creation replies, creator termination, no duplicate
  creation, final evidence and process exit around pin release.
- `zor-tasks` exercises receipt recovery, duplicate retry and human interference.
- `zor-recovery` exercises stop recovery, unavailable versus replaced fux,
  ownership isolation and retained history without replay.
- `zor-worktree` uses an actual child termination at a Git filter barrier, plus
  explicit journal fixtures for effect-before-record and removal-intent recovery.
  The latter model persisted crash states; they are not injected filesystem sync
  failures. It also checks symlink/replacement refusal and dirty/force intent.

See `codebase-improvement-report.md` for exact executed commands, evidence and
remaining coverage. These tests do not by themselves prove every crash boundary,
agent integration or native OS behavior.


## Attempt and group policy audit

Production writes to `Attempt.state` occur in the shared attached-observation
transition; new attempts are constructed by adoption or managed attachment. A
live observation preserves NeedsInput instead of blindly marking every attempt
Active. Positive server-replacement evidence remains Lost during later outages;
only authenticated final evidence publishes Finished. These observations do not
set TaskOutcome::Verified.

`group.rs` already owns admission, dependency/concurrency decisions, run generation,
pause publication and cancellation. It delegates submission to the shared delivery
path. Moving these policy decisions into a generic lifecycle engine would duplicate
that owner. Admission commits before submission; a later lock holder rechecks
cancelled membership through `authorize`. Its stable operation and receipt prevent
a scheduler restart from creating another input operation.

`resume.rs` retains explicit provider/session-selection policy. It checks the old
attempt and native session binding, original process absence and retained storage
metadata before creating a new stable launch intent. Shared managed attachment
calls `resume::attach_launch` inside the same candidate transaction that installs
the new session/attempt, so the old launch archive and current task association
cannot become separate successful commits. Existing offline resume evidence
verifiers test their recorded fixtures; they are not fresh execution of this
runtime. The fresh `zor-resume` scenario now covers the shared attachment resume branch
with a synthetic bundled adapter and forces exit before pin release. It reproduced
and now guards selection of the current launch after archiving, preserved old
evidence, stable retry and no input replay. It does not validate a real provider's
conversation restoration.
