# Headless controller setup and unavailable-service errors

Recaptured on 2026-09-08 after native dashboard integration. The earlier report is
preserved unchanged in `history/controller-setup-pre-native.json`; current counts
below come from the fresh Rust capture, not edited historical measurements.

Both stacks reached an overview containing the synthetic fixture pane in all three runs.
Both cold-service errors identified a recovery command. In this prepared headless path,
fux+zor used two non-polling startup invocations; herdr used three. This is not an installation
benchmark, a human-effort score, or the number of commands needed to open the normal herdr TUI.

| Step | fux + zor | herdr 0.8.2 |
|---|---|---|
| PTY owner | `fux serve` creates the configured default workspace/pane | `herdr server` starts the owner |
| Workspace | Created by the preceding command | `herdr workspace create --cwd PATH --focus` |
| Controller overview | `zor dashboard --once` starts its service and reads it | `herdr agent list` reads the integrated controller |
| First response | Explicit stale snapshot: “initial scan pending” | Empty agent list while discovery catches up |
| Additional overview reads, runs 1/2/3 | 1 / 1 / 1 | 3 / 3 / 5 |
| Total startup invocations through first populated overview | 3 / 3 / 3 | 6 / 6 / 8 |
| Subsequent read | Reuses the same zor service incarnation | Uses the same running herdr owner |

Additional reads use a 50 ms interval after each CLI returns. Their counts depend on scheduling,
CLI overhead and discovery timing; they are **not additional intrinsic setup steps** or a latency
benchmark. The interactive zor dashboard refreshes automatically. Herdr's ordinary `herdr`
entry point starts/attaches its integrated UI; this experiment explicitly exercises headless
commands. We make no universal usability claim from this selected path.

The fixture requires already-built binaries and sets an explicit default-command configuration
for each owner. Herdr's lab config disables onboarding/update checks and selects a non-login
worker. Two candidate config files cover debug/release application directories; the tested debug
build uses `herdr-dev`. Fux's lab config sets one argv vector. These are disclosed experiment
prerequisites, not measured installer or human onboarding work.

The synthetic compiled worker is named `claude` for process admission and waits for input.
This measures discovery and controller setup, not Claude accuracy or task verification. The
harness verifies that the real created pane appears in herdr's list and that zor's observation
contains the actual worker PID with input_sequence0. No prompt is sent. Initial empty/pending
responses do not count as success.

## Actionable errors

An absent zor service returns exit1 and plain-text stderr:

```text
zor: zor service unavailable; start `zor serve` with the same --directory: ENOENT: No such file or directory
```

An absent herdr service returns exit1 with structured stderr, including code `server_not_running`,
the selected socket path, and an instruction to run `herdr` to start or attach. Both provide a
human recovery action; herdr also provides a machine-readable CLI error code in this scenario.
Zor's separate structured service API is not reachable when its endpoint is absent. The report
retains the complete actual diagnostics and every invocation's exit code/stdout/stderr.

`zor shutdown` stopped its controller endpoint while fux and the same worker remained alive,
after which `zor status` again gave the unavailable-service diagnostic. Herdr has no separate
agent-controller stop in the tested interface; its server owns the PTYs. This distinction is
recorded as an unsupported equivalent action, not as a failed herdr shutdown. The separate
[service-failure comparison](service-failure.md) measures the different failure scopes.

## Reproduction and evidence

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- capture-controller-setup \
  --fux target/debug/fux --zor zor/target/debug/zor \
  --herdr target/herdr-reference/build/debug/herdr \
  --herdr-provenance docs/comparisons/controller-setup-build.json \
  --repetitions 3 --output /tmp/new-controller-setup.json
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- verify-controller-setup /tmp/new-controller-setup.json
```

New captures use the Rust harness and exact C worker, recording their current source hashes.
The retained historical Python harness is a nonexecutable provenance archive; its original
results and hashes are unchanged. Omit the verifier report argument to check that retained
report and the original negative regressions.

The [runtime report](controller-setup.json) retains six runs and exact command transcripts,
configuration contents, startup/read counts, process cleanup results, binary versions/hashes,
harness/helper/worker hashes and relevant source hashes. Calls have a 12-second timeout, discovery
loops an eight-second polling deadline, and owned server cleanup a ten-second grace followed by
bounded forced cleanup that fails the run. The worker has an independent 60-second lifetime bound.
Zor's automatically started service is stopped through its owned endpoint; no service exit code
or parent reaping is claimed. All owner processes exit0 and worker absence is confirmed.

The earlier disposable herdr build had been removed. Rebuilt the unchanged reference commit
`94f6d9c0d9bb9cf9ffae99d8bbfb09e9bf2fc9e0` from `git archive` under the ignored
`target/herdr-reference` directory with Rust1.95.0, Zig0.15.2 and locked Cargo dependencies.
The Zig archive checksum was verified before extraction; after compilation all 2,452 tracked
source bytes/symlink targets matched the reference. The [build record](controller-setup-build.json)
pins that new binary, archive and build-log hashes. It is a new binary hash, not a relabeling of
older comparison evidence. The [earlier build recipe](prompt-boundary.md#reproduction) documents
the same archive/private-cache procedure; set its build directory and toolchain to these paths
when regenerating this build record. Reference sources were not modified.

Five offline tests check retained provenance, all repetitions, populated-overview acceptance,
error semantics, poll-inclusive counts and cleanup/no-input claims. They are mandatory in the
combined gate and require no agents/accounts. They do not rerun the real comparison. The report was rerun after the reviewed dashboard fixes to retain matching current source
provenance. Other bounded R5 comparisons are recorded in the completion checklist.
