# Make fux fast: measured, end-to-end performance work

Execute a measurement-driven performance pass over fux that lowers input-to-screen latency,
frame cost, output throughput cost and memory without losing a capability, a guarantee or a test
assertion. Implement and verify the changes; do not stop at a plan or a profile. The deliverable
is a pull request against `main`, not a push to it.

## Objective

fux 0.4.0 is correct and idle-free (0.00 s CPU per 10 s with an attached viewer), but every
keystroke still travels through a full-frame path: the server derives a complete `view::Frame`
for each dirty viewer (every cell of every visible pane as a JSON object with text, kind and
style), validates it, serializes it into a length-prefixed JSON frame (up to 16 MiB allowed), the
viewer deserializes and validates it again, recomposes the whole ratatui buffer and diffs it
against the previous paint. On an 80×24 terminal that is on the order of 100 KB of JSON and two
full validations per keypress; on a 200×60 terminal it is six times that. Two viewers on the same
tab derive the same pane views twice. Output feeding copies every PTY chunk through a control-
string filter into a new buffer before the emulator sees it, and the pane's history keeps 10 000
rows of cells per pane in full.

Measured at 0.4.0 on the reference machine (release build, `tools/measure.py`, 80×24, one
viewer): startup 18–43 ms, idle CPU 0.00 s/10 s, RSS 6.4 MiB at start and 35–36 MiB after a
20 000-line burst, burst to quiescence 0.13–0.16 s, input→frame latency median 3–8 ms and p95
8–11 ms (100 samples). Those are the numbers to beat, and the ones that must not regress.

Target these areas, in order of expected payoff:

1. Frame derivation and transport: send what changed, not the whole screen, and derive each
   pane's view once per step however many viewers show it.
2. The viewer's receive path: decode, validate and paint proportionally to the change.
3. Output feeding: zero-copy or single-copy from the PTY reader to the emulator, bounded work per
   step, no per-chunk allocations that the emulator does not need.
4. Memory: history and cells stored at the size the feature needs, measured per pane and per
   viewer, with the documented limits unchanged.
5. Wide, tall and many-viewer cases: cost must scale with the change, not with the screen area
   times the viewer count.

Backwards compatibility of internal APIs is not a concern. The external contracts are: the
control protocol `FUXCTL2`, the manager protocol, descriptor files, the configuration file, the
CLI surface, the default bindings and the viewer's documented behavior. Those do not change.
The attachment protocol may change (see section 3): it is versioned for exactly this reason, and
koh only carries it as an opaque stream. Correctness, the deterministic ordering guarantees in
docs/design.md, MSRV 1.95 and the verification gate are concerns.

## Starting point and authorization

