# Release performance review — focused investigation pending

The main comparison completed all 48 runs: three alternating blocks of eight
workloads against baseline and candidate release binaries. Commands, hashes, host
observations and raw samples are retained in `release-comparison/manifest.json`
and its per-run folders. The host was 85.00–90.61% idle immediately before each
run; this is a precondition observation, not proof of constant isolation throughout
each workload. No builds overlapped timed workloads.

The baseline is the retained post-routing-fixture checkpoint. It is not the lost
original dirty checkout. Both variants use the same stable release toolchain and
the same harness executable within this comparison.

## Main results

Latency entries below are pooled median / p95 in milliseconds. These are
descriptive percentiles from three blocks, not confidence intervals.

| Workload | Baseline | Candidate |
| --- | ---: | ---: |
| single-viewer key latency | 2.088 / 3.973 | 1.929 / 4.206 |
| four-viewer key latency | 0.254 / 0.465 | 0.218 / 0.455 |
| scroll latency | 1.795 / 1.959 | 1.796 / 1.977 |
| manager latency | 0.107 / 0.153 | 0.092 / 0.171 |
| recovery latency | 42.454 / 55.170 | 41.401 / 58.969 |
| recovery retry latency | 29.105 / 42.487 | 34.393 / 43.516 |
| journal inspect latency | 12.628 / 15.710 | 13.403 / 15.768 |

| Resource/work | Baseline median | Candidate median |
| --- | ---: | ---: |
| 20k output burst (s) | 0.172 | 0.183 |
| four-viewer frame bytes/key | 855.500 | 855.500 |
| four-viewer CPU (s/1000 keys) | 0.147 | 0.100 |
| resize 2 panes (ms) | 0.253 | 0.253 |
| resize 8 panes (ms) | 0.134 | 0.133 |
| resize 32 panes (ms) | 0.119 | 0.114 |
| 10k plain+wide history RSS (KiB) | 40144.000 | 40208.000 |
| scroll PTY bytes | 73985.000 | 73950.000 |
| scroll server CPU (s/100 events) | 0.009 | 0.009 |
| scroll viewer CPU (s/100 events) | 0.029 | 0.029 |
| journal adopt child CPU (ms/op) | 4.197 | 4.408 |

## Interpretation and limits

- Scrolling medians are effectively equal. The first two blocks reverse the
  direction of small scrolling and manager differences; this does not establish
  a stable speedup. Single-viewer p95 is slightly higher despite a lower median.
- Resize frame bytes are exactly equal for each 2/8/32-pane workload. Four-viewer
  median key frame bytes are equal. Burst frame bytes vary with coalescing: baseline
  79,378–89,521 and candidate 82,335–89,543, with the direction reversing by block.
  Nothing here supports removing identity/revision metadata. This later checkpoint
  comparison cannot retrospectively attribute an older pre-checkpoint byte increase.
- Highest observed fux RSS across recorded checkpoints is 40,432 / 40,384 KiB;
  highest observed zor RSS in pressure cases is 27,728 / 25,680 KiB. These are
  maxima of discrete samples, not kernel high-water counters or summed process-tree
  memory. The 10k-history median difference is 64 KiB (about 0.2%).
- Reader decoder pending high-water reaches 21,688 / 21,592 bytes. It measures
  reader-side framing backlog, not server/controller queue allocation. Internal
  boundedness remains covered by dedicated queue/read tests.
- Existing idle CPU reporting rounds to 0.001 seconds: both variants report 0.000
  seconds per ten-second idle sample; this is not a claim of literally zero CPU.
- Recovery and journal wall time includes CLI startup and the bounded subprocess
  runner’s 10 ms polling. Recovery also includes the proxy’s 10 ms accept polling.
  Journal inspect median rises about 0.775 ms while child CPU is 3.313 / 3.320 ms
  and p95 is nearly unchanged. CPU/sample clocks are calibrated against process
  CPU time by the existing resource sampler.

