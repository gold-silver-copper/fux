# Make fux robust: hostile inputs, exhaustion, long runs, and executed Linux evidence

Execute a hardening pass over fux that proves, with fuzzers, hostile peers, soak runs and a
second platform, that the server and the viewer survive everything their inputs can do to them,
and fixes what they find. Implement and verify the changes; do not stop at a plan or a list of
findings. The deliverable is a pull request against `main`, not a push to it.

## Objective

fux 0.5.0 is correct on the paths its tests exercise and fast on the paths its measurements
cover, but its inputs are only bounded where someone thought to bound them. Two reviews of the
performance pass found the viewer allocating from wire-supplied dimensions before checking them
and a cell budget that refused a legitimate tab switch; both were found by reading, not by a
test. Every parser in the tree is reachable by a peer that is not fux: the attachment protocol
from a koh gateway carrying a remote user's bytes, the control protocol from any process running
as the user, the manager protocol, the descriptor files, the configuration file, and above all
the emulator, which is fed by arbitrary programs. Runtime evidence exists for macOS only; Linux
and Android are configured CI targets that have never run. The randomized ECS test runs for
seconds, and nothing has ever exercised a session through millions of operations of viewer
churn, killed viewers, stopped readers, full ptys and processes that refuse to die.

Target these areas, in order:

1. Fuzz every parser and every state machine that consumes bytes from outside: the control
   string filter and the emulator feed, the attachment frame decoder and the frame update
   application on the viewer, the control and manager frame decoders, key notation, descriptor
   and configuration files, the viewer's input classifier and copy session.
2. Hostile peers: a server that lies to a viewer, a viewer that lies to a server, a control
   client that floods or stalls, a program that emits pathological terminal output.
3. Exhaustion: a stated memory and handle budget per server that holds under 128 panes of full
   history, 64 viewers, 64 control connections and 1,024-event subscribers at once, with the
   documented limits unchanged; history storage that does not cost 32 bytes per cell of blank
   space.
4. A fast soak: drive the ECS in process, as fast as the machine allows, through millions of
   random operations and injected faults in a few minutes, checking invariants after every
   step; then a few minutes against a real server for the faults only the operating system can
   produce.
5. Linux: the whole gate executed on Linux, with every platform difference fixed or documented,
   and the CI matrix turned from configuration into evidence.

Backwards compatibility of internal APIs is not a concern. The external contracts are: the
attachment protocol v6, the control protocol `FUXCTL2`, the manager protocol, descriptor files,
the configuration file, the CLI surface, the default bindings and the viewer's documented
behavior. A protocol may gain a bound (a maximum the other side must respect) without a version
change if it only tightens what a conforming peer already sends; anything else is a version bump
handled the way 0.5.0 did it. Correctness, the ordering guarantees in docs/design.md, the
performance numbers in docs/ecs-acceptance.md (no measure may regress beyond noise), MSRV 1.95
and the verification gate are concerns.

## Starting point and authorization

- Baseline: `main` at `f542411` (the 0.5.0 merge). Re-fetch and record the actual SHA you branch
  from. Read README.md, HANDOFF.md, docs/design.md, docs/security.md, docs/ecs-acceptance.md,
  docs/local-attachment-protocol.md, docs/local-control-protocol.md, the `Cargo.toml` lints and
  the CI workflows before touching code.
- Work on a branch (`hardening/inputs-and-soak` or similar) in an isolated worktree. Never push
  to `main`. This prompt authorizes local implementation, fuzzing and soak runs, verification,
  documentation, independent reviewer subagents, commits on the branch, pushing the branch and
  opening one pull request (draft until the completion gate passes, then ready for review). It
  does not authorize merging, force-pushing `main`, editing koh or zor beyond what a tightened
  bound needs (which should be nothing), touching personal sessions or runtime directories
  (`~/Library/Caches/fux-runtime`, `$XDG_RUNTIME_DIR/fux`), or killing any server the task did
  not start. Every real-process run uses disposable HOME/XDG directories; no single soak or
  fuzz run may exceed ten minutes of wall time.
- Owner repositories (`references/koh`, `zor/`) stay at their pinned bases with their patches.
  Machine-local `zor` and `references` symlinks in worktrees are ignored by path; never commit
  them.
- Linux runs may use GitHub Actions on the pushed branch (the workflows exist), a local
  container, or both; say which, and keep the logs.

## 1. Inventory and bounds