- Baseline: `main` at `fa0c286` (the 0.4.0 merge). Re-fetch and record the actual SHA you branch
  from. Read README.md, HANDOFF.md, docs/design.md, docs/ecs-acceptance.md (the "Dependencies and
  performance" and 0.4.0 sections), docs/local-attachment-protocol.md, `tools/measure.py`, the
  `Cargo.toml` lints and the CI workflows before touching code.
- Work on a branch (`perf/frames-and-feeding` or similar) in an isolated worktree. Never push to
  `main`. This prompt authorizes local implementation, measurement, verification, documentation,
  independent reviewer subagents, commits on the branch, pushing the branch and opening one pull
  request (draft until the completion gate below passes, then ready for review). It does not
  authorize merging, force-pushing `main`, touching personal sessions or runtime directories
  (`~/Library/Caches/fux-runtime`, `$XDG_RUNTIME_DIR/fux`), or killing any server the task did not
  start. Use disposable HOME/XDG directories for every real-process measurement and test; run
  measurements on a quiet machine and say when they were not.
- Owner repositories (`references/koh`, `zor/`) stay at their pinned bases. The koh patch pins the
  attachment version number (`dependency-patches/koh.patch`, `"version":5` and `Some(5)`); if the
  attachment protocol is bumped, update that patch the same way 0.3.1/0.3.2 did, re-export it with
  `tools/dependencies.py export`, and re-run the koh gateway suites. zor consumes the control
  protocol only and must need nothing.
- Machine-local `zor` and `references` are symlinks in worktrees and are ignored by path; never
  commit them.

## 1. Baseline evidence and hot-path inventory

Before any change, record reproducible baselines and keep the commands in the report:

- `tools/measure.py` as it is (three alternating runs against the baseline release binary and,
  later, the branch binary; report ranges, not single runs).
- Extend `tools/measure.py` (or add `tools/measure_frames.py`) to also report, per configuration:
  bytes on the attachment socket per keystroke (median and maximum over 100 keystrokes), server
  CPU seconds per 1 000 keystrokes, server and viewer CPU seconds for a 20 000-line burst, and the
  same with 2, 4 and 8 viewers attached to one tab. Configurations: 80×24, 200×60, and one 200×60
  viewer plus one 80×24 viewer on the same tab (the smallest-viewer rule sets the pane size; the
  larger viewer still receives frames). The script uses the raw attachment protocol like
  `measure.py` does; keep it dependency-free Python.
- A viewer-side measurement: run the real `fux` viewer on a pty (the way `tests/verify/viewer.py`
  does), feed a `yes`-style burst and a keystroke loop into the pane, and record viewer CPU
  seconds and bytes written to the pty. The compositor's cost is invisible to the socket-only
  script.
- Profiles of the server and the viewer under the keystroke loop and the burst (`cargo build
  --profile profiling` with debug info if you add such a profile, `samply` or `sample` on macOS,
  `perf` on Linux if available). Name the top functions by self time in the report; they are the
  inventory. Expected suspects, to be confirmed or dismissed by the profile, not assumed:
  `PaneView::from_screen` and `Cell::from_vt100` (allocation per cell, a `String` per cell),
  `Frame::valid`/`PaneView::valid` (grapheme and width computation per cell, run twice per
  frame), `serde_json` encode of the frame and decode in the viewer, `encode_frame`'s bounded
  writer, `ViewerOutbox` coalescing (a frame replaced after being fully built and serialized),
  `client::render::compose` (full buffer rebuild) and `Buffer::diff`, `ControlStringFilter::process`
  (byte loop with a fresh `Vec` per chunk), `ServerTerminal::process` catching unwinds per chunk,
  `apply_pane_output`'s per-message title comparison, `resolve_layout` and `paint_separators` on
  layout changes, `with_history_screen` for `view` reads and captures.
- For each hot path: current cost with numbers, the proposed change, the expected effect, the
  tests that pin its behavior, and the risk to ordering guarantees.

## 2. Server: derive and send only what changed

- **Per-step pane view cache.** A pane's view (`PaneView` or its successor) is derived at most
  once per step and shared by every viewer's frame that includes it; a pane that did not change
  since the last step reuses the previous view. The `Pane.dirty` flag already says which panes
  changed; extend it (or the emulator) to say *what* changed: the vt100 screen can report per-row
  dirtiness or the diff between two screens — use that instead of re-reading every cell.
- **Delta frames.** Replace "every dirty viewer receives a full frame" with a frame that carries
  full state only when the viewer's picture is unknown (attach, resize, tab switch, workspace
  switch, generation mismatch, a coalesced or dropped frame) and otherwise carries the changed
  rows or cells per pane plus the bar/tab/focus metadata. The viewer applies deltas onto its last
  full frame and can always ask for a full frame. Keep the per-viewer `generation` (mouse hit tests
  depend on it) and the rule that a frame is queued before the replies it promises; a delta is a
  frame for that rule. `ViewerOutbox` coalescing must merge deltas correctly or fall back to a
  full frame; it must never drop a delta while keeping a later one.
- **Cells.** Stop allocating a `String` per cell. Represent a row as a compact run-length or
  packed structure (text bytes for the row plus per-cell width/style runs, or a `Cell` with an
  inline small string) and make the wire form the same shape so encoding is a copy, not a tree
  walk. Blank cells with default style must cost nothing to send.
- **Validation once.** Validate on construction (the server builds the frame, so it can build it
  valid by construction and assert in debug builds) and validate cheaply on receipt: bounds and
  counts, not a grapheme segmentation of every cell. Keep the documented invariants (one grapheme
  per cell, width 1 or 2, no control characters) enforced where cells are produced.
- **Snapshot phase work.** `publish_frames` iterates viewers in id order and rebuilds
  `tab_entities` and the tab list per viewer; build the per-tab and per-workspace metadata once
  per step. Do not clone `bindings` into every frame; send the registry once at attach and when
  it changes.
- **Output feeding.** The reader thread hands 8 KiB chunks through a bounded channel as
  `Inbound::PaneOutput { bytes: Vec<u8> }`; the output system runs the control-string filter into a
  new `Vec` and then the emulator. Feed the emulator in place (filter into a reusable scratch
  buffer owned by the pane, or make the filter a streaming adapter the parser reads through), keep
  the `catch_unwind` but per step or per pane rather than per chunk if the measurement shows it
  matters, and compare titles by a change flag from the callbacks rather than by string comparison
  per chunk. Merge consecutive chunks of the same pane within a step before feeding when that is
  cheaper. The per-pane FIFO order and the "EOF and exit after every earlier chunk" rule stay.