The main run shows a higher pooled median for recovery retries (about 18%) and
higher CPU in several pressure phases. The recovery retry block medians overlap
substantially (baseline 28.4–37.3 ms; candidate 28.7–36.3 ms). Pressure burst CPU
also varies materially; these observations must not be hidden by the aggregate
latency improvements. A focused alternating follow-up adds reaped zor child-CPU
counters to recovery and repeats recovery/pressure workloads. Its results remain
pending. No product optimization or assertion weakening has been applied to make
these numbers improve.


The first focused follow-up stopped at block 2, candidate pressure, after its
first three cases passed. The four-pane/four-viewer slow-reader case timed out
with the old generic `performance fixture observation` error. This is an unresolved
in-scope verification failure, not a completed performance conclusion. The failed
run and preceding results are retained under `release-followup-incomplete/`.
The first follow-up block had near-equal retry wall medians (34.260 / 34.531 ms),
lower candidate retry child CPU (3.866 / 3.801 ms) and a reversed single-pane burst
CPU difference (5.046 / 4.643 ms); these partial observations do not erase the
subsequent failure.

The harness now reports the failing readiness/workload stage and each reader's
counters and observed fixed markers, without extending the existing deadline.
A fixed five-repetition diagnostic run completed successfully against the same
candidate binaries: all 20 cases passed, including five four-pane/four-viewer
slow-reader cases. Its raw report and log are retained as
`pressure-diagnostic-candidate.json` and `pressure-diagnostic-candidate.log`.
This is non-reproduction, not a fix for the earlier timeout.

Source inspection found that the attachment writer awaits each complete framed
write before dequeuing another message; a write timeout ends the connection.
The outbox has one consumer and uses a retained notification permit. Pane deltas
carry complete changed rows, and coalescing replaces rows in sorted row order.
These observations do not support the proposed partial-write cancellation or
lost-notification explanations. They do not establish the original failing phase,
which the old harness did not retain.

A separate, predetermined three-block alternating recovery/pressure comparison
completed with all twelve runs passing and phase diagnostics enabled. Raw evidence
and analysis are retained under `release-followup-diagnostic/`. The earlier
incomplete comparison remains retained and unresolved; no timeout cause or fix is
claimed.

## Completed focused comparison

Each recovery variant contributes 36 operations across three alternating blocks.
The following are pooled medians; CPU covers the reaped zor CLI process, including
startup, while wall time also includes fixture/subprocess polling.

| Measurement | Baseline | Candidate |
| --- | ---: | ---: |
| Recovery wall time (ms) | 40.828 | 42.256 |
| Idempotent retry wall time (ms) | 29.604 | 35.973 |
| Recovery child CPU (ms) | 4.235 | 4.600 |
| Idempotent retry child CPU (ms) | 3.638 | 3.839 |

Retry wall time is 21.5% higher in this follow-up, so the earlier 18% increase
cannot be dismissed as a one-run anomaly. The child CPU median difference is
0.201 ms (5.5%), much smaller than the 6.369 ms wall difference; that alone does
not establish the reason for the remaining elapsed time. Candidate retry block
medians are 30.002, 33.736 and 38.548 ms, versus baseline 29.708, 30.713 and
28.977 ms. Retry p95 is 41.819 / 45.160 ms. Further attribution remains open.

Pressure CPU differences are less consistent: single-pane/single-viewer burst
medians are 5.742 / 5.932 ms, four-pane/four-viewer ordinary readers are
23.435 / 21.688 ms, and the slow-reader case is 12.168 / 14.724 ms. Individual
blocks reverse direction. These observations do not demonstrate a stable overall
pressure improvement or identify the earlier timeout's cause.

After timing ended, Cargo confirmed all four measured release binaries fresh for
their current source trees and their hashes unchanged from both comparisons.
`release-provenance-verified.json` retains the commands, product source hashes and
current harness hash. No build overlapped the timed follow-up.

## Request timing attribution

A subsequent fixed six-run recovery comparison adds opt-in proxy timestamps;
`recovery-attribution/` retains all raw samples and analysis. All runs passed.
Every one of the 72 retries made exactly `pane-location` followed by
`release-pane-pin`, with ordered timestamps within the measured CLI interval.
The instrumentation retains at most 256 fixed operation names and timestamps,
without request or reply payloads. Neither proxy nor subprocess polling changed.

