# G1 performance measurements

Measured on 2026-10-28, Linux x86_64, Rust 1.98.1, release/thin LTO,
against `9ac4d48e`. Each implementation step got one 300-key idle/flood
latency run and one 16 MiB ASCII throughput run, on private servers.
This was a shared host; timings are observations, not gates or a claim of
latency improvement. Load averages and full counter reports were retained
with the run JSON in the task's private scratch directory.

The checker kept self/forward-call convergence, widening, the diagnostic
pass and the naive interpreter witness. Cold library initialization fell
from **10,747,405 to 8,420,050 instructions (21.7%)**, with task CPU
1.56 → 1.50 ms. A cold `%1 capture` client, against the same private server,
fell from **10,852,525 to 8,527,143 instructions (21.4%)**, CPU 2.02 → 1.46 ms.
This meets the original 11M ceiling but misses the proposed post-cut 8.5M
ceiling by 27,143 instructions. No gate floor was changed. There is no
existing vocabulary map to reuse; P51's premise was refuted, and no index
was added.

| Step | Server M instructions | Instructions/byte | Server counter CPU ms | Idle / flood p50 ms |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 292.355 | 17.426 | 30.00 | 0.285 / 0.342 |
| P52: time inputs | 293.800 | 17.512 | 32.88 | 0.430 / 0.176 |
| P53: wakes | 292.303 | 17.422 | 28.28 | 0.350 / 0.217 |
| P54: poll storage | 292.019 | 17.406 | 30.04 | 0.527 / 0.166 |
| P55: input sweep | 292.260 | 17.420 | 31.32 | 0.221 / 0.212 |
| P56/P59: read storage, comment | 291.606 | 17.381 | 26.93 | 0.597 / 0.331 |
| P58: pending cursor | 291.336 | 17.365 | 29.24 | 0.190 / 0.223 |
| Final cleanup | 292.994 | 17.464 | 33.99 | 0.606 / 0.501 |

The CPU column is perf's instruction-event enabled/running time for the
server task; both were equal in every report (100% counted). It excludes
client CPU. The input workload, 16 KiB read slice and 64 KiB pane turn cap
were held constant. Real-server counts still depend on PTY read boundaries;
small differences are not a demonstrated speedup.

Every sample used **51 bytes per idle key**, under the 64-byte ceiling.
The final sample exceeds the 0.5 ms idle target; flood stays below 1 ms.
Intermediate idle samples also crossed 0.5 ms. These results are retained
rather than selecting only the passing samples. The final count remains
below 30 server instructions/byte.

`SHORT_READ = 512` remains a fixed turn-yield rule, with 16 KiB reads and a
64 KiB pane turn. The audit measured flood echo at 0.28–0.41 ms with it
and 0.53–0.84 ms reading on to EAGAIN or the turn cap. A short read does
not assert that a PTY is empty. The P59 comment changes no behavior.

`pane/pending` holds a 128 KiB synchronized frame of screen-wide edits and
resumes it at parser budgets. One before/after sample used 993,462,663 →
993,384,894 instructions, CPU 38.25 → 38.79 ms, peak RSS 4,040 → 3,984 KiB.
That instruction difference is inconclusive; no speedup is claimed.
The cursor eliminates repeated suffix allocation/copying, transfers held
frames without copying, and releases the allocation once consumed. A
consumed prefix can remain allocated until then. Retained bytes stay
within the existing frame limit plus one read; the vector also retains
its existing capacity growth allowance. The property test checks stable storage across
yields and its release at completion.

Other budget observations, one matched baseline/changed pair:

| Check | Baseline | Changed |
| --- | ---: | ---: |
| `pane/corpus`, net instructions | 848,796,897 | 848,889,047 (+0.011%) |
| `paint/corpus`, net instructions/frame | 985,019 | 985,007 |
| `capture/history`, net instructions | 315,375,939 | 315,338,000 |
| `capture/history`, baseline-subtracted CPU | 16.94 ms | 18.02 ms |
| Start RSS | 5,380 KiB | 5,324 KiB |
| Incremental empty pane RSS | 60 KiB | 60 KiB |
| Incremental full-history pane RSS | 9,663 KiB | 9,657 KiB |
| Idle CPU / 10 s | 0 | 0 |
| Idle voluntary/involuntary switches, server and all client threads | 0 / 0 | 0 / 0 |

Counted component runs subtract their own setup baseline. Paint remains
below 1.2M instructions/frame; the pane count is within the usual minimum
0.25% floor. CPU differences are noisy, and capture CPU is not a direct
measurement of a server request's hold. The 16 ms capture target cannot be
certified here. Full-history incremental RSS exceeds the 8 MiB + 1 MiB
line on both builds; this is an existing budget miss, not a cursor memory
saving claim. These two capture/history targets remain separate work.

Reproduce the real-server sample with task-private TMPDIR and target storage:

```sh
cargo run --release --locked -j 4 --manifest-path bench/Cargo.toml --   feel --muxes fux --parts latency,throughput --only ascii --instructions --keys 300
```

The pending workload is `fux-bench run pane/pending`; `--baseline` omits
parsing. `cli/library`, `pane/corpus`, `paint/corpus` and `capture/history`
use the existing counted-workload interface. Existing floors are retained.

Physical Rust line deltas, separating native code, test modules and tool
code at their module/harness boundaries: production +185, tests +354,
tools +41. Bundled fuxth source changes: 0. Most test growth is formatting
the supplied time arguments. The new properties cover supplied time and
stable cursor storage, current poll descriptors, and the touched-input set.
