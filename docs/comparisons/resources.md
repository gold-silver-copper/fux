# Idle and burst resources at 1, 4 and 8 observed panes

All 18 paired cases passed: three repetitions per backend and pane count, using
private HOME/XDG directories, real local owner processes, and identical synthetic
workers. This establishes bounded CPU/memory scaling and pane-API output visibility;
it does not establish observer capture traffic or agent-state freshness.

The table reports medians of three runs. CPU is elapsed process CPU time, not a
percentage. Idle covers approximately three seconds; burst covers input submission,
output visibility polling, and one additional second for background work. Fux and
zor components are summed; herdr is its integrated server process.

| Panes | Backend | Idle CPU ms | Burst CPU ms | Idle RSS MiB | Idle footprint MiB | All output visible ms |
|---|---|---:|---:|---:|---:|---:|
| 1 | fux + zor | 4.68 | 15.31 | 41.98 | 25.81 | 7.95 |
| 1 | herdr | 4.36 | 10.65 | 31.89 | 9.06 | 44.89 |
| 4 | fux + zor | 6.34 | 47.88 | 43.12 | 26.91 | 28.58 |
| 4 | herdr | 10.90 | 30.42 | 33.59 | 10.78 | 44.61 |
| 8 | fux + zor | 11.32 | 97.07 | 45.12 | 28.91 | 61.83 |
| 8 | herdr | 23.69 | 73.13 | 35.94 | 13.14 | 35.11 |

In this sample, fux+zor used less idle CPU at four and eight panes, while herdr used
less burst CPU and memory at every tested size. Output visibility favored fux+zor
at one and four panes, and herdr at eight. These are descriptive results with three
samples, not statistical superiority claims or a release-build performance rating.

## Method and limits

- One pane per workspace, no attached TUI. The foreground worker is named `claude`
  solely for process admission; it does not run Claude or contact a model. Both
  controllers must discover the expected number of panes before warmup.
- Each worker emits 256 lines of 66 bytes plus `BURST_DONE` and newline: 16,907
  application output bytes per pane before PTY newline translation. Inputs are
  submitted sequentially to every pane, then visible captures are read sequentially
  until each contains the marker. The reported latency includes these harness
  requests and its 30 ms retry delay; it is neither per-pane processing latency nor
  time to a fresh agent state. `harness_capture_reads` counts those burst reads only.
- Actual initial PTY dimensions were **80×23 for fux** and **120×40 for herdr**, as
  measured inside each worker with `TIOCGWINSZ`. These are the respective headless
  defaults; the workload fits both widths. Memory and capture costs are therefore
  comparisons of these configurations, not equal-geometry engine measurements.
- A one-second warmup follows discovery and ready-output checks. The sampler uses
  `proc_pid_rusage`, checks owner liveness and stable process-start counters, and
  calibrates Mach CPU units against the process CPU clock for the run. Components
  are sampled sequentially, so phase boundaries have sampler-launch skew.
- Only the owned fux+zor or herdr server processes are included. Synthetic workers,
  harness, sampler, operating-system work outside those processes, and model costs
  are excluded. RSS sums can double-count shared pages. Footprint is also a sum of
  per-process accounting, not a system-wide unique-memory measurement. The JSON
  retains before/after values; no peak-memory claim is made.
- These are existing debug binaries, pinned by SHA-256. The unchanged herdr 0.8.2
  reference build provenance is embedded. Order is fixed: fux+zor then herdr at each
  size. Short runs on one machine, scheduling, allocator warmup, viewport defaults,
  and fixed ordering limit extrapolation. Idle CPU is small relative to wall time.
- Every owner exited normally and every recorded worker PID disappeared. The
  harness rejects forced cleanup, missing discovery/output, or incomplete samples.

## Reproduction and verification

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- capture-resources \
  --fux target/debug/fux --zor zor/target/debug/zor \
  --herdr target/herdr-reference/build/debug/herdr \
  --herdr-provenance docs/comparisons/controller-setup-build.json \
  --output /tmp/resources.json
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- verify-resources
```

The output path must not already exist. `resources.json` retains all 18 runs, raw
samples, calibration, viewport sizes, cleanup results, and binary/source hashes.
Four mandatory offline tests validate the matrix and provenance, CPU conversion,
component accounting, PID identity and cleanup. They reject omitted zor costs,
incorrect units/values, PID reuse and unproven cleanup; they do not replace running
the live scenario. Python compilation and `git diff --check` also passed.

The subsequent [capture-traffic report](capture-traffic.md) measures zor workspace
IPC and records herdr internal reads as unmeasured. The [workflow report](workflow.md)
separately covers verification/artifacts/cleanup. This resource artifact alone does
not establish those outcomes.

The subsequent [live freshness report](detection-freshness.md) supplies the bounded
real Codex blocker/loss measurement. R5 measurement categories are now retained;
broader real-agent coverage remains R4, with R6/R7 also open.

The first-party capture is now Rust (`capture-resources`). Historical JSON remains
unchanged and verifies against the exact archived Python source. A fresh six-case
migration run at one repetition passed at `/tmp/fux-rust-resources-capture.json`;
it verifies the port and is not substituted for this retained 18-run comparison.
Rust's bounded subprocess polling contributes sampling skew, so its timings are not
directly compared against the historical Python capture. The worker and native C
resource sampler are unchanged. The retained offline matrix and negative cases pass.
