# Dashboard handoff checkpoint

This is loopback development evidence, not final product acceptance or published-pin CI.
The fixture creates disposable Local and two remote stacks, uses real koh gateways and a
controlling PTY, and never routes through an inherited user runtime. `handoff-final-pass.json`
records the binary paths and SHA-256 digests for the latest retained successful run.

Build fux/zor, then select the separately built development koh described in checkpoint 2:

```sh
CARGO_TARGET_DIR=/tmp/fux-multi-machine-build cargo +stable build --locked -p fux -p zor
FUX_BIN=/tmp/fux-multi-machine-build/debug/fux \
ZOR_BIN=/tmp/fux-multi-machine-build/debug/zor \
KOH_BIN=/tmp/strict-fux-koh-zor/koh-build/debug/koh \
python3 docs/verification/multi-machine/checkpoint-6/dashboard-handoff.py
```

The script requires the binaries explicitly. It verifies saved profile routing, inspection
without redundant control helpers, a non-default exact viewer, input isolation, detach suffix
discard, selection restoration, missing binding refusal, actual viewer SIGKILL, cancelled
preparation, terminal attributes, helper cleanup and surviving remote owners. It saves raw
PTY checkpoints only after synchronized frames finish. Startup/read/cleanup deadlines remain
bounded; a timeout fails the run.

With the existing native Betamax toolchain and a suitable installed monospace font:

```sh
cargo +stable build --locked --manifest-path tools/xtask/Cargo.toml \
  --features betamax --target-dir target/rust-harness
export FUX_BETAMAX_DIR=/tmp/zor-handoff-betamax
export FUX_BETAMAX_FONT='Noto Sans Mono CJK SC'
for recording in docs/verification/multi-machine/checkpoint-6/0*.ansi; do
  target/rust-harness/debug/fux-xtask betamax-record \
    /tmp/fux-multi-machine-build/debug/zor "$recording" 30 180 "$(basename "$recording" .ansi)"
done
target/rust-harness/debug/fux-xtask betamax-report "$FUX_BETAMAX_DIR"
```

`betamax-record` feeds the original PTY bytes into Betamax/Ghostty, not a reconstructed text
screen. It requires the native feature and output directory, rejects oversized recordings and
unfinished synchronized frames, and retains replay state alongside each checkpoint.

Seven wide-terminal states were individually reviewed: aggregate duplicate task names,
inspection, viewer, clean return, missing attachment binding, killed viewer and cancelled
preparation. Narrow terminals, reconnect/expiry/restart product flows and the remaining full
milestone checks are still pending. See the implementation ledger for failures, refinements,
exact verification scope and publication state.