| Retry interval, pooled median (ms) | Baseline | Candidate |
| --- | ---: | ---: |
| Entire CLI observation | 30.549 | 34.222 |
| Start to first proxy acceptance | 11.570 | 11.441 |
| Sum of handling both requests | 0.826 | 0.837 |
| Between the two accepted requests | 13.136 | 13.780 |
| Last proxy response to CLI observation | 6.995 | 7.240 |

Interval medians do not add to the whole-operation median. Means do: total mean
time is 32.995 / 33.971 ms, with first-accept means 11.578 / 11.890, handling
0.826 / 0.831, inter-request 13.061 / 13.090, and final observation
7.531 / 8.161 ms. Most of the roughly 0.976 ms mean difference lies before first
acceptance or after the final response, rather than in proxy/server handling.
Those intervals include CLI work, OS scheduling, and fixture polling; the current
timestamps cannot divide them further. Median child CPU is 4.093 / 4.379 ms.
These results narrow attribution and disfavor a large server-side RPC regression,
but do not prove that all candidate overhead is a measurement artifact.

The pressure fixture also had an evidence gap: it discarded `split` and
`send-keys` reply status. It now requires completed replies, so a control rejection
will fail at that request instead of becoming a later missing-marker timeout.
Readiness diagnostics retain the last zor CLI success, observation count and stale
flag. This improves failure localization; it does not establish that a rejected
request caused the historical timeout.


## Active reproduction and next controlled comparison

The fixed 200-case candidate pressure campaign (10 batches × 5 repetitions × 4
cases) completed successfully: all 200 cases, including 50 slow-reader cases,
passed. The campaign was configured to stop at the first failure. Every batch
report hash and the unchanged executable hash were verified. Raw evidence and the
manifest are retained under `pressure-reproduction/`. This is correctness
non-reproduction, not a fix or performance comparison. The historical timeout
still lacks a known cause; later success does not supply its missing phase data.

Separately, a source change replaces the launch proxy's unconditional 10 ms
accept-retry sleep with socket-readiness polling, keeping the same maximum
cancellation-check interval. It is not built into the active campaign executable.
Formatting, strict Clippy and 36 library + 36 binary harness tests passed after
the campaign. The fixed six-run baseline/candidate comparison completed successfully; results
are below. The intended test is whether removing this artificial
per-connection delay changes the observed retry-latency difference.


## Readiness-based proxy comparison and performance disposition

`recovery-readiness/` retains all six successful alternating runs and analysis.
The same product binaries and request assertions were used; only fixture admission
changed from unconditional sleep to readable-socket polling. CLI exit observation
still polls every 10 ms. All 72 retries have two ordered manager requests.

| Measurement, pooled median (ms) | Baseline | Candidate |
| --- | ---: | ---: |
| Recovery wall time | 27.028 | 26.799 |
| Retry wall time | 14.288 | 14.630 |
| Retry child CPU | 3.492 | 3.463 |
| First proxy acceptance | 3.778 | 3.728 |
| Proxy handling sum | 0.569 | 0.574 |
| Between proxy requests | 0.055 | 0.047 |
| Last response to CLI observation | 9.447 | 10.164 |

Retry p95 is 15.776 / 15.890 ms. The remaining median wall difference is
0.342 ms (2.4%); candidate retry child CPU is slightly lower. The earlier
13 ms inter-request gap has collapsed to roughly 0.05 ms for both products.
This controlled change confirms material fixture-induced delay and substantially
reduces the observed baseline/candidate difference. It does not retroactively
make earlier samples invalid, prove identical product cost, or justify claiming
a product speedup. Most remaining wall time is outside RPC handling and includes
coarse exit observation. No further product optimization is justified by these
measurements; identity validation, durability and backpressure remain intact.

The measured-regression investigation is complete within these documented
instrument/host limits. The historical pressure timeout is a separate unresolved
correctness observation: all 200 reproduction cases passed, but no cause or fix
for that earlier failure has been established. The OS power log for 03:10–03:13
local time does not show a sleep/wake transition during the original failure
window; it does not explain scheduling or prove absence of other host stalls.
