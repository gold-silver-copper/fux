# Retained task artifacts

```sh
zor task require-artifact worker-1 report output/report.json
zor task artifact-collect worker-1 report-1 output/report.json --requirement report
zor task artifact-inspect report-1
zor task inspect worker-1
```

Collection retains exact file bytes in zor's private, atomic task journal. JSON `artifact.bytes`
is an array of byte values, preserving binary data without UTF-8 conversion. Records identify
the task, attempt, relative source path, collection time, journal generation and optional requirement name. Task inspection lists compact
metadata and byte counts; artifact inspection returns one complete record.

Artifact IDs are globally unique. For manual collection, repeating the same ID with the same task, path and requirement reads the
retained record, even after the source changes, disappears, the task is cancelled, or its worktree
is removed. A different task/path/requirement conflicts. Use a new ID to collect a later version. No artifact
deletion or compaction operation is currently available.

`require-artifact TASK NAME PATH` declares up to 32 task-scoped names and exact relative paths
on an open managed task. Paths need not exist yet. Exact declaration retries read current task
state, including after sealing or cancellation; changing a declared path fails. Additions seal
when the first artifact is retained (even an optional artifact) or the first check is submitted.
There is no removal/replacement operation. Required-check additions retain their separate rule:
they seal at the first check submission. Declare the artifact policy before collecting outputs
or running checks.

Collection with `--requirement NAME` must match that task's declared name and path before file I/O.
Without it the record is optional, even if its path matches a requirement. Each requirement reports
`missing` or `collected` with its latest retained artifact ID, ordered by journal generation rather
than clock time. A failed collection publishes no record and does not replace earlier retained
bytes. Collecting another version uses a new ID; all earlier records remain readable. Task inspect
reports sealed/required/collected counts and result reads expose each requirement plus the
`required-artifacts-missing` blocker. Collected means bytes were retained, not that their content
is correct, current, or bound to a passing check. Empty files count as collected.

New collection requires an open managed task and reads beneath its launch cwd. Linked worktrees
must pass ownership/readiness checks and the opened root must match the pinned checkout inode.
Paths associated with retained worktree removal intent are refused. Adopted-only tasks do not
yet support artifact collection.

Paths must be relative UTF-8, at most 4096 bytes and 32 components, without control characters,
empty components, `.` or `..`. Each child is opened relative to its retained parent descriptor with
no symlink following; the final file must be regular with one hard link. FIFOs, directories,
symlinks and oversized files fail. File size and inode/change timestamps are checked before and
after reading to detect concurrent mutation. These checks are not an atomic filesystem snapshot
or protection against a hostile process running as the same user. Renames can change the current
name of an already opened object. Retained bytes are precisely what zor read, not proof of a
coherent project revision or the path's contents at some later time.

Limits are 64 KiB per artifact, 128 records and 512 KiB of retained artifact bytes per journal,
also subject to its four-MiB serialized limit and check-result reservations. Files are never
silently truncated. Collection holds the journal lock and reads at most 64 KiB plus one byte;
native filesystem calls can still stall. A crash before journal publication leaves no retained
artifact and a retry may read newer bytes. After publication, retries return the recorded bytes.

Artifacts are output evidence, not independently verified results. [Changed-file observations](CHANGES.md)
are collected separately and included in the [retained result view](RESULTS.md). Explicit `task verify`
seals coherent source/check/artifact selection. General artifact content validation and a frozen full
result remain unfinished.
Collecting a file never changes task outcome.


## Capture outputs with a source-bound check

```sh
zor task require-artifact worker-1 report output/report.json
zor task check worker-1 tests-1 --source revision-1 --requirement tests \
  --artifact report=report-1 -- /path/to/check-command
zor task artifact-inspect report-1
```

Repeat `--artifact NAME=ID` for up to eight declared artifact requirements. Each requested global
artifact ID must be unused, including IDs reserved by previous failed checks. Source input is
required. Names are task-scoped and resolve to the paths declared before the first check submission.
The mapping is immutable check intent: retries must supply the same names/IDs and source.

Before creating the check directory or starting its command, zor records Submitted and reserves
all requested IDs, artifact slots, 64 KiB of raw artifact space per request, and 300000 serialized
bytes per request in addition to the existing 64 KiB check-result reservation. The journal's 128
artifact/512 KiB raw-byte limits include these pending reservations. Admission fails before command
effects if any limit cannot fit. Other journal operations cannot consume this reserved capacity.

After an observed command exit, zor reads the requested paths through the original open directory
descriptor retained from source materialization. Renaming the directory and replacing its old path
cannot redirect collection. The usual no-symlink, single-link regular-file, size and mutation checks
apply. Successful records include check/source IDs and requirement names. All captured artifacts,
capture problems and command evidence publish in **one journal transaction**. Artifacts in the same
batch share that publication generation. Individual failures do not discard successfully read files.
A failed command can still have captured files; its exit status remains failed.

Uncertain command outcomes skip file collection and record a problem for every request. A missing,
unsafe or oversized output also produces an explicit capture problem. Artifact ID reservations
remain in check history even when no file was retained. Record/raw-byte/serialized reservations
release at terminal publication; lost callers/publication can leave Submitted and retain them.
Task cancellation does not prevent an already submitted check from publishing its evidence.

Use artifact-inspect to read captured bytes. Retrying the check or using check-inspect returns
the capture IDs/problems and command evidence. Manual artifact-collect cannot
claim a check-reserved ID, including a successfully captured one. No retry recollects changed files,
recreates a check directory or reruns the command. A fresh capture requires a new check execution
and new artifact IDs.

`artifact_captures` in task inspect/result reports the latest check submission requesting each
artifact requirement, with pending/collected/failed status and check/source/artifact IDs. Pending
and failed captures add `artifact-capture-pending` and `artifact-capture-failed` blockers. An older
capture finishing late cannot clear a newer failure. This is separate from required_artifacts,
which continues to describe availability of the latest retained bytes; old bytes can be available
while the latest requested capture failed.

Captured bytes were observed in a directory initialized from the recorded source after its invoked
command exited. Files may already have existed in the input snapshot. This is not proof of who
created them, a filesystem snapshot across all files, or protection against detached descendants
and same-user mutation. Explicit [verification](RESULTS.md) selects one source with its required
checks and artifacts. The command's `passed` field still describes its exit,
not capture success or task verification.
