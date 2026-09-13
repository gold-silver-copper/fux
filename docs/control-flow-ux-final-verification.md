# Control-flow UX final verification

The implementation fixes independent pane scrolling, one-press dismissal to normal
input, and the confirmed related ownership, scheduling, gesture and hint defects.
The [mode matrix](control-flow-mode-coverage.md) records each transient action's
completion, cancellation, target-loss and failure coverage. The
[transition contract](control-flow-transitions.md) defines the resulting behavior.

## Final checks

| Command / scenario | Result |
| --- | --- |
| `cargo +stable test -p fux --locked --lib` | 191 passed |
| `cargo +stable test -p fux --locked --lib client::` | 107 passed, 84 filtered |
| `cargo +stable clippy -p fux --locked --all-targets -- -D warnings` | Passed |
| `cargo +stable test --manifest-path tools/xtask/Cargo.toml --features betamax --target-dir target/rust-harness --locked` | 31 library and 36 binary tests passed |
| `cargo +stable clippy --manifest-path tools/xtask/Cargo.toml --features betamax --target-dir target/rust-harness --locked --all-targets -- -D warnings` | Passed |
| Root and standalone harness `cargo +stable fmt … -- --check`; `git diff --check` | Passed |
| Combined `local_cli` and `automation_integration`, no exclusions | Local 18/18; automation 19/23 |
| Supplemental `viewer-mouse-app` after adding copy/paste assertions | Passed |
| Betamax report/replay | 373 final integration frames and 39 supplemental frames passed |

The complete integration invocation was:

```sh
export PATH="/tmp/fux-betamax-toolchain/lib/python3.14/site-packages/ziglang:$PATH"
export FUX_BETAMAX_FONT='Noto Sans Mono CJK SC'
FUX_BETAMAX_DIR="$PWD/target/betamax-ux-final-v4" \
ZOR_BIN="$PWD/target/debug/zor" FUX_REQUIRE_ZOR_BIN=1 \
FUX_SCENARIO_DEADLINE_SCALE=3 \
cargo +stable test -p fux --locked --no-fail-fast \
  --test local_cli --test automation_integration -- --test-threads=1 --nocapture
```

The full run used the final production code. Subsequent changes added three
controller tests and extra mouse-app assertions/configuration, not runtime changes.
Those additions received the targeted PTY run, library tests and Clippy above.
The standalone harness uses the same Zig PATH and CJK font environment.

The four failures are `zor_group_scheduler`, `zor_groups`, `zor_recovery` and
`zor_service`: the previously documented alternate/proxy socket routing fixtures
against the workspace-manager migration. They remain unresolved; the suite is not
green. `zor_headless` passed this run. Its earlier intermittent pin-release failure
is not claimed fixed merely because this run passed. This UX task does not migrate
those unrelated fixtures. Full failure output is retained, without exclusions.

## Behavioral and visual evidence

The real PTY scenarios verify A → B → A offsets and content, viewer isolation,
unequal nested panes, application mouse routing/Shift override, Escape without a
popup or leaked byte, focused input, and resize/selection/tiny-layout restoration.
Controlled peers verify progress on B while A is withheld, fixed deadlines under
state traffic, stale reply rejection, buffer reply/state ordering, failure notices,
and canceled manager interactions without replay into a new target.

The supplemental reporting-app run additionally asserts exact application bytes
following q and successful copy, exact OSC52 payload, literal UTF-8 bracketed paste
containing the prefix, and discarded unfinished paste after Copy loses its buffer.
It verifies no accidental split and no bytes delivered to the other pane. Its first
two runs exposed fixture mistakes: disabled private clipboard writes, then use of
a helper counting only the unrelated COPY_TARGET payload. Correcting the fixture
made the exact payload assertion pass; no production behavior was weakened.

Reports are `target/betamax-ux-final-v4/index.html` and
`target/betamax-ux-copy-paste-v3/index.html`. All 24 final contact sheets (373 frames)
and all three supplemental sheets (39 frames) were inspected. They show simultaneous
histories, normal input after dismissal, selection, menus and field exits, delayed
cancellation, visible exit hints and restored borders/backgrounds. No additional
confirmed visual defect was found. Tiny widths necessarily truncate text; one-cell
views cannot display explanatory hints. Rendering checks accompany behavioral
assertions and do not replace them.

Logs and labeled contact sheets are retained under
[verification/control-flow-2026-09-12](verification/control-flow-2026-09-12/),
including `final-v4.log`, `final-lib.log`, `final-harness-tests.log`,
`final-v4-review/` and `copy-paste-v3-review/`.

## Separate source review

A separate review pass inspected the UX implementation against saved starting
client snapshots, including controller, copy, input, effects, read window, popup,
render, hints, screen, context and event-loop changes, request-ID assignment, and
related viewer/delayed-peer/manager/raw-app fixture changes. It examined exact input
ownership, attachment identity, deferred-input bounds and ordering, fixed deadlines,
finite-set round-robin progress, gesture tails, paste handoff and target loss.
The final test additions and fixture corrections were reread after their checks.
This was an explicit separate pass by the implementing agent; no independent
reviewer was used. Unrelated existing pane-engine/zor migration work is outside
this review's authorship and scope.

The review reproduced and fixed popup middle/right release ownership across mode
changes and clipped exit hints. It added direct target-loss assertions for tab,
layout, workspace creation/menu and vanished chooser targets. No further confirmed
in-scope finding remains. U6/U10 were risk/policy categories, not proof that every
sequence failed; their concrete cases now have the scoped evidence above. Shared
control-reply rejection is tested once as shared behavior, not misrepresented as
separate failure injection for every action. CloseWorkspace uses this control path;
workspace ordering/transfers have their own manager-failure coverage.

No commit, push or PR was made. This establishes the specified headless terminal
contract, not native OS mouse/keyboard validation, universal UX correctness, or
cross-product parity. The unrelated automation failures remain visible limitations.
