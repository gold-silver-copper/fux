# Fux and zor local verification

Run from the repository root. Prepare the pinned Zig toolchain and CJK font as
described in [the Betamax setup](betamax-harness.md). Export
`FUX_BETAMAX_FONT='Noto Sans Mono CJK SC'` and put Zig 0.15.2 on PATH. Node is
required for the synthetic bundled-adapter scenarios. No provider account is used.

Targeted ownership, routing, resume, delivery and rendering checks:

```sh
cargo +stable run --manifest-path tools/xtask/Cargo.toml --target-dir target/codebase-runner --locked -- verify-codebase targeted
```

Full local fux/zor gate:

```sh
cargo +stable run --manifest-path tools/xtask/Cargo.toml --target-dir target/codebase-runner --locked -- verify-codebase full
```

Both profiles use the existing Rust gate process owner and scenario harness. They
print a fresh external evidence directory containing `gate.json`, per-command
stdout/stderr, failure artifacts and a Betamax HTML/PNG/state/raw-stream report.
Source identity is checked before and after execution. Do not edit the checkout
while the gate runs. An interrupted, failed or source-changing run cannot report
completion. A failed scenario still gets a render-report attempt when capture exists.

The full profile runs formatting, strict Clippy, all workspace tests (including
all local CLI and automation tests with required explicit zor), supported zor
feature combinations, standalone harness and provider-fixture checks, docs,
package verification and existing offline evidence validators. It builds the
binaries before process scenarios, executes checks sequentially, and has no test
exclusions. It neither cleans nor changes companion repositories and does not
publish packages. `CARGO_TARGET_DIR` may select an external root build directory;
the integration helpers retain their existing `target/rust-harness` and
`target/rust-zor-fixtures` directories to avoid nested Cargo target-lock conflicts.

The targeted profile includes the controller reference model and transition tests,
wire contracts, focused viewer scenarios, and real launch/resume/delivery/recovery/
worktree scenarios. Use [trace replay](controller-trace-testing.md) for an individual
minimized failure, or the existing `scenario NAME FUX [ZOR]` entry point after
building the binaries when diagnosing a single scenario.

A passing command means the automated checks passed. Review the labeled Betamax
images and record the findings separately. Release baseline/candidate performance
measurements and native terminal acceptance also remain separate; this gate does
not infer those outcomes from automated test success. Offline provider evidence
validators check retained fixtures, not live provider sessions.

## Release measurements

Build both comparison revisions with the same release toolchain before measuring.
Use the same built xtask executable for both. Keep `FUX_BETAMAX_DIR`,
`FUX_DIAGNOSTICS` and `ZOR_DIAGNOSTICS` unset during timing, run on an idle host,
alternate baseline/candidate order and retain stdout JSON from every invocation.
The harness built by the full gate provides:

```sh
target/rust-harness/debug/fux-xtask measure-interactions /absolute/path/to/release/fux
target/rust-harness/debug/fux-xtask measure-recovery /absolute/path/to/release/fux /absolute/path/to/release/zor
```

The first command records raw scroll/render and manager-lookup latencies, CPU,
PTY bytes and sampled RSS. Scroll completion requires the actual history offset
and changed pane rows to be visible. The second measures public-CLI reconciliation
after an injected lost creation reply; attachment, exact process identity, pin
release and duplicate-free retry are asserted. CLI exit observation uses 10 ms
polling; per-request timestamps expose proxy handling and inter-request waits.
Proxy admission wakes on socket readability. Both commands use private fixture
roots and clean up their owned processes.

Use the existing `measure`, `measure-frames`, `measure-layout`, `measure-memory`,
`headless-journal` and `headless-performance` commands for complementary workloads.
The improvement report links the exact executed commands and raw comparison data.
Sampled RSS and reader backlog must not be presented as kernel high-water memory
or internal runtime queue allocation.
