# Shared pane-view experiment

Synthetic, headless local experiment on the recorded host. Each build ran three repetitions in fixed order at 80×24 viewer geometry; exact per-pane rectangles match across the paired configurations. These are unoptimized dev builds with explicit Rust flags, not release rankings against other multiplexers.

The after build constructs each immutable pane view once per publication pass and shares it with viewers. The cache expires after that pass. This adds Arc allocation/reference-count and map overhead. Serialization and transport bytes remain per viewer; no wire-volume reduction is claimed.

| Panes / viewers / slow reader | Phase | Median fux CPU ms before → after | Median visible ms before → after |
|---|---|---:|---:|
| 1 / 1 / no | idle | 0.00 → 0.00 | — |
| 1 / 1 / no | B01 | 29.88 → 33.01 | 37.94 → 38.92 |
| 1 / 1 / no | S01 | 1208.26 → 1212.59 | 1234.53 → 1239.04 |
| 1 / 4 / no | idle | 0.00 → 0.00 | — |
| 1 / 4 / no | B01 | 76.08 → 75.57 | 90.55 → 87.29 |
| 1 / 4 / no | S01 | 1261.64 → 1247.84 | 1290.29 → 1282.23 |
| 4 / 4 / no | idle | 0.00 → 0.00 | — |
| 4 / 4 / no | B01 | 201.15 → 174.03 | 204.15 → 180.19 |
| 4 / 4 / no | S01 | 1351.90 → 1318.75 | 1364.15 → 1340.52 |
| 4 / 4 / yes | idle | 0.00 → 0.00 | — |
| 4 / 4 / yes | B01 | 193.03 → 175.68 | 210.76 → 184.53 |
| 4 / 4 / yes | S01 | 1385.84 → 1321.52 | 1403.21 → 1351.37 |

The four-pane burst cases improved in these samples; the single-pane/single-viewer burst regressed. Sustained CPU changed relatively little. Three repetitions in fixed before/after order do not establish statistical significance or a universal speedup. Shared construction does not remove per-viewer JSON serialization.

The harness waits for fresh zor observations but fixes the same pre-change zor binary in both runs to isolate fux. This is observation-active coverage, not a journal benchmark or a measurement of subsequent zor changes. Journal and deterministic koh cost-center investigation remain separate milestone work.

Each phase retains owner PID/start identity, calibrated CPU ticks, RSS/footprint, elapsed wall time, received bytes, completed frame bytes/counts and client pending-buffer high-water marks. High-water values are cumulative per viewer, not server queue measurements. Visible latency includes harness polling and parsing. Allocations, internal server queues and lock hold times are not measured here. Idle has no visible-output latency.

All 24 cases retained clean owner exits and worker disappearance. The offline validator checks the complete paired matrix, matching geometry, sampler calibration, identities, monotonic counters and cleanup; it does not replay the workload. Run `cargo run --manifest-path tools/xtask/Cargo.toml --locked -- verify-headless-performance`.

Build records retain source and binary hashes. The baseline binaries were copied before changes. The candidate hash was recorded at capture; later test builds replaced that path, so the candidate executable itself is not retained. The older preliminary unpinned run is excluded. The baseline build record’s `matches_original_binary_hashes: false` describes that discarded preliminary comparison.

Affected bounds: attachments permit 64 viewers per workspace; client messages are 64 KiB, input chunks 4 KiB, server frames 16 MiB, with five-second frame deadlines. The immutable view cache contains at most one entry per visited pane for one publication pass. Viewer outboxes coalesce replaceable snapshots; serialized frames are still bounded and independently written. The exact frame-limit check occurs during serialization and does not establish a 16 MiB allocation cap on Rust view objects.

Zor subprocess capture is limited to 256 KiB per stdout/stderr stream and the caller’s original deadline; exceeding either signals the owned process group and reaps its direct child. Journal storage remains 4 MiB with check-result reserves and 512 directory entries, 128 tasks and 1,024 prompt records. Koh uses 16 KiB chunks, a 32-frame pending window, two incoming events and one queued outgoing frame; partial framing/application writes use ten-second deadlines. These are implementation bounds, not observed high-water claims. R6 remote acceptance is deferred.

## Journal investigation

The separate pinned-baseline public-CLI sample uses one local pane and 32 adopted tasks per repetition. Three repetitions each perform 32 adoptions, 32 inspections and 32 exact adoption replays. All owners and workers exited; generation remained 32 after inspection/replay. No journal production change was made.

| Operation | Samples | Median elapsed ms | Median child CPU ms | Logical replacements |
|---|---:|---:|---:|---:|
| adopt | 96 | 21.920 | 6.277 | 96 |
| inspect | 96 | 7.474 | 5.163 | 0 |
| idempotent-adopt | 96 | 7.377 | 5.215 | 0 |

Across the three repetitions, logical committed journal bytes total 649194. This sums the size of each observed atomic replacement, not physical filesystem writes. CLI startup is included in CPU/latency. Internal lock hold times, allocation counts and isolated fsync latency remain unavailable.

Inspection confirms that Store::open decodes, validates and re-serializes its bounded journal while holding a nonblocking process lock; transactions clone and atomically serialize the candidate. The repeated inspection/replay sample establishes no extra writes, so no storage rewrite or speculative cache was added. Service observation already reuses capture revisions; the fux experiment keeps it active. Dashboard composition uses existing correctness coverage; no dashboard-specific timing claim is made.

New journal captures use:

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- headless-journal --fux PATH --zor PATH --output NEW_JSON
```

The Rust capture preserves three repetitions of 32 adoptions, 32 inspections and
32 idempotent adoptions. Its elapsed times include the bounded subprocess runner's
10 ms completion polling, so they are not directly comparable with the historical
Python harness timings. The retained baseline remains attributed to the original
source archived as `tools/archive/tools/headless_journal.py.txt`.

## Rust capture migration

Current reproduction uses:

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- headless-performance \
  --fux target/debug/fux --zor zor/target/debug/zor \
  --output /tmp/headless-performance-new.json --repetitions 3
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- verify-headless-performance \
  /tmp/headless-performance-new.json
```

The four-case migration run at one repetition passed in
`/tmp/fux-rust-headless-performance-capture.json`. It preserves the worker, phases,
viewer delay, raw counters, process identity and cleanup checks. Rust's bounded
subprocess polling contributes sampling overhead; the new timings verify the port
and are not substituted for the retained paired comparison above. Historical Python
source remains as a nonexecutable exact-hash archive. The default verifier continues
to check the original paired matrices and negative cases.
