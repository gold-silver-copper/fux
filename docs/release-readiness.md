# Release readiness

fux 0.3.0 builds, tests and packages from a clean checkout with no other program, source tree,
key or graphical environment. Nothing in the rewrite authorizes a commit, tag, release or
registry publication; those remain separate user decisions.

## Evidence

Local verification on macOS is recorded command by command in
[ecs-acceptance.md](ecs-acceptance.md): formatting, strict Clippy, root tests, rustdoc, MSRV 1.95
compilation, the fixture-child suite, the packaged-binary verifier, the reconstructed koh and zor
integrations and the performance measurements against the 0.2.1 baseline.

Configured CI (`ci.yml`: Linux and macOS hosts, MSRV job, Android cross-compilation check,
package job, optional cross-repository job; `nightly.yml` with 2048 randomized cases;
`release-verify.yml`) describes what hosted runs would execute. No hosted run of this tree was
requested, so configured jobs are not executed evidence.

## Limits

- Runtime evidence exists for macOS only. Linux is compiled and tested only when CI runs; Android
  is a compilation cross-check, not runtime coverage.
- Terminal-emulator specific behaviour (OSC 52 handling, reserved mouse gestures) needs manual
  checks per emulator.
- Relay/NAT behaviour and mobile suspend/resume are koh's scope and were not exercised here.
- Attachment protocol v3 and control protocol `FUXCTL2` are incompatible with 0.2.x servers; users
  must save work and stop an old server with its own binary. fux never does it for them.

Earlier readiness records ([release-readiness-before-standalone.md](release-readiness-before-standalone.md),
[standalone-audit.md](standalone-audit.md), [contextual-help-acceptance.md](contextual-help-acceptance.md))
are historical and describe architectures that no longer exist.
