# Release readiness

fux builds, tests and packages from a clean checkout with no other program, source tree, key or
graphical environment. Nothing in the tree authorizes a commit, tag, release or registry
publication; those remain separate user decisions.

## Evidence

The gate is the command list in the README's "Verification" section plus the real koh and zor
integrations with explicit binary paths. What has been accepted, and where the dated evidence
lives in history, is indexed in [verification.md](verification.md).

Configured CI (`ci.yml`: Linux and macOS hosts, MSRV job, Android cross-compilation check,
package job, optional cross-repository job; `nightly.yml` with randomized cases;
`release-verify.yml`) describes what hosted runs execute. A configured job is not evidence
until it has run on the tree being released.

## Limits

- Runtime evidence covers macOS and targeted Linux ARM64; Android is a compilation cross-check.
- Terminal-emulator specific behaviour (OSC 52 handling, reserved mouse gestures) needs manual
  checks per emulator.
- Relay/NAT behaviour and mobile suspend/resume are koh's scope.
- The protocols carry no version numbers. An interactive `fux` offers to stop an incompatible
  older server after an explicit typed confirmation (terminating its panes), or shows how to run
  alongside it; it never stops one without that confirmation.
