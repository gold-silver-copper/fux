# Pane/layout performance comparison

The final-source measurement record is in the last section. Earlier sections retain intermediate
results and unresolved questions from those checkpoints; they are not the current source identity.

This is provisional evidence, not a performance acceptance pass. The shared Apple M2 Max host
had a recorded one-minute load average of 26.42–33.24 during these measurements. Three repetitions
cannot separate timing regressions from that contention. No manual visual inspection was completed:
the session's UI tool failed because `CUA_REPL_ENABLED_SURFACES` was unavailable.

Baseline: `1792223ea4a24905501823203b585e50906f0d38`, built in an isolated detached worktree.
Candidate: the current pane/layout implementation, built with the same locked release profile.
Both builds completed before measurements. The same harness ran sequentially, alternating build
order between repetitions. [Raw samples and source/binary hashes](verification/pane-layout-performance-2026-09-12.json)
identify the measured candidate without treating its unchanged Git HEAD as its source identity.

Values below are medians across three runs. CPU values are seconds unless indicated otherwise.

| Workload / metric | Baseline | Candidate |
| --- | ---: | ---: |
| 2 panes, 200 resize requests: server CPU | 0.0959 | 0.1074 |
| 8 panes, 200 resize requests: server CPU | 0.0328 | 0.0343 |
| 32 panes, 200 resize requests: server CPU | 0.0334 | 0.0341 |
| 2 panes: median resize request ms | 0.696 | 0.961 |
| 8 panes: median resize request ms | 0.223 | 0.222 |
| 32 panes: median resize request ms | 0.254 | 0.290 |
| 24×80 frame workload: median bytes per keystroke | 852 | 989 |
| 24×80 frame workload: server CPU / 1,000 keystrokes | 0.116 | 0.180 |
| Two viewers, 60×200 + 24×80: server CPU / 1,000 keystrokes | 0.120 | 0.094 |
| Real viewer: viewer CPU / 1,000 keystrokes | 0.344 | 0.392 |
| Real viewer: server CPU / 1,000 keystrokes | 0.271 | 0.304 |
| Real viewer: burst server CPU | 0.067 | 0.062 |
| Real viewer: burst completion seconds | 0.307 | 0.319 |

In the initial candidate, the stable frame-byte increase was 137 bytes (+16.1%) per median keystroke. Newly transmitted server,
viewer, layout revision, zoom and tab metadata are likely contributors; source inspection is not
an exact byte attribution. This cost needs follow-up profiling and reduction where metadata can
safely be omitted from unchanged deltas. Timing differences remain unresolved under the observed
contention; neither apparent improvements nor regressions establish a general performance claim.

Reproduction commands, using the same harness binary for both release binaries:

```sh
fux-xtask measure-layout PATH_TO_FUX
fux-xtask measure-frames PATH_TO_FUX --keystrokes 100 --config 24x80 --config 60x200+24x80
fux-xtask measure-viewer PATH_TO_FUX --keystrokes 100
```

`measure-layout` uses private runtimes and checks pane/PID identity after 200 alternating ±100
ratio-unit requests at 60×200, for 2, 8 and 32 panes. Request latency includes socket and JSON
overhead. Small rectangles can quantize these ratio edits to unchanged cell sizes, so this does
not replace a larger-resize workload. Frame and viewer tools retain their existing output-burst
workloads. The viewer tool uses an escape-stripped transcript, not a visible-latency screen model.
The raw JSON records every sample, load reading and metric rather than only these medians.

## Sparse metadata follow-up

Full frames now include connection identity and the complete tab catalog. Ordinary deltas inherit
identity and carry the catalog only when it changes. Applying or coalescing updates preserves
omitted metadata and honors explicit empty catalogs. Workspace switches publish full metadata.

The rebuilt release candidate was measured against the same baseline with the same harness,
three repetitions and alternating order. Both viewer configurations consistently measured **855
bytes per median keystroke**, versus baseline **852** and the initial candidate **989**. This
removes 134 of the original 137 added bytes; remaining overhead is 0.35% of baseline.
[Follow-up samples and source/binary hashes](verification/pane-layout-sparse-metadata-2026-09-12.json)
preserve this separate measurement without replacing the initial results.

| Workload / metric | Baseline | Sparse metadata candidate |
| --- | ---: | ---: |
| 24×80 server CPU / 1,000 keystrokes | 0.117 | 0.131 |
| Two viewers server CPU / 1,000 keystrokes | 0.110 | 0.137 |
| 24×80 burst bytes | 85,766 | 136,314 |
| 24×80 burst seconds | 0.171 | 0.286 |
| Two viewers burst bytes | 87,182 | 88,583 |
| Two viewers burst seconds | 0.165 | 0.167 |

These are medians across three runs. Host one-minute load was 13.21–14.39. Timing and burst
results remain unresolved: the single-viewer candidate burst ranged from 88,778 to 146,510
bytes, while CPU and latency medians also increased. Sparse metadata establishes the keystroke
byte reduction, not a general performance pass. Further profiling must distinguish scheduling
and frame pacing from added layout work; resize and real-viewer workloads need final reruns.

## Dimension-changing resize and refreshed output measurements

The revised `measure-layout` uses ±2000 ratio units. Before timing, it executes the identical
200-operation sequence and requires every operation to change at least one pane's dimensions.
It requires the sequence to restore initial sizes, then repeats it without inspection requests
inside the timed interval. Timed viewer bytes exclude setup/preflight. Final size and pane/PID
checks also pass. This verifies resize work rather than ratio-only no-ops; it does not directly
count PTY ioctls. Request latency includes socket/JSON work. CPU includes the final 100 ms drain,
while reported elapsed time excludes that drain.

