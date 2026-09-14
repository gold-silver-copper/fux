# Finish the fux/zor boundary cleanup

Follow-up to the second boundary audit on `main` `0a5d46f` of
`https://github.com/gold-silver-copper/fux` (fux 0.8.0, zor 0.3.2, local-ipc 0.1.0). Governing
principle from the user: **fux is minimal; anything that can live in zor lives in zor.** fux
keeps only what requires owning the PTY, the process, the retained grid or the event log
(pane lifecycle, coherent capture, sequenced input with receipts, exit evidence, bounded
replay); every policy, convenience or workflow built on top of those belongs to zor. What
remains after the last audit is dead automation surface in fux, two fux commands that are
workflows rather than primitives, socket plumbing still duplicated between fux and zor, a
boundary test that guards names but not protocol shape, and housekeeping. Do the four items
below as **one pull request**
against `main`, branch `boundary/cleanup`, from a fresh worktree; one commit per item, each
building and passing the fast suites on its own; push when the whole branch passes
verification, open the PR, wait for hosted CI and report it. Do not merge, do not publish.
Keep the runtime of `/Users/kisaczka/Desktop/code/fux` unchanged.

Rules: no compatibility shims (removed forms are gone in the same PR, every in-repo consumer
moved in the same PR, no aliases, no dual paths, no deprecation markers); no single build, test
run or CI wait over five minutes; no batch campaigns; do not run the full 45-command headless
gate (leave its command in the report); per-crate lints stay as they are; regenerate
`crates/fux/tests/fixtures/multiplexer-boundary.json` only after confirming every fux
declaration change is generic; the `wrap` feature stays a default feature of zor and fux keeps
building zor with `--no-default-features --features cli`. Use one independent subagent to
review the complete diff for leftover compatibility paths, semantic drift and any wire change
outside item 1's stated removals; fix confirmed findings before pushing.

## 1. Delete dead automation surface in fux

All of the following have no consumer in `crates/fux`'s viewer/CLI beyond thin passthroughs and
none in `crates/zor` (verify each with grep before deleting; if a consumer exists, keep that
item and say so):

- `Request::Wait`, `WaitUntil::{Exit,Seq}`, `WaitFired`, `CommandResult::Waited`, the `Waits`
  and `PendingWait` resources and `MAX_WAIT_MS`/`MAX_PENDING_WAITS`/`MAX_WAITS_PER_PANE`, the
  wait dispatch in `ecs/systems/requests.rs`, the blocking-connection path in
  `server/connections.rs` (the branch that keeps a control connection open for a wait), the
  long read window in `main.rs` and the `fux wait` CLI subcommand, its fixtures, ECS tests and
  local-CLI scenario steps. Nothing in fux depends on it once `fux run` has moved (below).
- `InfoLimits` fields with no consumer anywhere: verify `retire_grace_ms`,
  `terminate_deadline_ms`, `output_event_interval_ms`, `frame_interval_ms`, `viewer_queue`,
  `subscriber_queue`, `frame_bytes`, `event_filters` and any other that only `fux info` prints;
  remove them from the reply and its fixture, keep the ones zor or the CLI actually read (pid,
  instance nonce, version, runtime dir, and the capture/input bounds a consumer needs to size
  requests). If a limit is needed to size a request correctly, it is a consumer and stays.
- The `events` filter on `Request::Subscribe` (zor never sends one) and the event kinds with
  no consumer: `pane.title`, `client.attached`, `client.detached`. Update `EventKind`, the
  subscriber filtering code, `fux subscribe`'s CLI syntax, fixtures, docs. zor already ignores
  unknown kinds, so nothing on its side needs a change; confirm with the `zor-events` scenario.
