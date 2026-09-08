# Make fux a first-class headless target: the protocol and agent surface pass

Execute a pass over fux's external protocols that makes a workspace fully drivable by a program
that never attaches a terminal, removes redundancy from the protocol surface, and turns the zor
integration from a polled, one-directional sidecar into something fux users can see. Implement
and verify the changes; do not stop at a plan. The deliverable is a pull request against `main`.

## Objective

fux 0.5.0 is already controllable without a viewer: the manager socket starts and kills
workspaces, the `FUXCTL2` workspace socket creates, focuses, kills, resizes, feeds and captures
panes, `list` carries stable ids, pid, cwd, title, progress, geometry, cursor, modes and exit
status, and `subscribe` streams eight event kinds. What is missing is every primitive an agent
needs to *wait* rather than poll, an incremental read, per-pane environment and size, named keys,
a global event stream and a way to ask the server about itself. Three schemas (manager, control,
attachment `control`) overlap for workspace commands, and aliases (`new` for `split`, `tab
select` and `tab select-id`) each cost a schema arm, a parser, a doc row and a fuzz surface. zor
observes a pane by polling `list` and an attributed `capture` ten times a second, re-emulating
the text in its own vt100 and printing OSC 7877 reports to a stdout nothing in fux displays; fux
has no code that recognises OSC 7877 at all, although zor's design assumes it does.

Targets, in order:

1. Agent primitives on the control socket: `wait`, an output sequence per pane with
   `capture {since}`, a row-structured capture, `env` and size on creation, key notation in
   `send-keys`, an `info` request, and workspace lifecycle events on the manager socket.
2. One schema for workspace management: the manager socket serves the control framing and the
   `workspace` command family; aliases that are pure spellings of another command are removed or
   reduced to CLI sugar over the canonical request.
3. Agent state visible in fux: OSC 7877 reports parsed from pane output the way OSC 9;4 progress
   already is, exposed in `list`, as a `pane.agent` event, in the attachment frame and in the bar,
   so `zor -- COMMAND` in a pane (or an agent emitting the OSC itself) lights up fux with no
   socket; `zor observe` stays supported and becomes optional.
4. Shared protocol fixtures: golden request, reply, event and frame files in the fux repository
   that fux's, koh's and zor's unit tests load, so a schema change is caught at test time in all
   three repositories rather than only when the real-binary suites run.
5. A headless end-to-end proof: a real agent-shaped session (create, run, wait, read, react,
   exit) driven by a Python harness with no pty attached, and `fux run -- COMMAND` as the CLI
   convenience over it.

There is no protocol versioning in fux from this pass on: no version numbers in prefaces,
hellos, descriptors or documents, no negotiation, no compatibility policy, no migration offer
for an older server and no version history tables. fux is nowhere near a public release and
every consumer (the CLI, the viewer, koh, zor, the harnesses) ships from the same tree or from a
pinned base plus a patch, so a schema is simply whatever the current tree defines and the
fixtures in section 5 pin it. The only thing a peer checks is a fixed magic preface (`FUX\n`
on the control and manager sockets, a `hello` without a version field on the attachment
socket) so that a stray connection from something that is not fux executes nothing; that is
framing hygiene, not versioning. A server older than its client shows up as a decode error and
is reported as "restart the session server"; nothing is ever stopped automatically. Remove the
versioning that exists today: the `FUXCTL2` number and `VERSION` constants, `hello.version`,
the protocol versions in descriptors, `src/daemon/migration.rs` and the interactive stop offer,
`tests/verify/migration.py`, the "no longer served" version paragraphs and tables in both
protocol docs, the `"version":6` pin in `dependency-patches/koh.patch` (replaced by a hello
without a version), and every `--version` flag of the measurement scripts. Rejection tests keep
proving that a wrong or partial preface and an unknown field reach no handler.

The contracts that do remain are behavioral: the attachment framing and frame semantics, the
control and manager commands, descriptors, the configuration file, the CLI, default bindings,
the viewer's documented behavior, the ordering guarantees in docs/design.md, the performance
table in docs/ecs-acceptance.md (no measure may regress beyond noise), MSRV 1.95, the lints
(`unsafe` forbidden) and the gate. Schemas may change freely as long as every consumer in the
inventory changes with them in the same pass and the fixtures show the diff.

## Starting point and authorization

