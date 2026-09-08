# Live sign-in blocker and pane-loss freshness

Six live runs passed: three repetitions of actual signed-out Codex 0.153.4 under
fux+zor and the unchanged herdr 0.8.2 reference. The independent pane capture shows
“Sign in with ChatGPT” and “Press enter to continue” before timing begins and again
at the end. No prompt, trust acceptance or credential is supplied.

| Repetition | Zor first Blocked, ms | Herdr first Blocked | Zor close→absence, ms | Herdr close→absence, ms |
|---|---:|---|---:|---:|
| 1 | 227.67 | Not observed | 123.78 | 25.96 |
| 2 | 219.76 | Not observed | 126.21 | 26.11 |
| 3 | 179.26 | Not observed | 127.81 | 26.02 |

Herdr discovered the real Codex process but reported Unknown throughout each
approximately three-second observation window. This is an unmatched blocker, not
zero latency or evidence that waiting longer could never produce a state change.
Zor reports the visible blocker within the sampled window. Both remove the closed
pane from their agent observations. Herdr's Unknown here differs from its screen-only
file-matcher fallback, demonstrating why static rule matching cannot substitute for
live runtime measurement.

## Method and limits

The harness uses fresh private HOME/XDG/CODEX_HOME with an environment allowlist.
It starts a real Codex executable as the pane command, then launches zor's shared
service or uses herdr's integrated detection. It waits for the independently visible
sign-in dialog before polling controller state for three seconds. First-Blocked
latency begins at that successful pane read, not at Codex's internal render time;
a state already published before the first poll would produce only a sampling bound.
All controller replies and sample timestamps are retained, including initial empty
observations. The final pane capture confirms the dialog is still present.

Zor is polled through `zor status`; herdr through `agent.list` socket calls. The
50 ms sleep after each call, CLI startup, scheduling and controller startup all affect
the values. Fux uses its 80×23 headless viewport; herdr's larger default also displays
Codex's animated logo above the same sign-in dialog. The captures retain that layout
difference. These are end-to-end observations for these configurations, not isolated
classifier execution times or equal-geometry latency benchmarks.

After the window, the harness closes the owned pane using the respective generic
API and measures until the controller no longer lists it. This is explicit pane
loss, not a natural Codex exit test. Zor's detected PID and herdr's recorded shell PID
must disappear, and all owned server/service processes must exit normally. Fux's
final capture has input sequence zero. No forced cleanup is accepted.

This real startup/loss pair supplements the [screen conformance](detection-screens.md)
and [synthetic observer capture](capture-traffic.md) measurements. It establishes a
bounded live blocker-freshness result, not broad working/idle transition accuracy,
response correlation, or universal agent support. Those broader initial-agent
coverage requirements remain R4; no additional rules or production changes were
made for this experiment.

## Reproduction and validation

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- capture-detection-freshness \
  --fux target/debug/fux --zor zor/target/debug/zor \
  --herdr target/herdr-reference/build/debug/herdr \
  --codex /opt/homebrew/bin/codex \
  --herdr-provenance target/herdr-reference/provenance.json \
  --output /tmp/detection-freshness.json
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- verify-detection-freshness /tmp/detection-freshness.json
```

Use a new output path. The retained JSON pins binaries/source and reference-build
provenance, and contains initial/final captures, raw controller samples and cleanup
results. All six live runs and four mandatory offline evidence tests passed. The
tests reject converting an unmatched blocker to zero latency, substituting another
screen, inventing controller states, or claiming unproven PID cleanup. Python
compilation and `git diff --check` also passed.
