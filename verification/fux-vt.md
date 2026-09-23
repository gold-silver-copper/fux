# fux-vt implementation and verification

Status: **correctness gates complete on the final tree; the performance
gate is explicitly deferred by the user's instruction ("only correctness")
and is reported below with all measured numbers, not claimed as passed.**
See the final report at the end.

## Baseline

- Branch: `feat/fux-vt`, created from updated main `9140af1` (PR #39).
- Platform: macOS arm64, Darwin 27.0.0; Rust/Cargo 1.98.1.
- Untracked `docs/` was present before this task and is preserved, not staged.
- Baseline root fmt, strict Clippy, test and build passed: 58 unit + 35
  integration tests. Independent harness fmt, strict Clippy, test and build
  passed: 11 unit + 3 controlled-failure integration tests.
- Log: `/tmp/fux-vt-evidence/baseline-checks.log` (2026-09-21
  19:02:44–19:03:04 UTC). Baseline binary and harness copies:
  `/tmp/fux-vt-evidence/fux-baseline`, `fux-fuzz-baseline`.
- Initial scale warm-up (`--seconds 600`) timed out during tab creation
  (last action t910); diagnostics and cleanup passed. The next run was
  explicitly interrupted and also cleaned up. Logs and bundles remain as
  `baseline-scale-{0,1}` under the evidence directory; neither is a pass.
- Scale baseline restarted with the supported `--seconds 3600`, warm-up
  index 0 and measured indices 1–5, same seed 1 and preserved binaries.
  Logs/bundles: `/tmp/fux-vt-evidence/baseline-long-scale-N*`. Parent PID
  68251 records statuses in `baseline-long-scenarios.log`. Stream seed 1
  follows, with `--seconds 120`. Final comparison must use the same budgets.

## Required completion ledger

- [x] Read application consumers, tests, README and hunt prompts; inspect
  upstream dispatch, cells, grid, history/resize and callbacks.
- [x] Baseline branch, preserved binaries and passing ordinary checks.
- [x] Pre-implementation sequence/semantics contract in `fux-vt/README.md`.
- [ ] Fresh six-run scale baseline and stream evidence.
- [x] Owned bounded parser, ASCII fast path, row-major arenas and row IDs.
- [x] Permanent operation/boundary tests and sequence matrix coverage.
- [x] Differential corpus, chunking/resize/history/reply comparisons,
  minimized divergence inventory and passing differential-phase checkpoint.
- [x] Independent expected-result mappings, then scaffolding removal.
- [x] Root workspace membership; excluded independent harness/fuzz package.
- [x] Complete migration of production and root tests, including frame paths.
- [x] Actual 1x1 PTY creation/resize with traces, unchanged 2x2 layout rule.
- [x] Persistent safe row-ID selections and overwrite/eviction tests/traces.
- [x] Row-version caching, complete-frame semantics, multi-viewer traces.
- [x] Updated root/harness documentation.
- [ ] Final fuzz compile, fmt and clean 600-second run with artifacts.
- [ ] Final root/harness fmt, strict Clippy, tests and builds.
- [ ] Dependency/source audit (include root `tests/` as well as prompt paths).
- [ ] Every saved trace replayed on final binaries; record count and status.
- [ ] Individual affected scenarios, 20 walk iterations + 3000-action seed,
  20 raw iterations, 20 scene_fuzz iterations, full 600-second smoke.
- [ ] Same-machine five-run performance comparison, no median regression;
  stream equality/latency, focused parser/cache benchmarks and memory plateau.
- [ ] Requirement-by-requirement final audit and one complete PR.

## Audit observations requiring explicit coverage

The old source does not implement DECAWM 7 or 1047/1048; the first must be
added as an intended divergence, the latter remain explicitly unsupported.
It also has fixed eight-column tabs, mutually exclusive bold/dim, and an
absolute CPR (including the parked cursor). Root integration tests also use
the old crate and must migrate even though the prompt's sample grep omitted
`tests/`. Selection validation is not a pure revision check. The existing
cache is terminal-local and frame output is complete, not a dirty delta.

## Initial crate checkpoint

The workspace now includes an owned parser/grid implementation. `cargo test
-p fux-vt --locked` has passed the initial cell test and 14 semantic tests;
strict crate Clippy passed. A temporary differential suite compares 11
sequence families at every chunk (sizes 1/2/3/7/whole), plus ASCII history
resize operations, including replies and all retained history windows. Both
tests passed (`/tmp/fux-vt-evidence/differential-first.log`). This is **not**
yet the full differential-phase acceptance: generated corpus, exact
intentional divergences, broader parser/resize invariant coverage and xterm
evidence remain to be added. The main application is still unchanged.

One initial tiny-grid test expectation was wrong: it wrote a two-cell glyph
before expecting the cursor for an empty grid. Resetting before the separate
ASCII-wrap assertion corrected the fixture, not production behaviour.

## Passing differential phase

`cargo test -p fux-vt --all-features --locked` passes 3 unit tests, 5
scaffolding tests, the permanent 11-fixture test (five chunkings), 3 corpus
invariant tests and 17 semantic tests. Log:
`/tmp/fux-vt-evidence/differential-phase-final.log`. Root/fuzz fmt and strict
crate Clippy (`--all-features --all-targets`) also pass. The permanent mapping
is `fux-vt/tests/golden/README.md`; all goldens were recorded from the independent
oracle, never from fux-vt.

Two implementation differences were fixed (wrap below margins and height-only
resize wrap metadata). Two deliberate corrections beyond the known tiny-grid
crashes were verified against XTerm(411): DECAWM and ignored out-of-margin
IL/DL. The exact inventory is `fux-vt-divergences.json`; all 180 seeded
geometry/chunking comparisons and differing-boundary counts are retained in
`fux-vt-corpus-differences.json`. The diagnostic switch restores ONLY those two
oracle behaviours; otherwise it must match every retained cell and operation.
No measured mismatch remains unexplained. This does not claim exhaustive
terminal correctness.

Homebrew xterm and xorg-server were installed on macOS to execute
`python3 verification/fux-vt-xterm.py /tmp/fux-vt-evidence/xterm`. This owns
its Xvfb/xterm children and uses a controlled printer command, not an
interactive shell. Seven checks pass; results are retained in
`fux-vt-xterm-results.json`. Probe mistakes, not emulator defects: printing
was initially limited by margins, and printer completion needed an atomic
rename acknowledgement rather than reading an unfinished file.

The independent cargo-fuzz package builds with the installed nightly.
A preliminary `cargo +nightly fuzz run terminal --fuzz-dir fux-vt/fuzz --
-max_total_time=30 -max_len=4096 -rss_limit_mb=1024` passed 16,388 executions
in 31 seconds (`fuzz-preliminary.log`). This is NOT the final 600-second gate.
The 120 named seed files are encoded from the permanent adversarial corpus;
untracked coverage-growth files remain local and ignored. Shared invariants
check every retained row ID, wide half, cursor, clipped window and split-input
result after generated operations.

The first long-budget scale measurement failed on a ten-second HTTP timeout
after loading the large scene, not its 3,600-second run deadline; diagnostics
and cleanup passed. Indices 2–5 passed. Replacement measurement index 6
passed; warm-up index 0 is not counted. The baseline stream
passed in 3.485 seconds, including final frame equality. Record every failed
or interrupted measurement rather than silently excluding its existence.

## Migration checkpoint

All application consumers and root tests now use fux-vt. The temporary
scaffolding, diagnostic feature and dev-dependency are removed, after passing
checkpoint `b8fa0d8` and its permanent mapping. The prompt's exact source /
manifest / lock grep exits 1 with no matches; a separate grep of root `tests/`
also has no matches. The independent harness still uses upstream.

All three workarounds are retired in code, with unit coverage and saved
`owned-terminal-*` traces. The tiny and selection regressions were committed
**before** migration in `1be79f5`; both fail against the preserved baseline
with diagnostics/cleanup passing, and both pass after migration:

- `regression-tiny-before.log`: child PTY never reaches 1x1; after: 655 ms PASS.
- `regression-selection-before.log`: anchored top LINE-039 changes to LINE-042;
  after: 1,122 ms PASS, exact retained `LI` OSC52 effect.
- `regression-stream-after.log`: 3,499 ms PASS; catch-up 1 ms, advancing 9 ms,
  convergence 130 ms, final frame equality true. This is a checkpoint, not
  the final paired benchmark.

Selection now uses row IDs/columns, row versions and selected-span comparisons,
including all interior rows. Tests pin unrelated writes/styles, full and
partial scrolling, unselected eviction versus required-row loss, explicit
browsing, clipping, resize, buffer switch/reset and pane removal notices.
Copy checks byte limits while extracting. A newly added cache test exposed
an integration semantic difference: full-window bounded extraction preserves
a trailing selected empty row, whereas whole-pane copy historically trims
trailing empty rows. Whole-pane copy now trims those newlines explicitly;
selected-range extraction does not. The test-only frame text helper has the
same whole-screen display convention.

Terminal caches extracted rows by identity/version/width with a 4096-entry /
4 MiB budget, not a revision-keyed screen. Independent complete row sets are
returned on every request, including a new viewer and cursor-only changes.
Unit tests alternate widths/history offsets without another extraction,
verify SGR-only relocation, resize/reset/alternate behaviour, and sustain
12,000 output scrolls through eviction with equal plateau footprints.

`migration-final-tests.log` records passing workspace tests: 70 application
unit + 35 integration, 3 crate unit + 1 golden + 4 invariant + 17 semantic
(in addition to two explicitly ignored performance probes, still to be run).
Root strict Clippy/fmt/build and independent harness strict Clippy/fmt/tests /
build pass. Harness test log: `migration-harness-tests.log`.

Fixture mistakes are separate from production defects: a pane-removal unit
fixture originally inserted Focused without any workspace; navigation repair
correctly removed that unattached Viewer. The isolated fixture no longer
constructs an unrelated invalid navigation graph.

Baseline scale replacement index 6 completed in 161.636 seconds, PASS. The
five valid measurements 2–6 have medians 45 ms (201 panes) and 148 ms (move to
tab 1000). Stream instrumentation and necessary scenario changes mean the
final comparison will rerun BOTH binaries with the same final harness.
No final performance/completion gate is claimed yet.

## Late copy audit and mandatory re-verification

The first 600-second run passed: 374,057 executions in 601 seconds, seed
481504938, peak RSS 518 MiB, exit 0, 2026-09-21 20:29:51–20:39:52 UTC.
`fuzz-final.log` retains the complete result. The test-only metadata probe
was appended while it ran; rebuilding proved the fuzz binary byte-identical
(`terminal-fuzz-813dd51`). This clean run is nevertheless **not final**:
a later production copy fix requires restarting the full clean run.

The preliminary final harness series passed all seven affected scenarios,
21 saved traces, and all four stress invocations (20 walk cases in 192.920 s;
3000-action walk in 388.160 s; 20 raw cases in 53.874 s; 20 scene-fuzz cases
in 67.780 s). Reports are `final-scenarios-v2/results.json`,
`final-traces/results.json` and `final-stress/results.json` under the evidence
root. These must be rerun on the post-copy-fix binaries. The first scenario
attempt had one harness error: resize still asserted the retired 2x2 backing
minimum. That assertion now requires exact positive dimensions; frame
oracle minima remain unchanged.

An audit of bounded extraction across resize found an additional production
bug: a historical soft wrap widened from 5 to 10 columns copied
`abcde     fgh` instead of `abcdefgh`. `036f8b1` committed the failing unit
regression and independent upstream example; `0e7a21e`/`8c35989` committed
the real clipboard trace before the fix. The new `Window::text` skips
nonexistent historical padding just as it skips a clipped wide leader.
Actual stored spaces are still retained at soft joins. Root whole-pane copy
has a direct unit assertion too; the independent example has no fux-vt
link/dependency.

Evidence: `history-copy-before.log`, `history-copy-oracle.log`,
`history-copy-trace-before-v2.log` (FAIL at exact clipboard assertion, cleanup
and diagnostics pass), `history-copy-after-tests.log` and
`history-copy-trace-after.log` (1,126 ms PASS). The first new trace attempt
incorrectly expected padded frame text to omit trailing display spaces;
trimming only those visual assertion line ends fixed the fixture. The exact
clipboard assertion was never relaxed. The divergence inventory includes
this fixed mismatch; open mismatches remain zero.

There are now **22** saved traces and **134** named fuzz seeds. Final-tree
checks/probes are reproducible through `verification/fux-vt-checks.py`;
`fux-vt-gates.py` enumerates scenarios/traces/stress/smoke; the paired
`fux-vt-performance.py` retains every scale/stream run, includes warm-ups,
alternates version order, and refuses any scale median regression.
The actual CLI accepts 1–100 iterations, 1–5000 actions, and 1–3600 seconds
(`final-harness-help.txt`). No timeout, interruption or unrun gate is a pass.

A second error-boundary audit injected row-ID exhaustion after the first of
two scroll iterations. The grid remained valid, but the structural mark was
only updated on success; moved unchanged rows could be missed by a marks-based
reader after the error. `79332e3` pins that failure. Structural invalidation
is now published before the fallible scroll; independent readers both get
all three retained rows. Reset/resize already construct replacements before
committing, so they do not have this partial-mutation issue. This owned-API
case has no upstream row-ID analogue and cannot be reached in a practical
black-box run without the test-only exhaustion injection.

`exhausted-scroll-before.log` preserves the assertion failure. The in-progress
second fuzz run and second long walk were deliberately interrupted to avoid
claiming them against superseded code; both exit 1. The walk reports diagnostics
and cleanup OK. Their logs remain `fuzz-final-v2.log` and
`final-stress-v2/walk-777x3000.log`. No interruption counts as a pass. The
post-exhaustion-fix final checks, clean fuzz run and complete harness chain
will use new artifact directories.

## Instance identity and harness settling

Row IDs are parser-local. A selection whose leaf is retargeted to another
process, or whose process entity receives a replacement `Terminal`, could
match coincidentally equal IDs. `Terminal` now carries a process-wide unique
instance number; `Selection` records it and a mismatch clears copy mode with
`selection cleared: terminal replaced`. Unit test:
`parser_local_row_ids_cannot_cross_terminal_instances` (both retarget forms);
`selection-instance-before.log` preserves the failing assertion.

The third clean fuzz run passed with the same libFuzzer seed 481504938:
615,992 executions in 601 seconds, peak RSS 523 MiB, exit 0, 21:13:39–21:23:41
UTC (`fuzz-final-v3.log`, initial corpus archived as
`fuzz-final-v3-initial-corpus.tgz`). The fux-vt crate is unchanged since
that run (`git diff da6f606 -- fux-vt` is empty), so it covers the final tree.

The full smoke failed once at case 35 (`resize_cmd`): the repeated-grow
settle accepted two consecutive equal 5 ms polls, which can straddle the
reflected size publication. Replaying the scenario six times per binary
reproduced the same failure on the preserved **baseline** binary
(`resize-cmd-repeat-baseline-2.log`), so this is a harness race, not an
emulator defect. The scenario now requests a frame to apply queued layout
changes and requires twenty consecutive equal observations; ten replays per
binary then pass (`resize-cmd-fixed-*.log`). No assertion was weakened.

## Performance investigation

The first paired comparison on `b63f1ef` (`paired-performance-1/performance.json`;
one warm-up plus five measurements per version and scenario, alternating
order, seed 1, debug profile, same harness build, stream final frames all
equal) showed medians of 44 ms before / 45 ms after at 201 panes and 147 ms /
147 ms for the move to tab 1000, with +1 ms medians on nine labels. The
timer resolution is 1 ms, but the pane-count labels moved consistently, so
this was investigated as real rather than dismissed as noise.

Cause: the old whole-screen snapshot returned an unchanged pane's lines with
no per-row work, whereas the row cache performs one lookup per painted row.
In the unoptimized profile the measured debug cost was about 213 ns per row
(`ROW-REUSE-TIMES` reused 20.4 ms for 96,000 rows), or roughly 1 ms at a
few thousand rows per 201-pane frame. `c5e867d` replaces SipHash with a
multiplicative key mix (process-private, bounded table), halving that to
about 96 ns per row (9.2 ms per 96,000 rows) with byte-identical output.
Both binaries were then remeasured with the same harness as
`paired-performance-2`. That run was noisier (isolated 200–300 ms outliers on
both sides) but still showed 43 ms before / 45 ms after at 201 panes and
50 / 53 ms after the large round trip, so hashing alone was not enough: an
unchanged pane still paid one lookup per painted row.

The row cache therefore gained a bounded index of the eight most recently
served windows, keyed by the emulator's non-destructive mark and the
window's offset/height/width and sharing the row entries' `Arc`s. It is not
the removed revision-keyed snapshot: several viewers' windows coexist, keys
are emulator marks rather than the terminal revision, and any change
(including cursor-only) falls back to row-ID/version reuse rather than a
full rebuild. Unit tests assert zero row lookups for alternating unchanged
windows, three lookups and zero extractions after a cursor-only change,
and the eight-window bound with row reuse after eviction. Debug probe:
window hit 0.1 µs/frame, row reuse 2.5 µs/frame, rebuild 165 µs/frame
(release: 0.01 / 0.16 / 12 µs). A third paired comparison was started as
`paired-performance-3` and then **stopped after three of twelve scale runs**
when the user directed that only correctness matters for now. Its partial
results (warm-ups plus one measured pair) are retained; they are not a
measurement. The performance gate in the prompt is therefore **deferred, not
passed**: the last complete comparison (`paired-performance-2`, before the
window index) showed +2 ms median at 201 panes and no change at the move to
tab 1000.

## Final report (commit `39724bc`)

**Differential mismatches.** Open mismatches: **zero**. Fixed in fux-vt:
wrap below margins, height-only resize wrap metadata, widened-history copy
padding, partial-scroll mark invalidation. Allowlisted with executed
XTerm(411) evidence: DECAWM (upstream ignores it) and IL/DL outside margins
(upstream edits rows there). Narrow exclusions with explicit expectations:
the two known upstream tiny-grid crashes. Inventory:
`fux-vt-divergences.json`; all 180 seeded comparisons:
`fux-vt-corpus-differences.json`.

**Sequence coverage and exclusions.** `fux-vt/README.md` matrix; permanent
mapping of every scaffolding case in `fux-vt/tests/golden/README.md`; goldens
recorded from upstream only, never fux-vt. Exclusions: 1047/1048, reflow,
graphics, kitty keyboard, grapheme segmentation beyond wide+combining,
alternate-screen history, programmable tabs.

**Workarounds retired (all three).** 1x1 PTY creation/resize verified by
child `stty size` (process scenario, `owned-terminal-tiny-child-geometry`);
row-ID selections with span/version validation and instance binding
(`owned-terminal-retained-selection`, 14 selection unit tests); the
revision-keyed whole-screen snapshot removed in favour of a bounded
row-ID/version cache plus a bounded mark-keyed window index
(`owned-terminal-complete-slow-frames`, 4 cache unit tests). Pane layout
keeps its 2x2 usability minimum.

**Fuzz.** cargo-fuzz 0.13.2, rustc 1.100.0-nightly (bba531001 2026-09-20),
`-max_total_time=600 -max_len=4096 -rss_limit_mb=1024 -seed=481504938`:
615,992 executions in 601 s, peak RSS 523 MiB, exit 0 (`fuzz-final-v3.log`).
fux-vt is byte-identical since that run (`git diff da6f606 -- fux-vt` empty).
134 named seeds committed.

**Memory.** 24x80 with 10,000 history rows: 26.0 MB steady reserved
(cells 25.66 MB + row metadata 0.24 MB + slot order 0.08 MB), identical
after a further 10,000 scrolls; transactional resize to 60x120 peaks at
65.2 MB reserved (`memory-plateau.log`). Row cache: 4096 entries / 4 MiB;
window index: 8 windows sharing those rows.

**Harness mistakes (not production defects).** Resize scenario asserted the
retired 2x2 backing minimum; resize_cmd settle could straddle size
publication (reproduced on the baseline binary); one history-copy fixture
compared padded frame text; one unit fixture built an unattached viewer;
xterm probe printer margins/atomic rename.

**Final-tree checks** (`final-checks-v5/checks.json`, all pass): fmt,
strict Clippy, tests (70 unit + 35 integration + 27 fux-vt), builds, harness
fmt/Clippy/tests/build, fuzz fmt/build, `cargo tree --workspace --locked`,
source/manifest/lock audit exit 1 with no output (also root `tests/`).

**Harness gates on the final binary** (sha256 `113403b8…`, results.json
under `final-scenarios-v6`, `final-traces-v5`, `final-stress-v5`,
`final-smoke-v4`), all on commit `39724bc`, every invocation exit 0:

| Group | Invocations | Result |
| --- | --- | --- |
| terminal_edge, adversarial, selection, history, resize, process, stream (`--seconds 3600`) | 7 | pass, 21.5 s |
| every saved trace under `fux-fuzz/traces` (`--replay PATH --seconds 3600`) | 22 | pass, 38.6 s |
| walk 100×20, walk 777×3000 actions, raw 600×20, scene_fuzz 300×20 | 4 (61 cases) | pass, 608.2 s |
| full smoke `--seconds 600` | 1 (62 cases) | pass, 308.8 s |

Stream convergence (`STREAM-TIMES`, five measured runs each, final frame
equal in every run): catch-up 1 ms both versions; advancing 13 ms before /
12 ms after; convergence 130 ms before / 129 ms after
(`paired-performance-1`).

## fux-vt 0.1.1: opt-in outputs for koh (branch `feat/fux-vt-for-koh`)

Additive, opt-in API for a consumer that mirrors a terminal elsewhere (koh):
`Options { events, extended_replies }`, `Event`/`Sink`/`process_with`,
`Screen::application_keypad`, and `Cell::new`/`Cell::wide_continuation`/
`Attributes::new`+`with_*`. Contract rows: README "Opt-in outputs" and the
ESC =/> row. `Parser::new` keeps `Options::default()`; fux never enables either
option, so fux's replies, events (none) and retained payloads (none) are
unchanged.

Evidence (macOS arm64, Rust 1.98.1, nightly 2026-09-20 for fuzzing):

- `tests/opt_in.rs` (7 tests): defaults, events, chunk invariance, the 64 KiB
  OSC bound and cancellation, extended replies, keypad state, exact cell
  reconstruction. `cargo test --workspace --locked`: 176 passed, 0 failed.
- Fuzz target: header bits 0x10/0x20 enable the options; events join the
  whole-vs-byte comparison. Two clean 600-second runs (391,478 and 445,601
  executions): `/tmp/fux-vt-evidence/fuzz-optin-600.log`, `fuzz-final-600.log`.
- `fux-vt-checks.py`: every check through `dependency-tree` passes. The
  `source-audit` check fails only on the pre-existing `vt100` crates.io keyword in
  `fux-vt/Cargo.toml` (also fails on `main`); `root-test-audit`, history-copy
  oracle, memory plateau, parser and row-reuse measurements pass when run
  individually.
- `fux-vt-xterm.py`: all nine cases byte-identical to the recorded results.
- `fux-vt-gates.py`: scenarios 7/7, traces 24/24, stress 4/4. The 600-second
  smoke run had one failure, case 46 (`scale`, `timeout: global` at 405 s under
  heavy machine load; diagnostics and cleanup OK). Rerun alone, `scale` passed
  on this branch (282 s) and on `main` (541 s).
- `tests/remote_lifecycle.rs::blocked_terminal_paint_does_not_block_stream_drain`
  is timing-sensitive under load: it failed once in a loaded full run, and
  under saturation it failed 1/30 on `main` and 0/30 on this branch. Otherwise
  it passes.
- Performance: one paired run (`/tmp/fux-vt-evidence/performance`) converged
  every stream (median convergence 127 ms before / 129 ms after) and flagged
  small, noisy scale-median deltas in both directions (sum of medians 1903 ms
  before / 1820 ms after). The noise rerun was stopped at the user's request
  (the machine was busy), so no performance claim is made here.