- Baseline: `main` at the current tip (0.5.0, `f542411` at the time of writing; re-fetch and
  record the SHA you branch from). Read README.md, HANDOFF.md, docs/design.md,
  docs/security.md, docs/ecs-acceptance.md, docs/local-attachment-protocol.md,
  docs/local-control-protocol.md, `src/proto/control.rs`, `src/daemon/rpc.rs`,
  `src/server/connections.rs`, `src/terminal.rs`, `src/main.rs`, zor's `OBSERVATION-CONTRACT.md`
  and `src/observe.rs`, koh's `src/gateway/sessions_real_fux.rs`, and the `Cargo.toml` lints
  before touching code.
- Work on a branch (`proto/agent-surface` or similar) in an isolated worktree. Never push to
  `main`. This prompt authorizes local implementation, verification, documentation, independent
  reviewer subagents, commits on the branch, pushing the branch and opening one pull request
  (draft until the completion gate passes, then ready for review). It does not authorize
  merging, force-pushing `main`, touching personal sessions or runtime directories
  (`~/Library/Caches/fux-runtime`, `$XDG_RUNTIME_DIR/fux`), or killing any server the task did
  not start. Every real-process run uses disposable HOME/XDG directories.
- koh and zor are first-party but separate repositories at pinned bases with the patches in
  `dependency-patches/`. Changes fux needs from them (the unversioned hello and preface, fixture loading, an
  event-driven `zor observe`) are made as patches in `dependency-patches/` that
  `python3 tools/dependencies.py verify --build` reconstructs, exactly as the v6 pin was, and
  are listed in the PR as the follow-up PRs to open in those repositories. The fux branch must
  pass its gate with the pinned zor and koh plus those patches; nothing in this pass depends on a
  merge elsewhere. Machine-local `zor` and `references` symlinks in worktrees are ignored by
  path; never commit them.

## 1. Remove versioning, inventory the consumers

Do the removal listed in the objective first, as its own slice, with the gate green at its end:
the CLI, the viewer, the harnesses, the fixture-child suite, koh (through the patch) and zor
(through a patch if `observe.rs` needs one; today it sends `FUXCTL2\n`) all speak the unversioned
prefaces and hellos. Then inventory every consumer of each schema (the CLI aliases,
`tests/verify/*.py`, the fixture-child suite, koh's gateway tests, zor's `observe.rs`) and list
what each one reads, so the review can check nothing lost its shape when the schemas below
change.

## 2. Agent primitives (control protocol)

Add to `Request`/`Reply`/`Event`, all executed in the ordered step with viewer input and answered
with the existing reply envelope:

- `wait {id, pane, until, timeout_ms}` where `until` is one of `quiet {ms}` (no output for
  `ms`), `pattern {regex}` (matched against the visible screen's plain text after every change;
  the regex is bounded in length and compiled with a size limit, no new dependency unless the
  standard library cannot do it, in which case justify `regex-lite`), `exit`, or `seq {value}`
  (the pane's output sequence reached `value`). The reply says which condition fired, the pane's
  current sequence and exit status; `timeout_ms` is bounded (≤ 300 000) and a timeout is a
  `failed` reply with a dedicated code, not a hang. A wait is a deadline in the ECS (`Deadlines`),
  never a thread or a sleep; a pane that closes fails every wait on it with `not-found`; there
  are at most 64 pending waits per connection and a documented total, and a connection that
  closes cancels its waits.
- `seq`: a monotonic per-pane output sequence (advanced once per step in which the pane's
  retained grid changed) in `PaneSummary`, in `pane.output` events, and in `capture` replies.
- `capture {since: seq}` returns only rows that changed since `seq` (or everything when the
  server cannot know, with a flag saying so), and `capture {format: "rows"}` returns
  `[{row, text, wrapped}]` plus the cursor and `seq` instead of a formatted dump. The plain and
  attributed forms keep their exact bytes (zor and the harnesses pin them).
- `new`/`split {env: {K: V}, rows, columns}`: bounded environment (count and bytes, names
  validated like the daemon's sanitised environment; credential-like keys are not filtered here
  because the caller is the user, but document it), and the initial size of a tab created with
  no viewer attached (today 24×80 from `creation.rs` and `requests.rs`).
- `send-keys {keys: "…", notation: "escapes" | "keys"}` where `keys` uses the notation of
  `commands.rs` (`C-c`, `Enter`, `Escape`, `Up`, …) and is decoded by the same parser the
  bindings use; the default stays `escapes`.
- `info {id}` on both sockets: server pid, instance nonce, crate version, configured limits,
  runtime directory.
- Manager socket: `subscribe` for `workspace.opened`, `workspace.closed`, `workspace.retired
  {exit_status}` with the same queue rule as workspace subscriptions.

Every new request has its bounds in docs/security.md, a rejection test for each bound, a
deterministic `tests/ecs.rs` test for its semantics (waits across every deadline and every pane
exit order; `since` across resize and history eviction), a property test where a property exists
(`capture {since}` folded over any split of a byte stream equals the full capture), and a line
in the CLI (`fux wait`, `fux capture --since --rows`, `fux new --env`, `fux send-keys --keys`,
`fux info`).

## 3. One schema for workspace management

The manager socket keeps its path and the fixed preface but serves the control framing and
`Request::Workspace {list, new, kill}` plus `info` and `subscribe`; `resolve` becomes
`workspace new {name?}` with the documented default rule and the descriptor in its reply. Keep
`src/daemon/rpc.rs`'s `ManagerRequest` as the CLI's and the viewer's typed client over the same
frames or remove it if nothing needs it. `new` (the pane alias) becomes CLI sugar over `split`;
`tab select-id` folds into `tab select {tab}` beside `select {index}`; `workspace select` stays
(it is the only viewer-scoped action). The old arms are removed outright, not deprecated; `protocol_rejection.py` and the
fixture-child suite prove an unknown command or field gets the documented `unknown-command` or
`invalid-request` reply and reaches no handler.

## 4. Agent state in fux (OSC 7877)

Parse OSC 7877 in `terminal.rs` beside progress: the v1 schema in zor's
`OBSERVATION-CONTRACT.md`, bounded in size, applied in arrival order, with `state`, `agent`,
`flags` and the report's own fields kept as an `AgentReport` on the pane; malformed reports are
dropped and counted. Expose it as `agent` in `PaneSummary`, a `pane.agent` event (rate-limited
like `pane.output`), a field in the attachment pane update (koh's gateway tests
read `state.state.panes.<id>.cells[].text` and must keep passing against the new binary), and in the bar and the command column with
the same restraint as the progress indicator. `zor -- COMMAND` as a pane's argv must show its
state in an attached viewer within one frame interval of the report; `tests/verify/observer.py`
keeps proving `zor observe` still works against the same binaries, and a new harness proves the
in-band path with a script that emits the OSC. Nothing in fux spawns, supervises or trusts zor:
the report is presentation metadata, and docs/security.md says so.

Then the zor patch: `zor observe` subscribes to `pane.output` (with `seq`), captures
`format: "rows"` only when `seq` moved, drops the second emulation, and reads the pid from
`list` so `--pid` becomes optional. Measure zor's CPU per observed pane before and after with a
disposable server (`ps` over 60 s, idle and under a 20,000-line burst).

## 5. Shared protocol fixtures

Under `tests/verify/fixtures/` (or `docs/protocol/`) commit one file per schema and direction:
control requests and replies, events, manager frames, attachment client and server messages
including one full and one delta frame update, descriptors. fux's unit tests round-trip every
fixture through the real types (`deny_unknown_fields` both ways) and the encoders reproduce the
files byte for byte; a `tools/fixtures.py` regenerates them from the types so a schema change is a
visible diff. koh's gateway tests and zor's observe tests load the same files through patches in
`dependency-patches/` instead of the literals they carry today (the hello in
`sessions_real_fux.rs` and the JSON pointers in `observe.rs`). The fixtures are the contract the README points integrators at.

## 6. Headless proof and `fux run`

`tests/verify/agent_headless.py`, joining `tests/local_cli.rs`: with no pty anywhere, start a
server through `fux workspace new`, create panes with `env` and a size, run a command, `wait`
for a pattern, read `capture --since`, react with `send-keys --keys`, `wait` for `exit`, read the
exit status from the event and from `list`, kill the workspace, and assert the server exited.
Assert byte-exact captures against expected output, that every wait returned within its bound,
and that the process's fd count and the runtime directory are clean at the end. `fux run
[--workspace NAME] [--cwd DIR] [--env K=V] [--rows R --columns C] [--timeout MS] -- COMMAND…`
creates a pane, waits for exit, prints the final capture (rows format on request) and exits with
the command's status; it is implemented over the public requests only.

## 7. Preserve the contract, prove it

Every existing test keeps passing or is migrated with its assertion intact and an entry in an
assertion ledger: `tests/ecs.rs`, `tests/structure.rs`, `tests/local_cli.rs` and its harnesses,
`tests/zor_integration.rs`, the fixture-child suite, the koh gateway suites (against the new pin).
Rerun `tools/measure.py`, `tools/measure_frames.py`, `tools/measure_viewer.py` and
`tools/measure_memory.py` on both binaries; no measure in the 0.5.0 table may regress beyond
run-to-run noise (the OSC parser and the sequence stamp sit on the output path; profile them if
the burst CPU moves). Run the full gate on the final tree and record it in docs/ecs-acceptance.md:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
ZOR_BIN=$PWD/zor/target/debug/zor FUX_REQUIRE_ZOR_BIN=1 PROPTEST_CASES=2048 cargo test --locked -- --test-threads=1
cargo doc --no-deps --locked
cargo +1.95.0 check --all-targets --locked
cargo test --locked --manifest-path tests/verify/fixture-child/Cargo.toml
FUX_BIN=$PWD/target/debug/fux KOH_REQUIRE_FUX_BIN=1 cargo test --manifest-path references/koh/Cargo.toml --test gateway --locked
FUX_BIN=$PWD/target/debug/fux KOH_REQUIRE_FUX_BIN=1 cargo test --manifest-path references/koh/Cargo.toml --lib gateway:: --locked
tests/verify/release-package.sh --allow-dirty
python3 tools/dependencies.py verify --build
git diff --check
```

No new dependency without a measurement or a finding that justifies it; the lints stay as they
are.

## 8. Work in bounded slices

Suggested order: versioning removal and inventory → `seq`, `capture {since, rows}` and `info` → `wait` →
`env`, size and key notation → manager fold and alias removal → OSC 7877
and the attachment bump → fixtures and the koh/zor patches → headless harness and `fux run` →
docs. Each slice commits with its tests; a behavior is not done until its fixture, its doc row and
its bound test exist.

## 9. Independent review and acceptance

Use fresh reviewer subagents that did not implement the reviewed slice, once after the `wait`
slice and once on the complete branch diff against the merge base. They review whether any
version number, negotiation or migration path survived anywhere (code, docs, scripts, patches),
whether every new bound is enforced by code and a
test, whether `wait` can be made to hang, leak a deadline or fire on the wrong pane, whether
`capture {since}` can return stale rows after eviction or resize, whether the OSC 7877 path can
be abused by a hostile program (size, rate, unicode in the agent id, state flapping), whether the
fixtures are actually loaded by koh and zor, and every claim in the documents. Fix confirmed
P0/P1 and in-scope lower findings, document rejected findings with reasons, rerun affected checks,
and obtain a final review after fixes.

## Deliverables and completion

- The branch with slice-sized commits, pushed to origin as authorized above.
- One pull request against `main` whose description contains: the baseline SHA, the list
  of everything the versioning removal deleted, the consumer inventory, every new
  request and event with its bounds and its tests, the fixture list, the zor before/after CPU
  figures, the headless harness transcript, the performance re-measurement table, the gate
  summary, the assertion ledger, the review findings with dispositions, and the list of
  follow-up PRs to open in koh and zor with the patch contents. Open it as a draft; mark it ready
  only when the gate and the final review are clean.
- Updated README.md (the headless workflow, `fux run`, `fux wait`, the fixture directory, the
  zor in-band path), docs/design.md (agent state, waits as deadlines, the manager fold),
  docs/security.md (every new bound), both protocol docs (no version numbers or history anywhere, the new
  tables, the fixed prefaces), docs/ecs-acceptance.md (an "Agent surface" section with the evidence),
  CHANGELOG 0.6.0, a short HANDOFF.md, and `dependency-patches/` with the koh and zor patches.

Do not declare completion while any version number or negotiation remains in the tree, any
new request lacks a fixture, a bound test or a doc row, any
consumer in the inventory reads a shape that changed without its test changing, the koh or zor
suites fail against the pinned bases plus patches, or the PR is still a draft with known
findings. Do not merge, do not comment on other PRs, and do not push to `main`.