- `fux run` and `fux final` move to zor. `fux run` (create a throwaway workspace, run a
  command, wait for exit, print the final screen, exit with the child's status, `--timeout`,
  `--rows/--columns/--env/--cwd`) is a workflow over `split` and the manager `final` request;
  implement it as `zor run [flags] -- <command>` in `crates/zor` (a task-free convenience that
  uses zor's existing fux client, launch and final-record readers; it must not require `zor
  serve`), with the same observable behavior and its own tests, then delete `run_command`,
  `run_in_workspace`, the `run` and `final` CLI subcommands and their scenarios from fux (the
  `run-command` xtask scenario moves to a `zor-run` scenario driven from the automation
  integration test). Keep in fux only what zor cannot do: `ManagerRequest::Final`, the retained
  final records and their bounds. The zor `rules`/`agent` flags do not apply to `zor run`.
- `progress` on `CaptureSnapshot` and the cells reply, and the OSC 9;4 parser in fux's
  `terminal.rs`, unless zor's `rules::view::Captured` reads `progress` — check
  `crates/zor/src/rules/view.rs` and `crates/zor/src/rules/**` first. If zor's rules use it,
  keep it and record that as the consumer; if only the standalone wrapper's own emulator parses
  progress, delete fux's.

While deleting, apply the principle to anything else you find in `crates/fux/src/main.rs`
or the protocol that is a workflow over primitives rather than a primitive (list it in the
report with the reason it stayed or moved). Update `docs/local-control-protocol.md`,
`docs/design.md` (the ownership table states the principle), `docs/security.md`, README,
HANDOFF and `CHANGELOG.md` (fux 0.9.0, breaking, naming every removed request, field, event
kind, flag and subcommand; zor's changelog names `zor run`). Verify with fux lib/ECS/fixtures/structure/boundary suites, all local CLI
scenarios, the new `zor-run` scenario, and the zor scenarios that touch observation
(`zor-events`, `zor-service`, `zor-launch`, `zor-tasks`).

## 2. local-ipc absorbs the remaining duplicated plumbing

Compare `crates/fux/src/proto/socket.rs`, `crates/fux/src/daemon/rpc.rs`,
`crates/fux/src/daemon/paths.rs` with `crates/zor/src/fux.rs` and `crates/zor/src/service.rs`
and move into `crates/local-ipc` (0.2.0) what is the same discipline:

- deadline-bounded connect (`Connecting` + poll until the deadline, retrying `EINTR`);
- deadline-bounded full write of a byte slice;
- newline-delimited frame reader with a maximum frame size and an absolute deadline,
  retaining partial data across polls;
- runtime-directory discovery given an application name (`XDG_RUNTIME_DIR/<name>`, the macOS
  cache fallback). fux and zor currently disagree on the macOS fallback path
  (`crates/fux/src/daemon/paths.rs` versus `crates/zor/src/fux.rs`): resolve the disagreement
  by making zor use fux's path for fux's sockets (zor must find fux where fux puts it) and
  state it in the changelog as a bug fix; zor's own service directory keeps its own name.

Keep each crate's preface bytes, limits, error texts and timings where they differ; do not
unify behavior that differs unless it is the macOS path bug above. The crate stays free of fux
and zor names and `forbid(unsafe_code)`. All socket tests in both crates pass with unchanged
assertions except where the macOS path fix requires a new expectation. Report lines removed
from each crate.

## 3. A protocol-consumer fixture

Add `crates/fux/tests/fixtures/control-consumers.json` listing every control-protocol request,
reply variant, event kind, `list`/`capture` field and manager request, each with its consumers:
`"viewer"`, `"cli"`, `"zor:<file>"`, or `"none"`. A `"cli"`-only consumer is acceptable for a
primitive (the CLI is fux's debugging surface) but not for a workflow; note which is which. Add a test in
`crates/fux/tests/agent_boundary.rs` (or a new `tests/protocol_consumers.rs`) that (a) derives
the current protocol surface from the source (the enums and structs in `src/proto/control.rs`
and `src/daemon/rpc.rs`, using the same lightweight parsing the boundary test already does)
and fails when an item is missing from the fixture or the fixture names an item that no longer
exists, and (b) fails when any item's consumer list is `"none"`, with a message saying to
delete the item or record its consumer. Grep-verify the zor consumers you record. This test is
the gate that would have caught item 1 automatically. Keep it deterministic and under one
second.

## 4. Housekeeping

- `crates/zor/Cargo.toml`: the comment near the `wrap` feature still says it is off by
  default; make it say it is a default feature and that fux builds zor without it.
- Rename `crates/fux/tests/zor_integration.rs` to `automation_integration.rs` (and the
  scenario dispatch names it depends on, `tools/xtask/checks.json`, CI, docs); the test
  content stays; the `ZOR_BIN`/`FUX_REQUIRE_ZOR_BIN` variables stay because they name the
  binary, not the boundary.
- Bump versions: fux 0.9.0, zor 0.4.0 (`zor run`, macOS path fix, local-ipc 0.2.0),
  local-ipc 0.2.0;
  changelog entries for each. Publish order after merge: local-ipc, zor, fux.

## Reporting

Write `.verification/boundary-cleanup/REPORT.md` in the worktree with: the PR URL and hosted
CI result; per item the commit, lines added/removed per crate, and any deliberate deviation
with its reason; the consumer fixture's summary (how many items, how many per consumer); the
reviewer's disposition; the grep-backed statement that no removed identifier remains outside
changelogs and archived evidence; and the gate command:

```sh
cargo run --locked --manifest-path tools/xtask/Cargo.toml -- dependencies verify --build --headless
```