- **Budgets and fairness.** `PANE_BUDGET`, `INGRESS_BUDGET` and the channel depths were chosen
  without measurement; measure the burst with one and with eight panes streaming and tune them
  with numbers, keeping the guarantee that a hot pane cannot starve input, exits, timers or
  signals (the fixture stdin/backpressure test and the acceptance table pin it).
- **Listing and capture.** `list` builds `PaneModes` and cursor data for every pane; `capture`
  and `view` reads swap the emulator's scrollback offset twice. Make them proportional to what was
  asked (a `list` of 128 panes must not touch 128 emulators' full screens), without changing the
  `FUXCTL2` reply shapes.

## 3. Attachment protocol

A delta frame, a packed cell encoding and a registry-once rule change the wire format. That is a
new attachment protocol version (v6), and the rules from 0.3.1/0.3.2 apply:

- Bump `proto::attach::VERSION`, document v6 fully in docs/local-attachment-protocol.md (message
  shapes, when a full frame is sent, how a delta is applied, validation rules, limits), and keep
  the v5 document section as history in git only.
- The hello mismatch path must still produce the `Unsupported` error that lets the launcher offer
  the incompatible-server dialog; `tests/verify/migration.py` and `protocol_rejection.py` pin it.
- Update the koh patch to the new version number, re-export it, and run the koh gateway suites
  against the new binary; koh must need no other change because the stream stays opaque.
- Keep JSON unless the measurement shows encoding is the bottleneck after the delta and packed-
  cell changes; if a binary encoding is proposed, it needs a number that justifies a dependency
  and a compatibility argument for koh's fixture that inspects frame state (see
  `frame_state_shape_is_stable_for_gateway_consumers` in `proto/attach.rs` — that test defines
  what gateway consumers may rely on; change it deliberately or keep the shape).
- Frame limits: the 16 MiB server frame ceiling exists for a full 512×512 frame; a delta must have
  its own bound and a viewer must reject a delta that does not fit its last full frame.

## 4. Viewer

- Apply deltas into a retained `Frame` and repaint only the rows a delta touched plus the bar;
  keep `Buffer::diff` as the final safety net or replace it with direct row painting if the
  measurement favors it. A full frame still repaints everything.
- Decode without a second full validation (section 2); keep the cheap structural checks that stop
  a hostile server from making the viewer index out of bounds (the lints deny indexing anyway).
- `compose` rebuilds the separators grid and the bar every frame; cache the separator layer per
  layout generation and repaint the bar only when its text changed.
- The escape/notice/request deadlines are computed every loop turn; keep them but make a burst of
  frames not re-enter `select!` per frame when the outbox has already coalesced (measure viewer CPU
  during the burst before and after).
- The stdin producer thread polls with a 100 ms timeout; confirm it costs nothing while idle (it
  should) and that a keystroke is not delayed by it.

## 5. Memory

- Measure RSS per pane at 10 000 rows of history for a plain-text pane and for a pane with styled
  wide output; report bytes per history row. If `vt100`'s per-cell representation dominates, argue
  the options: a lower default `scrollback-lines` (documented behavior change, needs the user's
  decision — report, do not decide), a compact history representation outside the emulator, or
  accepting the number. The limits in `config.rs` and the documented 1–100 000 range stay.
- Per-viewer memory: the retained frame and the last painted buffer are the only state that
  should scale with the screen; check the outbox depth (64 frames of a 200×60 screen must not be
  64 full frames in memory — coalescing and deltas should make that moot).
- Startup RSS and binary size: report them; do not chase them unless a change in this pass moved
  them.

## 6. Preserve the contract, prove it

Every existing test keeps passing or is migrated with its assertion intact and an entry in an
assertion ledger: `tests/ecs.rs` (20 deterministic tests including the randomized sequence at
2048 cases and the invariant checker after every step), `tests/structure.rs`, `tests/local_cli.rs`
with all six Python harnesses, `tests/zor_integration.rs` with a real `ZOR_BIN`, the fixture-child
binary suite, and the koh gateway suites against the new binary. Add tests for what changes:
delta application (property-based: applying the deltas the server produces to the last full frame
equals the full frame the server would have produced), coalescing of deltas in the outbox,
generation mismatch recovery, the packed cell encoding round trip including wide and combining
characters, and the output filter fed one byte at a time.

