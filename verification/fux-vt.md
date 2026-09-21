# fux-vt implementation and verification

Status: **in progress; not complete**. No completion PR has been opened.

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
- [ ] Independent expected-result mappings, then scaffolding removal commit.
- [x] Root workspace membership; excluded independent harness/fuzz package.
- [ ] Complete migration of production and root tests, including frame paths.
- [ ] Actual 1x1 PTY creation/resize with traces, unchanged 2x2 layout rule.
- [ ] Persistent safe row-ID selections and overwrite/eviction tests/traces.
- [ ] Row-version caching, complete-frame semantics, multi-viewer traces.
- [ ] Updated root/harness documentation.
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
and cleanup passed. Indices 2–5 passed. A replacement measurement index 6
is running (PID 2992); warm-up index 0 is not counted. The baseline stream
passed in 3.485 seconds, including final frame equality. Record every failed
or interrupted measurement rather than silently excluding its existence.

No application workaround is retired yet and no final verification gate is
claimed to pass.
