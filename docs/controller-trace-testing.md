# Reproducible controller traces

The fux library test `generated_controller_traces` compares a specification model
with two real controllers and two panes. The model records interaction ownership,
captured rename targets and history recency. It does not inspect controller modes,
history containers, parser buffers or cleanup helpers. The specification determines
when outstanding lookup/history responses become invalid before delivering them.

Events include wheel browsing, private focus, Copy, rename submission, Escape,
normal-input history restoration, buffer changes, resize, target removal,
attachment replacement, lookup, delayed responses, fragmented paste, prefix
commands and captured selection gestures. After each event, both
viewers must match the ownership/history specification. Rename must emit exactly
one request for its captured pane, with no unexpected manager/action/clipboard
effect. The fixture bounds visible retained state to its two panes.

Paste start, content and end are separate events, so buffer/target changes can
cancel an interaction while its parser still owns the unfinished paste. Synthetic
content includes prefix bytes, carriage return and escape sequences. The prefix
parser also receives UTF-8/SS3 input and bracketed paste in one- or two-byte chunks;
ordinary bytes must survive exactly and command-looking paste must stay literal.
Selection press, motion and release events track ownership independently of
keyboard mode. Cancellation must retain the gesture tail until release or a fresh
press, including after resize, buffer change and target loss.

Run from the repository root:

```sh
# Default deterministic corpus: 256 seeds × 128 events.
cargo +stable test -p fux --lib generated_controller_traces

# Reproduce a generated sequence.
FUX_CONTROL_SEED=67 cargo +stable test -p fux --lib generated_controller_traces

# Cargo runs tests in the crate directory, so use an absolute trace path.
FUX_CONTROL_TRACE="$PWD/docs/verification/codebase-improvement/control-traces/history-escape.json" \
  cargo +stable test -p fux --lib generated_controller_traces
```

On failure, deletion minimization retains the same reported invariant violation.
The test writes `trace.json` and `failure.json` to a temporary directory and prints
the seed and exact replay command. Replay input is bounded to 64 KiB and 256
events. Traces contain synthetic events, not terminal or prompt data.

The retained three-event history/Escape trace exposed a specification mistake:
Escape restores the most recently manipulated history, preserving the other pane.
The corrected model follows the documented contract. This was not a product
defect, and the saved trace now passes.

## Real terminal counterparts

The standalone harness shares bounded viewer helpers through
`tools/xtask/src/scenarios/viewer.rs`. Focused modules under `viewer/` contain
history, application mouse, transfer, tiny-layout, modal, gesture and delayed-manager scenarios.
The `viewer` composition scenario retains its existing cross-feature assertions
and still invokes tiny-layout checks. Tiny layout is also directly runnable:

```sh
cargo +stable run --manifest-path tools/xtask/Cargo.toml -- \
  scenario viewer-tiny-layout /absolute/path/to/fux
cargo +stable run --manifest-path tools/xtask/Cargo.toml -- \
  scenario viewer-modals /absolute/path/to/fux
cargo +stable run --manifest-path tools/xtask/Cargo.toml -- \
  scenario viewer-gestures /absolute/path/to/fux
```

`viewer-history-controls` covers the model's A/B history and Escape behavior with
real PTYs and independent viewers. The gesture fixture's existing fresh-press and
auxiliary-tail assertions are shared with that scenario. The modal fixture checks
one-Escape dismissal followed by application input and unchanged pane/tab identity
and layout. The broad `viewer` scenario includes the modal fixture.

These bounded traces complement the real raw-byte mouse/paste and delayed-operation
tests. They do not enumerate every parser sequence or prove OS-level delivery from
the in-memory model alone. Automatic whole-scenario failure artifacts and fresh
Betamax verification remain part of the broader improvement task.