The ordering guarantees in docs/design.md ("Ordering guarantees") are the correctness contract for
this work: per-source byte order, frames before replies, detach draining, stale generations
ignored, bounded ingest. A change that makes any of them probabilistic is not accepted regardless
of the speedup.

Performance acceptance, release builds on the same machine, three alternating runs, ranges
reported (targets are for 80×24 with one viewer unless stated; "baseline" is the branch point):

| Measure | Target |
|---|---|
| input→frame latency median / p95 | ≤ 2 ms / ≤ 5 ms |
| bytes on the socket per keystroke (median) | ≤ 2 KiB (from roughly 100 KiB) |
| server CPU per 1 000 keystrokes | ≤ ¼ of baseline |
| 20 000-line burst to quiescence | ≤ 0.10 s, and server CPU for it ≤ ½ of baseline |
| 200×60, one viewer, latency median | ≤ 1.5× the 80×24 figure |
| 8 viewers on one tab, server CPU per keystroke | ≤ 2× the one-viewer figure |
| RSS after the burst | ≤ baseline; report bytes per history row |
| idle CPU per 10 s | 0.00 s, unchanged |
| startup | not worse than baseline range |

If a target is not met, report the number and the limiting evidence rather than lowering the
target silently; a partial improvement with an honest explanation is acceptable, a regression is
not.

Run the full gate on the final tree and record it in docs/ecs-acceptance.md:

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

The crate's lints (`deny` on `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`,
`string_slice`, `dead_code`, `unsafe_code = "forbid"`) stay as they are. `unsafe` is not a
performance tool here; if a hot loop needs unchecked indexing, restructure it so the bounds are
proven by iterators.

Dependencies: none added for the server path without a measurement that justifies it; `criterion`
as a dev-dependency for micro-benchmarks of the cell encoding, the filter and the delta
application is acceptable if the benchmarks are committed under `benches/` and referenced from
the report. Do not enable `bevy_ecs` features. Do not add a threaded executor: the single-threaded
step is a design decision (one logical writer); parallelism belongs in the reader threads and the
viewer's own process.

## 7. Work in bounded slices

For each slice: measure, change, measure again with the same command, run the targeted tests,
update the ledgers, commit on the branch with a message that names the slice and its numbers.
Suggested order: measurement tooling and profiles → pane view cache and cell representation
(server only, wire unchanged) → output feeding → delta frames and protocol v6 with the viewer's
delta application → viewer painting → budgets and memory → docs. A slice that adds complexity
without a measured gain is reverted, not kept "for later".

Measure after every slice; do not batch the measurements to the end. Record the machine state
(other processes, power state) with each measurement.

## 8. Independent review and acceptance

Use fresh reviewer subagents that did not implement the reviewed slice, once after the protocol
slice and once on the complete branch diff against the merge base. They review correctness of the
delta path against the ordering guarantees (a reviewer should try to construct a sequence of
frames, coalescing and generation changes that desynchronizes a viewer, and show either that it
cannot or that a full frame recovers it), the packed-cell encoding at cell boundaries (wide
characters at the right edge, combining marks, zero-width joiners, control characters), the
filter's streaming behavior at every split point, the memory claims, the measurement method (is
the script measuring what the report claims?), and every claim in the documents. Fix confirmed
P0/P1 and in-scope lower findings, document rejected findings with reasons, rerun affected checks
and measurements, and obtain a final review after fixes.

## Deliverables and completion

- The branch with slice-sized commits, pushed to origin as authorized above.
- One pull request against `main` whose description contains: the baseline SHA, the measurement
  commands and machine, the before/after table for every measure above with ranges, the profile
  summaries before and after, the assertion ledger, the protocol change summary (v6) with the koh
  patch delta, the memory analysis, the gate output summary, and the review findings with
  dispositions. Open it as a draft; mark it ready only when the gate and the final review are
  clean.
- Updated README.md (the "Panes and history" limits if any changed, the protocol version),
  docs/design.md (frame derivation, delta rules, feeding path), docs/local-attachment-protocol.md
  (v6), docs/ecs-acceptance.md (a performance section with the tables and the review record),
  CHANGELOG (0.5.0: attachment protocol v6, performance), a short HANDOFF.md, and
  `tools/measure*.py` as the reproducible measurement method.

Do not declare completion while any measure lacks a before/after number, any migrated assertion
lacks a replacement, the protocol document does not describe what the code sends, required
verification is unresolved, or the PR is still a draft with known findings. Do not merge, do not
comment on other PRs, and do not push to `main`.
