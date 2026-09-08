# Observer capture traffic

Nine live zor runs passed: three repetitions at 1, 4 and 8 observed panes. Zor made
zero capture requests in every 4.2-second idle window. After the same synthetic
output bursts used in the paired [resource comparison](resources.md), every pane's
observer capture contained the completion marker. This measures actual zor→fux
workspace traffic; herdr's in-process detection reads are not measured by this
interface and are stored as `null`, not zero.

The table shows medians of three runs. Capture bytes count newline-delimited JSON
frames, excluding the 8-byte connection handshake; text bytes count decoded UTF-8
capture text, excluding JSON escaping and metadata. Event counts cover burst-window
event frames. The JSON also retains event bytes and individual completed requests.

| Panes | Idle captures | Idle lists | Burst captures | Capture request bytes | Capture reply bytes | Capture text bytes | Events | All markers received ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 0 | 1 | 2 | 262 | 3436 | 2868 | 3 | 147.20 |
| 4 | 0 | 4 | 8 | 1048 | 13740 | 11472 | 12 | 160.41 |
| 8 | 0 | 8 | 16 | 2096 | 27484 | 22944 | 24 | 194.23 |

Each window has one workspace per pane. Lists show that idle observation still
performs discovery checks; zero captures does not mean zero work or zero IPC.
The observed burst count of two captures per pane is a result for this workload,
not an API guarantee or a fixed future budget.

## Scope and herdr comparison

A transparent private proxy replaces each owned fux workspace socket after startup.
Only zor uses the published path. Fixture listing and input submission go directly
to the upstream socket, so they do not enter the observer counters. One proxy record
is retained per completed request/reply or event frame. Counters are assigned to
windows by completion time; connections and requests in progress across boundaries
are not separately counted. Manager discovery IPC is outside this measurement.

The named-claude synthetic worker emits 16,907 application bytes per pane before
PTY translation. No model or credentials are used. The observer must establish all
event streams and initial captures before warmup. Following burst input, every
workspace must have a proxied capture containing `BURST_DONE`. The final one-second
settling window includes subsequent captures. Request bytes, reply bytes and decoded
text bytes are distinct measures, not three additive payloads.

“All markers received” is time from the first input request until the harness sees
a completed proxied capture with the marker for every pane. It includes sequential
input, proxy forwarding, and 30 ms polling; the proxy reads frames byte by byte and
adds substantial overhead. It is not production latency or time until zor publishes
a classified state. CPU and memory from the unproxied [paired report](resources.md)
remain the appropriate separate resource measurements.

The unchanged herdr reference calls `terminal.detection_text()` inside its detection
tasks, after `decide_detection_screen_read` (see
[pane.rs](../../references/herdr/src/pane.rs:883) and its second path at line 2463).
The read decision can skip a scan; a periodic detection loop is not proof of one
capture per tick. There is no equivalent external multiplexer request at this call
site for this proxy to count. Internal read counts/bytes remain **unmeasured**, not
zero, and no numerical capture-traffic superiority claim follows. The existing
paired 18-case resource workload covers both systems; this additional experiment
quantifies the extra IPC boundary specific to fux+zor.

## Reproduction and validation

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- capture-traffic \
  --fux target/debug/fux --zor zor/target/debug/zor \
  --output /tmp/capture-traffic.json
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- verify-capture-traffic /tmp/capture-traffic.json
```

The output path must be new. The retained JSON includes source/binary hashes, raw
proxy records, exact window boundaries and normal process cleanup. The harness has
request, startup, connection-count and cleanup bounds. It rejects unexpected proxy
failures and verifies every owned worker PID is gone. All nine runs ended normally.
Four mandatory offline tests passed: matrix/provenance, accounting mutations,
exclusion of harness input, and fresh capture coverage for every workspace. Python
compilation and `git diff --check` passed. No production source was changed.

Together with the paired resource report, this addresses the bounded §8.7 overhead,
latency, memory, scaling and available capture-traffic measurements, with herdr's
internal count explicitly unmeasured. The [workflow report](workflow.md) separately
covers the two-worker verification/artifacts/cleanup comparison.

The subsequent [live freshness report](detection-freshness.md) supplies the bounded
real Codex blocker/loss measurement. R5 measurement categories are now retained;
broader real-agent coverage remains R4, with R6/R7 also open.