Both locked release builds and harness checks completed before five paired repetitions, alternating
build order. Frame and real-viewer workloads use 500 keystrokes, so their byte medians are not
directly comparable to the earlier 100-keystroke samples. All 30 commands passed. The exact measured
candidate predates the keyboard navigation availability fix discovered during the concurrent full
review; its source and binary hashes are retained in the
[refreshed raw record](verification/pane-layout-performance-refreshed-2026-09-12.json).

Medians across five runs:

| Workload / metric | Baseline | Measured candidate |
| --- | ---: | ---: |
| 2 panes, 200 resizes: server CPU seconds | 0.1195 | 0.0630 |
| 8 panes, 200 resizes: server CPU seconds | 0.0312 | 0.0303 |
| 32 panes, 200 resizes: server CPU seconds | 0.0470 | 0.0382 |
| 2 panes: median request ms | 0.8882 | 0.3698 |
| 8 panes: median request ms | 0.1928 | 0.1745 |
| 32 panes: median request ms | 0.4082 | 0.2523 |
| 32 panes: p95 request ms | 0.7292 | 0.8350 |
| 24×80 bytes per median keystroke | 892 | 895 |
| 24×80 server CPU seconds / 1,000 keystrokes | 0.115 | 0.157 |
| Two viewers server CPU seconds / 1,000 keystrokes | 0.129 | 0.174 |
| Real viewer CPU seconds / 1,000 keystrokes | 0.397 | 0.291 |
| Real viewer server CPU seconds / 1,000 keystrokes | 0.302 | 0.224 |
| Real viewer burst seconds | 0.183 | 0.181 |

Every resize configuration adds exactly 15,400 transmitted bytes per 200 requests (77 per
request), while steady keystrokes add three bytes (+0.34%). These are stable measured costs.
The resize traffic increase is consistent with changed layout metadata, but this run does not
attribute individual fields. CPU/latency results remain inconclusive under one-minute host loads
of 12.65–25.51. Inspecting the pairs shows candidate 24×80 frame CPU lower in three repetitions
and higher in two; two-viewer CPU is higher in four and lower in one. The opposite movement in
real-viewer CPU and large within-build spread prevent assigning these differences solely to
layout code. The two-viewer CPU increase and 32-pane p95 remain follow-up profiling targets.
These measurements establish executed workloads and stable traffic overhead, not a universal
speed claim or final performance acceptance for later source changes.

## Final source after review fixes

Five alternating paired repetitions against baseline `1792223` completed after both navigation
and mouse-confirmation fixes. All 30 commands passed. The measured product/harness source hashes
were checked against the working tree before saving the
[final raw record](verification/pane-layout-performance-final-2026-09-12.json).
The workload and timing boundaries are unchanged from the dimension-changing benchmark above.

| Metric, median across five runs | Baseline | Final candidate |
| --- | ---: | ---: |
| 2 panes, 200 resizes: server CPU seconds | 0.0707 | 0.0712 |
| 8 panes, 200 resizes: server CPU seconds | 0.0453 | 0.0388 |
| 32 panes, 200 resizes: server CPU seconds | 0.0502 | 0.0320 |
| 2 panes: median request ms | 0.4441 | 0.4431 |
| 8 panes: median request ms | 0.2792 | 0.2655 |
| 32 panes: median request ms | 0.4485 | 0.1980 |
| 32 panes: p95 request ms | 1.2906 | 0.4012 |
| 2 panes: server RSS KiB | 11,824 | 12,400 |
| 8 panes: server RSS KiB | 12,976 | 13,168 |
| 32 panes: server RSS KiB | 16,928 | 17,184 |
| 24×80 bytes per median keystroke | 892 | 895 |
| 24×80 server CPU seconds / 1,000 keys | 0.110 | 0.108 |
| Two viewers server CPU seconds / 1,000 keys | 0.123 | 0.114 |
| Real viewer CPU seconds / 1,000 keys | 0.283 | 0.303 |
| Real viewer server CPU seconds / 1,000 keys | 0.233 | 0.251 |
| Real viewer burst seconds | 0.206 | 0.200 |

The earlier two-viewer CPU and 32-pane p95 increases did not recur with the final binary.
Pair-to-pair movement changes direction; shared-host one-minute load ranged from 5.76 to 53.52.
This repeat does not establish a persistent material timing regression, and it also cannot
establish that the candidate is generally faster. Real-viewer CPU medians rose about 7–8%; this
remains observational under the same contention. Raw samples preserve all repetitions rather
than discarding slow samples.

The stable wire cost is explained by the serialized fields. In the single-tab fixture, the old
repeated `tabs` object is 34 compact JSON bytes, while the new `layout_generation`/`zoomed` object
is 37. Their common outer braces cancel in the comparison: unchanged output adds three bytes,
or 0.34% of the measured median keystroke. During resize, the catalog also carries the destination
revision and first pane. With the three-digit revisions reached after the 200-operation warm-up,
the additional fields total 77 bytes per request, matching the observed 15,400-byte increase for
every 200-request workload. These fields support coherent layout and stale-destination checks;
removing their semantics would weaken the controls being measured.

The prompt's before/after workload measurement and follow-up investigation are recorded for the
final source. This is a comparison with the pre-change fux build, not a head-to-head Herdr
benchmark, a rendered-latency measurement, or a claim of universal performance superiority.
