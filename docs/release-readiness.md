# Release readiness

Fux is independently buildable and packageable. It does not require koh or zor registry releases,
sibling source trees, or integration patches. No publication is authorized by this refactor.

## Automated evidence

A disposable fux-only checkout passed root and fixture tests offline. The release verifier packaged
and installed fux and passed its eight real-binary scenarios against that installed binary. Default
CI has Linux/macOS host jobs, an MSRV job, and an Android cross-check job. Hosted CI has not been run
for this uncommitted change; configured jobs are not claimed as executed evidence.

Optional integration CI requires its explicit manual switch. Local tests have exercised real zor
sidecar reports and failure isolation, authenticated koh gateway access, and automatic reconnect
under forced loopback QUIC loss. See [the progress log](standalone-plan.md) for exact checks and
coverage limits. The completion audit links current verification records.

## Coverage limits

Native runtime checks for this refactor ran on macOS. Android cross-compilation is not runtime
coverage. Relay/NAT behavior, real Android suspend/resume, terminal-emulator OSC collision behavior,
and genuine-agent observation rules still need environment-specific evidence before release claims
covering those scenarios. Fux's standalone local operation does not depend on those integrations.

The requirement-by-requirement refactor audit is recorded in [standalone-audit.md](standalone-audit.md). Prior account/publication and
coupled-build notes are preserved in [the historical snapshot](release-readiness-before-standalone.md)
and must not be interpreted as current registry status or a prerequisite for packaging fux.
