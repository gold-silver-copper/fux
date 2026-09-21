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
- Scale baseline in progress: warm-up index 0, measured indices 1–5;
  `/tmp/fux-vt-evidence/baseline-scale-N.log` and matching run directories.
  Each command uses baseline copies, `--scenario scale --seed 1 --seconds 600`.
  A stream run follows with `--scenario stream --seed 1 --seconds 120`.
  Parent process 2190 records statuses to `baseline-scenarios.log`.

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

No differential mismatches have been measured yet. No workaround is retired
and no final verification gate is claimed to pass.
