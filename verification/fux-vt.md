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
- [ ] Owned bounded parser, ASCII fast path, row-major arenas and row IDs.
- [ ] Permanent operation/boundary tests and full sequence matrix coverage.
- [ ] Differential corpus, chunking/resize/history/reply comparisons,
  minimized divergence inventory and passing differential-phase commit.
- [ ] Independent expected-result mappings, then scaffolding removal commit.
- [ ] Root workspace membership; excluded independent harness/fuzz package.
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

No measured common-subset differential mismatch yet. No workaround is retired
and no final verification gate is claimed to pass.
