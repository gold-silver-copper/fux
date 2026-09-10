# Retained committed inputs for checks

```sh
zor task source-collect worker-1 revision-1 --revision HEAD
zor task source-inspect revision-1
zor task source-file revision-1 src/main.rs
zor task check worker-1 tests-1 --source revision-1 --requirement tests -- cargo test
```

Source collection resolves a revision to a commit in a managed task's owned ready worktree,
then retains its regular file bytes and executable modes in zor's atomic private journal.
Dirty index/worktree changes, untracked files and ignored output are not included. Commit those
inputs before collecting a new source ID. The revision expression is resolved only for initial
collection; retrying the same task/ID/expression reads the retained snapshot even if HEAD moved,
the task was cancelled or the originating worktree was removed. A different intent conflicts.

Git object reads bypass clean/smudge filters, text conversion, hooks and replacement refs. Git LFS
pointers therefore remain pointer bytes. The retained commit object, blob bytes and reconstructed
path/mode/tree hierarchy are checked against Git SHA-1 or SHA-256 object identities on collection
and journal load. This detects corrupted content, names and modes; it is not authentication against
someone who can replace the entire journal. Checkout inode is rechecked before publication.
Source collection issues no explicit Git mutation commands and does not modify refs, the index
or the worktree. Git object reads in partial/promisor repositories may lazily fetch missing objects
through configured transports and populate the object store; the bounded subprocess contract still
applies. Complete local repositories avoid that dependency.

Supported trees contain regular files only. Symlinks, submodules, special modes, control characters,
non-UTF-8 paths, backslashes, dot components and `.git` path components are refused. Limits are 256
files, 64 KiB per file, 256 KiB total file bytes and 32 KiB path bytes per snapshot. Paths have at most
32 components and 4096 bytes. A retained commit object is at most 64 KiB. Empty root trees work;
tree structures containing explicit empty subdirectories cannot be reconstructed from files and
are refused. Filesystem name collisions during materialization also fail before command execution.

At most 128 sources and 512 KiB of aggregate source-file bytes share the journal's four-MiB
serialized bound, alongside other records and check-result reservations. Serialized JSON byte
arrays can reach that limit before the raw byte limits. Oversized input is rejected, not truncated.
The collection holds the journal lock, uses the existing bounded Git runner with a shared ten-second
command budget after worktree preflight, and publishes only complete validated snapshots. Native
filesystem/process operations retain their normal latency. No pruning or source compaction exists.

`source-inspect` returns commit/tree/provenance and a bounded file inventory without file contents;
`source-file` returns one exact byte array. Result views list compact source summaries and each
check's optional source ID. They do not embed the complete source tree.

A check with `--source ID` must use a source for its own task/attempt. It durably records Submitted
and a fresh `check-<random nonce>` path under the state root before creating files or launching its
command. It materializes retained bytes in that new private directory and runs there, with no
`.git` checkout metadata and no agent edits or previous check output. Files are owner-readable and
writable, with executable files owner-executable. Each execution gets a separate directory.
Materialization errors publish Uncertain evidence; a crash can leave Submitted and partial files.
Neither state allows replay of the same execution ID. Exact retries are reads, even if the private
directory changed or disappeared. A new execution ID materializes a fresh copy.

Directories and command outputs remain after execution; there is no automatic cleanup yet. At most
128 retained check records can introduce such directories, but commands can write arbitrary output
under the existing command execution contract. Do not treat leader exit as proof that detached
children have stopped using a directory. Source files are durable in the journal; directory contents
are mutable execution scratch space and are not retained artifact evidence.

This binds the **initial supplied files** to a check invocation. It is not a sandbox, a read-only
filesystem, dependency pinning, or proof that the command read only those files. Explicit commands
can modify their copy, access other paths, use inherited environment/programs or contact services.
The command timeout starts after materialization; existing runner/output/reply limits still apply.
Without `--source`, a check retains its existing behavior in the managed launch cwd.

Required-check counts still describe the latest command execution for each name; they can include
unbound checks or checks from different snapshots. Explicit [verification](RESULTS.md) selects a coherent
source/check/artifact set. Manual artifact collection reads the task's original launch cwd. Checks can instead request
[bound artifact capture](ARTIFACTS.md) with `--artifact NAME=ID`, publishing captured bytes and check
evidence together. A frozen full result, check-directory cleanup, check cancellation
and crash resolution remain unfinished. A passed source-bound check
never by itself changes a task to Verified.
