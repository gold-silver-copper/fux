# Worker continuity after service failure

Zor's separate service can be killed and restarted while fux keeps the original
worker alive. In this synthetic scenario, coordination resumes with the same
session and prompt receipt, an explicit retry adds no duplicate input, and a new
prompt reaches that same worker. Killing the PTY owner ends the worker in both
fux and herdr. This measures the benefit of separating orchestration from terminal
ownership; the injected faults do not remove equivalent components.

The [harness](../../tools/comparisons/service_failure.py) and
[evidence](service-failure.json) cover three repetitions of each fault:

| SIGKILL target | Role of killed process | Original worker survives | Coordination exercised afterward |
|---|---|---|---|
| zor service | External observer and task controller | 3/3 | Restart service, retain session and receipt, retry, report, send next prompt |
| fux server | PTY owner | 0/3 | None; owner restart/resume is outside this test |
| herdr server | Combined orchestration and PTY owner | 0/3 | None; owner restart/resume is outside this test |

Each case starts the same compiled synthetic line worker in a private multiplexer,
sends `before-crash`, and waits for both its input log and response marker before
injecting SIGKILL. Herdr receives an explicit idle integration report before its
`agent.prompt` call. The executable is named `claude` only for herdr foreground
admission; this is not a real-agent detection or integration test.

The fux cases adopt the already-running worker into a zor task through the service
API. After zor-service loss, the replacement service has a different incarnation,
but the retained session, pane/process target, and prompt receipt match. The
pending wait remains pending until the fixture supplies a prompt-scoped report
for the response emitted before the crash. It then becomes `response-observed`.
This is explicit evidence supplied after recovery, not an automatically recovered
adapter report. The next prompt produces `after-crash` in the same worker's log.
The task stays `open`; no verified task completion is claimed.

Final logs contain exactly one `before-crash` in every case, plus one `after-crash`
in the zor-service case. They are checked again after process cleanup. Reconciliation
occurs within receipt retention; no receipt expiry, mid-submission crash, concurrent
human input, adapter restart, managed-launch recovery, remote transport, or durable
exactly-once application processing is established here.

## Architecture and interpretation

Herdr's `src/server/headless.rs` describes a server that initializes AppState and
PTYs and continues after client disconnect. Its `src/app/mod.rs` loads persisted
sessions through `persist::restore`; `src/persist/restore.rs` can spawn terminal
runtimes and apply agent resume metadata. Those restoration paths are not tested
or disabled here. Loss of an original process does not demonstrate lack of session
resume. Herdr client disconnection also does not kill its server, so this result
must not be presented as a client-disconnection advantage.

Zor's observation/task service is an external fux API consumer. Keeping that
service separate provides a smaller failure domain for orchestration changes and
crashes. The fux-server control makes the limit explicit: the tested worker does
not survive losing fux itself. There is no server handoff, PTY-owner persistence, performance,
or universal superiority conclusion in these counts.

## Provenance and reproduction

The JSON pins all three binary hashes, both harness/helper hashes, worker source,
platform, and the herdr commit. The harness requires the exact herdr binary hash
from the previously verified [reference build](prompt-boundary-build.json).
See the [reference build instructions](prompt-boundary.md#reproduction).
No product or reference source changes were needed for this comparison.

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- capture-service-failure \
  --herdr /absolute/path/to/reference-build/target/debug/herdr \
  --fux target/debug/fux --zor zor/target/debug/zor \
  --repetitions 3 --output /tmp/service-failure-new.json
```

Run from the fux repository with Python 3.11+, `/usr/bin/clang`, Unix sockets and
PTY access. Output must be a new file. HOME/XDG paths, sockets, logs and processes
are disposable and private. Socket and process waits are bounded. An alarm limits
the synthetic worker to 45 seconds if an unexpected owner failure leaves it alive;
setup must finish within 30 seconds so that alarm cannot satisfy the measured
post-crash disappearance window (at most 5 seconds waiting for the killed owner,
then 8 seconds for the worker). Cleanup attempts both process stops and worker-exit
confirmation even if another cleanup step fails. It never signals a raw worker PID.

The first-party capture is now Rust. Historical evidence above remains unchanged;
the exact Python source is archived under `tools/archive/tools/comparisons/`.
The default build provenance still selects `prompt-boundary-build.json`. For the
subsequent verified build of the same reference commit, pass
`--herdr-provenance docs/comparisons/controller-setup-build.json`; the tool checks
its binary hash and records the actual build metadata. A one-repetition migration
run passed all three scopes at `/tmp/fux-rust-service-failure-capture.json`.
It establishes synthetic continuity and cleanup, not real-agent session resume.
