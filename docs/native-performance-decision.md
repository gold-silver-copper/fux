# N3 measured optimization decision

**Reject the ASCII validation fast path.** It reduces isolated validation cost, but
the complete workload shows mixed results and a substantial slow-consumer sustained
regression. Production validation is restored. No universal speedup is claimed.

Evidence is retained in `.verification/native-performance-20260908/`: baseline and
candidate executable copies, source archives, SHA-256 records, build/toolchain records,
raw viewer matrices, focused probe logs, public-CLI journal capture and koh counters.
Both runtime builds use explicit empty RUSTFLAGS, dev opt-level 0 and debug level 2.
The identical baseline zor executable is used in both viewer matrices. No paid calls,
privileged instrumentation or R6 runtime checks were used.

## Paired viewer results

Each build ran three repetitions of the same Rust harness at 80×24. Values below
are medians, in milliseconds. B01 is burst; S01 is sustained. Every run retained
successful cleanup and passed the capture validator. Baseline ran before candidate;
these samples are not randomized or statistical significance evidence.

| Panes/viewers/slow | Phase | Fux CPU baseline → candidate | Visible latency baseline → candidate |
|---|---|---:|---:|
| 1/1/no | B01 | 38.324 → 33.189 | 53.198 → 50.815 |
| 1/1/no | S01 | 1043.373 → 990.428 | 1259.456 → 1283.275 |
| 1/4/no | B01 | 78.307 → 76.874 | 99.775 → 94.106 |
| 1/4/no | S01 | 1269.454 → 1281.881 | 1295.192 → 1309.435 |
| 4/4/no | B01 | 172.481 → 168.138 | 187.234 → 191.189 |
| 4/4/no | S01 | 1353.875 → 1344.158 | 1374.091 → 1362.223 |
| 4/4/yes | B01 | 180.341 → 169.298 | 211.898 → 201.464 |
| 4/4/yes | S01 | 1344.675 → 1520.344 | 1373.349 → 1555.007 |

Raw records also retain idle, CPU ticks/calibration, geometry, memory, frame/byte
counts and client pending-buffer high-water marks. Latency includes polling/parsing;
client high-water marks do not measure server queues. The rejection avoids selecting
only favorable burst results or spending repeated benchmark runs to seek acceptance.

## Cost investigation

The temporary test-only `view::tests::measure_native_milestone_view_costs` measured production
PaneView construction, validation and JSON serialization at 200 iterations, three
repetitions, for full ASCII and wide-Unicode panes. Construction includes allocation,
validation and drop; serialization is a pane object, not a complete transport frame.
Median ASCII times per pane were construction 1.050 → 0.509 ms, validation
0.566 → 0.020 ms, serialization 10.444 → 10.409 ms. Encoded size stayed 301,886 bytes.
Unicode encoded size stayed 323,006 bytes; its general validation path was unchanged.
Serialization dominates these isolated costs. Sharing serialized snapshots would
require a separately justified memory/ownership change and is outside this selected
experiment. Capture construction and per-viewer validation/serialization remain
distinct; the existing immutable per-publication pane sharing remains unchanged.

The probe's empty loop/black-box overhead was 1.3–1.9 microseconds per 200 iterations.
Timers bracket loops, not individual cells. No measurement hooks run in normal builds.
Allocation counts, allocator overhead and isolated server queue measurements remain
unavailable; encoded bytes are not presented as allocation counts. The baseline archive
predates the probe; its identical probe body is retained in the candidate source archive
and `tools/archive/native-milestone/view-cost-probe.rs.txt`. The probe was removed
from active source after measurement to comply with the mandatory no-ignored-tests
invariant. Neither measurements nor archived build inputs were changed.

Zor's current public-CLI journal sample performs 96 operations of each kind:

| Operation | Median elapsed ms | Median child CPU ms | Logical committed bytes |
|---|---:|---:|---:|
| Adopt | 25.628 | 5.150 | 654426 |
| Inspect | 13.056 | 4.244 | 0 |
| Idempotent adopt | 13.067 | 4.161 | 0 |

All repetitions retained generation 32 and clean exits. CPU includes CLI startup;
elapsed time includes the runner's 10 ms polling. Store::open reads/decodes, validates
and reserializes under its nonblocking journal lock; transactions clone and serialize
before atomic replacement. Internal lock hold time and fsync cost are not isolated.
Observation processing retains revision/input identity and bounded capture reuse;
dashboard composition scans bounded journal summaries and joins at most 128 observed
panes. Native summaries omit response and input payloads. Those paths were inspected,
but no dashboard-specific timing or optimization is claimed. This experiment changes
neither their storage nor observation behavior.

Koh's actual deterministic framing/window test passed: 524,288 retained receive bytes,
32 items, 16,395 encoded bytes for a 16 KiB data frame, nine ACK bytes and 2,345 writes
with a seven-byte partial sink. Production exchange copies each input chunk into its
bounded retained frame, clones pending frames for delivery, and commits application
writes before ACK. This identifies copying/buffering costs without claiming remote
performance. No koh optimization or R6 runtime proof is included.

## Correctness and bounds

The candidate passed exhaustive ASCII/kind checks, existing Unicode/frame tests and
strict lint. Independent review found no semantic defect. It was rejected on measured
tradeoffs, not because those correctness tests failed. The candidate source and binary
remain retained; final `src/view.rs` is byte-identical to the original baseline source.

No buffer, queue, cache, deadline, framing, input receipt, ordering or freshness policy
changes survive the experiment. Existing 16 MiB frame bounds, per-publication immutable
view lifetime, viewer coalescing and koh window/ACK rules are unchanged. Final complete
diff review and mandatory headless gate passed; see
[the final verification record](native-final-verification.md).
