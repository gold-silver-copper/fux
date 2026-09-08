# Paired screen-detection conformance

On eleven retained real-agent screens, zor's final bundled rules classify all eleven as expected and
herdr's classify eight. Zor recognizes all six input-required screens; herdr recognizes three.
This is conformance on fixtures used to develop zor's rules, **not held-out accuracy or universal
superiority**. Both production CLI matchers received the same normalized text and a supplied
agent identity, in disposable HOME/XDG directories with default bundled manifests.

| Measured outcome | zor | herdr 0.8.2 |
|---|---:|---:|
| Expected state on selected real screens | 11/11 | 8/11 |
| Expected state with a matched rule | 11/11 | 6/11 |
| Missed input-required screens | 0/6 | 3/6 |
| Non-Unknown states on derived negatives | 0/66 | 66/66 |
| Matched rules on derived negatives | 0/66 | 21/66 |

Herdr's 66 non-Unknown negative results include **45 Idle fallbacks with no visible evidence**,
12 visible blocker matches, five visible working matches and four visible idle matches. These are different evidence strengths;
the fallback count is not presented as 45 explicit visible-idle detections. Herdr's source
intentionally falls back to Idle for an identified agent without a matching screen rule. Zor
returns Unknown in that case. A screen verdict is not task completion in either measurement.

| Real screen | Expected | zor | herdr |
|---|---|---|---|
| Codex sign-in | Blocked | Blocked | Idle fallback |
| Codex directory trust | Blocked | Blocked | Blocked |
| Codex Working indicator | Working | Working | Working |
| Codex response, empty composer | Idle/input-ready | Idle | Idle fallback |
| Codex command approval | Blocked | Blocked | Blocked |
| Claude initial theme selection | Blocked | Blocked | Idle fallback |
| Claude Working footer | Working | Working | Working |
| Claude response, empty composer | Idle/input-ready | Idle | Idle (matched prompt-box rule) |
| OpenCode startup composer | Idle/input-ready | Idle | Idle fallback |
| OpenCode native shell permission | Blocked | Blocked | Blocked |
| OpenCode native question | Blocked | Blocked | Idle fallback |

The first local measurement found two missing zor OpenCode blocker rules, alongside the unrecognized
Codex response screen. Added the blocker rules from the existing real native-dialog captures and
reran both implementations on unchanged inputs. Codex’s recorded response composer now has narrow Idle recognition; active Working
and MCP startup rows exclude it. This does not establish a general idle rule. Herdr already recognized
the OpenCode permission screen before that change.

The 66 negatives comprise stale transcripts, quoted screens, shell output, echoed process names,
nested-tool output wrappers and malformed control strings, derived from each real screen. They
are detached text fixtures, not six new real-agent workloads. Exact copied live viewports can
still spoof passive matchers. These file commands do not parse an original terminal byte stream;
malformed-control cases measure screen-matcher specificity, not OSC-parser safety.

Codex 0.153.4 response/approval traces use a real authenticated OpenAI model. OpenCode 1.18.29
permission/question traces use the real application with a synthetic provider. Claude 2.1.263
contributes its real unauthenticated theme-selection screen and real authenticated Sonnet 5
working/response screens in bare mode with tools disabled. Herdr matches that live idle composer
with visible evidence; zor now recognizes the recorded manual-mode input-ready footer too. The narrow sample does not establish
Claude default-mode coverage, nested-tool execution guarantees, other versions/layouts or broader
agent support. The separate [inventory](detection-inventory.md) lists declared manifests, not
measured accuracy or executable support.

Herdr's file explanation interface supplies no OSC title/progress input, so neither backend
receives it here. Herdr's title detection and live PTY activity arbitration can be stronger than
screen-only matching; those paths are not measured or disabled in production. This comparison
also excludes process identification, heartbeat expiry, state hysteresis, evidence freshness,
live detection latency and observer overhead. Separate [live freshness](detection-freshness.md)
and [resource](resources.md) reports now cover bounded R5 scenarios.

Reproduce from the fux checkout with the verified herdr reference build and current zor binary:

```sh
cargo +1.91.0 build --manifest-path zor/Cargo.toml --locked --bin zor
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- capture-detection-screens --zor zor/target/debug/zor \
  --herdr /absolute/path/to/reference/herdr --output /tmp/new-detection-screens.json
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- verify-detection-screens /tmp/new-detection-screens.json
```

The [JSON report](detection-screens.json) records all 77 paired cases, source fixture paths/JSON
pointers, text hashes, expected labels, rule IDs, herdr visibility/fallback details, binary hashes,
source hashes and harness hash. Each process call has a ten-second deadline. No server, agent,
account or personal configuration is used by this comparison. Missing expected output or an
operational CLI error aborts collection; zor check's expected exit 1 without assertion headers is
accepted only with its exact normal verdict output.

The herdr binary SHA matches the persistent clean build recorded by
[controller setup](controller-setup.md), rebuilt after the original temporary binary disappeared.
Its 2,452 tracked build-copy files were checked against the unchanged reference, including symlinks. Four offline provenance/accounting tests reject
missing cases, mismatched scores and conflating fallback with visible evidence. They validate the
retained report; they do not rerun the CLI comparison or prove a live detection guarantee.

Historical provenance: this retained comparison predates the H4 headless CLI additions.
Its exact `zor/src/main.rs` bytes are archived under `tools/archive/zor/src/main.rs.txt` and
match the original recorded hash. Current-source captures retain their own attribution.
The Rust validator reconstructs the corpus from source fixtures and rejects missing rule
fields as well as the original omitted-case, score and false-visible-evidence mutations.
