# Retained changed-file evidence

```sh
zor task changes-collect worker-1 changes-1
zor task changes-inspect changes-1
zor task result worker-1
```

An open managed task with an associated owned Ready worktree can collect a bounded Git change
observation. The record identifies task/attempt/worktree, the worktree's original base commit,
observed HEAD, collection time and journal submission generation. It stores two lists:

- `committed`: Git name/status changes from the pinned base commit to observed HEAD.
- `working`: Git porcelain-v1 index/worktree status (`XY`), including all untracked files.

Paths are arrays of raw byte values, not line-delimited or lossy UTF-8 names. Renames are reported
as deletion/addition so each entry has one unambiguous path. Ignored files are excluded. Submodule
changes use Git's status representation; this does not recursively inventory submodule files.
This is Git's view of changes, including its index, attributes and filter semantics, not a raw
filesystem inventory. Missing/unavailable Git evidence fails collection rather than publishing
an empty success record.

Collection checks owned worktree identity/readiness, samples HEAD/diff/status twice for equality,
then rechecks the checkout inode before publishing. The read commands share a six-second runner
budget after worktree preflight and a 256 KiB output limit per command stream. Preflight and native
OS/filesystem calls have their existing latency limits. Git hooks and fsmonitor are disabled;
diff disables external diff and text conversion, and replacement objects are ignored. Existing
repository-local clean filters retain native Git behavior and may run while Git inspects files.

Repeated equal metadata is only a stability observation. Source contents may change without
changing status; concurrent changes can occur between samples or after collection. This is not
an atomic source snapshot, content digest, immutable verification checkout or check/source binding.
The operation never changes task outcome or treats a clean tree as verified completion.

Change IDs are globally unique. A matching task/ID retry reads its retained record, including
after task cancellation or worktree removal; another task conflicts. Choose a new ID for a fresh
observation. At most 128 records share the journal; each allows at most 256 paths across both lists,
4096 bytes per path and 32 KiB total filename bytes. The journal's four-MiB serialized bound and
check-result reservations still apply. No silent clipping or deletion/compaction is provided.
Worktree preflight may update its existing reconciliation diagnostics even if later collection
fails; failed sampling never publishes a partial Changes record.

Task result includes the latest retained observation by submission generation, explicitly marked
`atomic_snapshot: false` and `ignored_files: excluded`. Result reads do not resample Git. Earlier
records remain available through changes-inspect. Artifact bytes, change observations and passing
checks remain separate evidence until source/artifact/check binding is implemented.