Before any change, write down every input boundary with its parser, its current bounds, what
enforces them, and what happens on violation (error, disconnect, silent drop, panic path): the
PTY reader and `terminal.rs` (control-string filter, `vt100`, callbacks, title, clipboard,
progress reports), `proto/attach.rs` (frame length, `read_frame`, `FrameUpdate`/`PaneUpdate`/
`WireCell` decoding and `within_bounds`, `Frame::apply`, `ViewReply`), `proto/control.rs`
(`decode_request_frame`, `validate`, `decode_key_bytes`), `server/connections.rs` (`read_line`,
subscriptions, the accept loops and their limits), `daemon/` (descriptors, manager requests,
startup channel, paths), `config.rs` and `commands.rs` (key notation), and the viewer's
`client/input.rs`, `controller.rs`, `copy.rs`, `render.rs` and `hints.rs` for what a server
can make them do. Record the memory each bound allows at its maximum (for example: 128 panes ×
10,000 rows × columns × 32 bytes) and the sum; that sum is the current worst case and must be
stated in docs/security.md.

## 2. Fuzzing

Add `cargo fuzz` targets under `fuzz/` (nightly toolchain only for fuzzing; the crate itself
stays on stable and MSRV 1.95) and, for what libFuzzer cannot drive well, `proptest` generators
in the existing suites:

- `terminal_feed`: arbitrary bytes, split at arbitrary points, into `ServerTerminal::process`,
  then `refresh_grid`, `grid().update(None)` and `PaneView::from_update`; assert no panic, the
  filter's bounds, and that the update applies. Include resizes between chunks and history reads
  (`with_history_screen`, `capture`) at arbitrary offsets.
- `attach_decode`: arbitrary bytes into `read_frame` and `FrameUpdate`/`ViewReply` decoding,
  then `Frame::apply` onto a fuzz-chosen held frame; assert the documented bounds and that
  allocation stays within them (instrument with a counting allocator in the fuzz target).
- `control_decode`, `manager_decode`, `key_notation`, `config_parse`, `descriptor_parse`.
- `viewer_input`: arbitrary bytes through `PrefixFilter::feed`/`resolve_escape` and
  `Controller::feed` against a fuzz-chosen frame; assert byte exactness of ordinary input, that
  no byte is lost or duplicated across splits, and that modes cannot be left in a state the
  documented keys cannot exit.
- `copy_session`: arbitrary sequences of keys, drags, installs and live refreshes.
- `outbox_merge`: arbitrary sequences of updates merged in arbitrary groupings must apply to the
  same frame as applying them in order (the property test from 0.5.0, widened to arbitrary
  layouts, sizes and pane sets).

Run each target for at least ten minutes on the reference machine with the corpus seeded from
the existing tests and harness traffic; commit the corpora and every crash's minimized reproducer as
a regression test in the normal suite. Report findings, fixes and run times.

## 3. Hostile peers

Write Python harnesses under `tests/verify/` (the same style as the existing ones, disposable
directories, real binaries) that play each role badly and assert the documented outcome:

- A hostile server against the real viewer: oversized and undersized frames, wrong hello,
  updates naming panes the viewer does not hold, deltas of the wrong size, blank runs crossing
  rows, 129 panes, cells with control characters, a stalled `hello`, a frame that never
  finishes, `exited` mid-request, replies without frames, a flood of updates faster than the
  terminal can paint. The viewer must either apply the update or exit with one diagnostic line,
  restore the terminal (raw mode, alternate screen, mouse and keypad modes, cursor) in every
  case, and never allocate beyond the frame budget (measure its peak RSS).
- A hostile viewer against the real server: oversized inputs, mouse reports with impossible
  coordinates, thousands of queued requests behind a barrier, a client that never reads (the
  outbox depth rule), a client that sends a frame length and stops, reconnect storms (attach
  and disconnect as fast as the socket allows), the 65th viewer, a hello for every version
  0–7. Panes must stay intact and other viewers unaffected; assert with a second, honest viewer.
- A hostile control client: frames of 1 MiB + 1, `subscribe` with a full queue never drained,
  64 connections that stall after the preface, requests for panes of other workspaces, `kill`
  from a workspace connection, `send-keys` of 64 KiB, `capture` with maximal scrollback on a
  full history, `list` on 128 panes in a tight loop. Assert the reply codes documented in the
  control protocol and that the session's latency for an honest viewer stays within 2× the
  measured baseline while the hostile client runs.
