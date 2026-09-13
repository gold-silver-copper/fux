# Diagnostics and headless failure artifacts

## Fux diagnostics

Set `FUX_DIAGNOSTICS=1` when launching fux to record interaction transitions,
effect-queue size, request IDs, history read completion/stale replies/timeouts,
and invalidated effect targets. Records include process and attachment identifiers
where relevant. They do not include input bytes, terminal text, paste text, rename
contents or environment values.

The existing tracing subscriber writes these fields to
`$XDG_STATE_HOME/fux/diagnostics.log` (or the normal platform state-directory
fallback). Detailed records are disabled by default. When enabled, they go to the
private file rather than the viewer's terminal. The existing informational/error
logging also uses that file in this mode.

The file is private, refuses symlinks, and is strictly capped at one MiB. It resets
when a subsequent record opens a full file. Writers use a nonblocking exclusive
lock; records can be dropped during contention or a write failure. Diagnostics are
best-effort observations, never authoritative lifecycle or delivery evidence.

## Zor diagnostics

Set `ZOR_DIAGNOSTICS=1` to record bounded JSON-lines metadata in the task store's
`diagnostics.jsonl` (normally `$XDG_STATE_HOME/zor/diagnostics.jsonl`). Records cover
launch, attempt, delivery and worktree transitions and launch reconciliation
outcomes. They exclude argv, paths, prompt contents and error text. Each record
includes the process ID and journal generation. `directory_synced` is a boolean
for a transaction observation and null for a recovery observation; recovery
`succeeded` describes the call result, not task completion or proof of delivery.

The private, single-link regular file refuses symlinks and nonprivate permissions.
Nonblocking exclusive locking drops observations on contention. Records are at
most 4 KiB; before a write would exceed one MiB, the log resets. Logging follows
the journal rename and directory-sync attempt and cannot change its return result.
It is best-effort diagnostic evidence, never journal state or recovery authority.

## Automatic scenario artifacts

The standalone harness records bounded observations for each `scenario` command.
Successful runs discard them. A failed or unwinding scenario writes a private
`fux-harness-failure-*` directory and prints its path. Set
`FUX_HARNESS_ARTIFACTS=/absolute/directory` to choose the parent; otherwise it uses
the platform temporary directory.

`failure.json` contains:

- The scenario arguments, binary SHA-256 identities, and the Git revision plus a
  hash of tracked/nonignored regular files observed before the run. Verification
  artifacts are excluded. This source observation is not proof that a binary was
  built from that source; both identities are retained separately.
- A bounded error chain and final modeled terminal screens, dimensions, last
  checkpoints, input lengths/hashes and resize/checkpoint events.
- Metadata for the harness's local RPC helper: command, request/pane IDs,
  elapsed time and transport outcome. It does not record request payloads.
- Tails of explicitly enabled fux and zor diagnostic logs collected before isolated roots
  are deleted. Standard harness roots enable both diagnostic streams automatically.

The recorder retains at most 16 terminals, 256 events per terminal, 256 RPC records
and 16 diagnostic-log tails of at most 64 KiB each. Terminal models have at most
128 rows × 512 columns, no scrollback, and process at most eight MiB per terminal.
Oversized streams/dimensions produce `screen_omitted`; output truncation and dropped
records are explicit. The final screen is fixture output, not a visual approval or
a substitute for Betamax's raw-stream capture/replay.

Source collection is also bounded: a four-MiB file listing, 16,384 files, 64 MiB per
file and 128 MiB total, with a five-second hashing deadline. Unavailable source
identity is reported explicitly. Observations use the scenario thread; subprocess
CLI RPCs appear through runtime diagnostics, not the in-process RPC list.

## Validate the failure path

The following deliberately returns failure **after** a normally passing modal
scenario has destroyed its fixtures. Use fresh binaries:

```sh
FUX_HARNESS_ARTIFACTS=/tmp/fux-failure-probe \
FUX_HARNESS_FAIL_AFTER=viewer-modals \
  cargo +stable run --manifest-path tools/xtask/Cargo.toml -- \
  scenario viewer-modals /absolute/path/to/fux
```

Inspect the printed directory's `failure.json`. Its error must say
`injected failure after scenario teardown`; an earlier assertion failure is a
different result. The final screen should retain the normal-input markers and the
diagnostics should contain transition metadata without those marker contents.
Leave `FUX_HARNESS_FAIL_AFTER` unset for ordinary verification.

Generated in-memory controller traces retain their own seed, minimized trace and
replay command; see [controller-trace-testing.md](controller-trace-testing.md).
The final integrated verification remains work in progress; see [codebase-improvement-report.md](codebase-improvement-report.md).
