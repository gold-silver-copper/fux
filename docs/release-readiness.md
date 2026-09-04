# Release readiness

This file distinguishes automated evidence from evidence that still needs an operator. A skipped or
unavailable check is not green.

## Automated gates

CI runs formatting, clippy, tests and documentation on stable Rust for Linux and macOS, plus an
MSRV job on Linux at Rust 1.91. The Android job compile-checks `aarch64-linux-android`; it does not
execute fux. The cross-repository job builds zor explicitly and exports `ZOR_BIN` without guessing
a target path. The integration suite uses that exact binary with a captured rule, observes
zor-generated OSC 7877 in fux state and control events, sends input through both PTYs, and verifies
that the detachable workspace retains the completed child's screen. This local seam is not claimed
as a real network outage/reconnect.

Koh 0.12.1 and zor 0.1.2 are published. Release preparation reruns both the full package and
publish-dry-run gates and inspects the resulting archive. No workflow publishes automatically.

## Human evidence still required

- Genuine Claude Code captures for idle, working, permission/plan/select blockers, transcript and
  model-picker skip states, plus the typed-prompt guard.
- OSC 7877 collision/passthrough checks on Terminal.app, iTerm2, kitty, alacritty, wezterm, Termux
  and tmux.
- Provenance and schema confirmation for OSC 21337.
- crates.io ownership and ordered zor-before-fux publication (completed with zor 0.1.2).
- Real Android runtime attach, suspend/resume, resize and detach testing.
- A real remote-relay session, including authorization rejection and reconnect.

## Local-environment caveats

This workspace stages zor as a nested gitignored repository because the execution sandbox cannot
write the intended sibling path. The manifest therefore uses the staging path. Release preparation
must restore the audited sibling dependency layout before packaging.