- A hostile program in a pane: the worst terminal output you can write (unterminated control
  strings, C1 introducers inside UTF-8, resize storms through `stty`, bells, title changes at
  kilohertz, OSC 52 floods, 100,000 columns of wide characters, cursor moves off screen, DECRQM
  floods that generate host replies), and a process that ignores SIGHUP, forks a grandchild that
  reparents to init, and closes its pty slave while alive. Assert the reap gate, the exit status,
  the bounded host replies and that the server's RSS returns to its pre-burst baseline within
  the documented grace.

## 4. Exhaustion and memory

- State a server memory budget in docs/security.md (for example "at the documented limits a
  server holds at most N MiB of history plus M MiB of grids and frames") and make it true.
  History is the largest term: `vt100` keeps 32 bytes per cell, so a blank 200-column row costs
  6.4 KB. Evaluate keeping history outside the emulator in a compact form (run-length rows of
  text and style spans, or the emulator's own scrollback trimmed to a few hundred rows with the
  rest owned by fux), keeping `view` reads, `capture` and copy mode byte-for-byte identical in
  what they return (the existing tests and harnesses pin them). Report bytes per row before and
  after with `tools/measure_memory.py`, and the maximum for 128 panes at 200 columns.
- Handles: file descriptors per pane (pty master, dup'd reader, writer), per viewer, per
  control connection, per subscription; the process's `RLIMIT_NOFILE` on both platforms; what
  happens at the limit (a clean `limit` error, never a panic or a wedged accept loop).
- Threads: two per pane today; state the maximum and confirm it is reachable and survivable
  with 128 panes.
- Queues: every bounded channel, outbox and subscriber queue with its depth and the behavior
  when full, verified by a test that fills it.

## 5. Soak, in minutes, not hours

The ECS is a pure function of its inbound messages and the clock: `Session::step` takes a batch
of `Inbound` messages and a time and returns effects, with no sockets, sleeps, threads or
processes. That is the soak. Add `tests/soak.rs` (a test binary run explicitly, not part of the
default gate) whose harness reuses the one in `tests/ecs.rs` and drives a `Session` as fast as
the machine allows for a configurable wall-clock budget (default three minutes, `FUX_SOAK_SECONDS`)
with a seeded generator over everything the protocols can do: workspace and tab creation,
splits, kills, renames, focus and resize, viewer attach, detach, resize and workspace switch,
input, mouse reports with stale and current generations, history reads, control and manager
requests naming live, dead and never-existing ids, spawn completions delivered late, out of
order or failed, pane output including the pathological byte sequences from section 3, EOF and
exit reports in every order relative to completions, time skips across every deadline (retire
grace, terminate deadline, frame interval), viewer queues filled to the barrier limit, viewers
that never read (apply the outbox merge to what they would have received and assert it still
applies), and shutdown followed by a fresh `Session` (ids never reuse, nothing carried over).
After every step it asserts `check_invariants`, that every effect names a live public id or is
one of the documented late-completion effects, that every viewer's retained frame applies and
stays valid, that the retained-message count is zero, and that the World's entity counts and the
emulator memory of every pane stay within a band of their values after the first ten seconds
(a leak in the ECS shows up as growth within the run). It reports steps per second, operations by
kind and the seed. A failure prints the seed and the failing step; a deterministic replay from
the seed to that step, bisected to the shortest failing prefix, becomes a test in `tests/ecs.rs`
before the fix.

Then a short real-process run, `tools/soak.py`, bounded to five minutes, for the faults only the
operating system produces: SIGKILL of viewers mid-frame, SIGSTOP/SIGCONT of a viewer for
seconds (the outbox and the pty must survive), a viewer that stops reading, pane processes that
exit with every signal, `kill -9` of a pane's process group, a pty closed from the slave side,
and `SIGTERM` of the server at a random moment followed by a fresh start in the same directory.
It checks the control listing against its own model, that every pane process it started is
either alive and listed or reaped, that no socket or descriptor file is leaked, and that the
server's RSS and fd count return to a band after each fault.

Both runs are documented commands with their seeds; the gate runs a thirty-second in-process
soak, `nightly.yml` a three-minute one and a two-minute real-process one.

## 6. Linux

Run the complete gate on Linux: the root suites with the real zor, the fixture-child suite, the
koh gateway suites with a real fux, the packaging script, the dependency reconstruction, the
measurement scripts (`ps` fields, `/proc` wakeups, `sample` absent), and the hostile and soak
harnesses. Expect and resolve: pty buffer sizes and read chunking (the stream wait was tuned
against macOS's ~11-byte reads), `TIOCSWINSZ` and `SIGWINCH` timing, `/proc/self/fd` versus
`/dev/fd`, `getpeereid` versus `SO_PEERCRED`, `flock` semantics, `XDG_RUNTIME_DIR` present and
mode 0700 by default, `O_NOFOLLOW` on directories, `unicode-width` and terminal differences in
the Python harnesses, and timing sensitivity under a slower CI runner. Record the Linux numbers
from `tools/measure.py` and `measure_frames.py` next to the macOS ones. Make the GitHub Actions
workflows produce a run on the pushed branch and keep its link in the report; if a job cannot
run there (no zor or koh checkout on the runner), make the required integrations run through
`tools/dependencies.py apply` in the workflow rather than skipping.

## 7. Preserve the contract, prove it

Every existing test keeps passing or is migrated with its assertion intact and an entry in an
assertion ledger: `tests/ecs.rs` (21 tests including the randomized sequence at 2,048 cases),
`tests/structure.rs`, `tests/local_cli.rs` with its six harnesses, `tests/zor_integration.rs`,
the fixture-child suite, the koh gateway suites. New harnesses join `tests/local_cli.rs` so they
run in the gate; fuzz runs are documented commands with committed corpora, not gate steps, except
a 60-second smoke per target in `nightly.yml`; the in-process soak runs for thirty seconds in the
gate and three minutes nightly.

Performance acceptance: rerun `tools/measure.py`, `tools/measure_frames.py`, `tools/measure_viewer.py`
and `tools/measure_memory.py` on both binaries; no measure in the 0.5.0 table may regress beyond
run-to-run noise, and bytes per history row must fall if section 4 changed the representation.

Run the full gate on the final tree on macOS and on Linux and record both in docs/ecs-acceptance.md:

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
python3 tools/dependencies.py verify
git diff --check
```

The crate's lints stay as they are; `unsafe` stays forbidden. Fuzzing may add `cargo-fuzz`,
`arbitrary` and `libfuzzer-sys` under `fuzz/` only (its own crate, not a dependency of fux);
nothing else is added without a measurement or a finding that justifies it.

## 8. Work in bounded slices

Suggested order: inventory and the memory budget statement → the in-process soak (it is cheap
and finds ECS defects first) → fuzz targets with corpora (run, fix, add regression tests) →
hostile-peer harnesses (write, run, fix) → exhaustion tests and the history representation if it
pays → the real-process soak → Linux (execute, fix, record) → docs. Each slice commits with its findings and their fixes named;
a finding is not closed until its reproducer is a committed test.

## 9. Independent review and acceptance

Use fresh reviewer subagents that did not implement the reviewed slice, once after the hostile-
peer slice and once on the complete branch diff against the merge base. They review whether each
fix closes the whole class rather than the reproducer, whether a bound can still be exceeded by
a different encoding, whether the soak's model is strong enough to notice a leak or a lost pane,
whether the Linux evidence is real (a run link, not a green badge on an unexecuted matrix), and
every claim in the documents. Fix confirmed P0/P1 and in-scope lower findings, document rejected
findings with reasons, rerun affected checks, and obtain a final review after fixes.

## Deliverables and completion

- The branch with slice-sized commits, pushed to origin as authorized above.
- One pull request against `main` whose description contains: the baseline SHA, the input
  inventory with bounds before and after, every finding (fuzz, hostile, exhaustion, soak, Linux)
  with its reproducer test and fix, the memory budget before and after, the soak run records
  (steps per second, operations by kind, seeds, outcome, wall time), the Linux run link and the
  two gate summaries, the performance
  re-measurement table, the assertion ledger, and the review findings with dispositions. Open it
  as a draft; mark it ready only when both gates and the final review are clean.
- Updated README.md (how to run the fuzzers and both soaks), docs/design.md (history
  representation if changed), docs/security.md (the budget and every bound), docs/ecs-acceptance.md
  (a hardening section with the findings and evidence), CHANGELOG (0.6.0 if the history
  representation or a bound changed, 0.5.1 otherwise), a short HANDOFF.md, and `nightly.yml`
  with the fuzz and soak smokes.

Do not declare completion while any finding lacks a committed reproducer, any bound in
docs/security.md is not enforced by code and a test, the Linux gate has not actually executed,
or the PR is still a draft with known findings. Do not merge, do not comment on other PRs, and
do not push to `main`.
