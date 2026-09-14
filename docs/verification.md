# Verification index

What was accepted, when, and where the evidence lives. The commands that make up the gate are
in the README's "Verification" section; the design they verify is in [design.md](design.md).

## Where the evidence lives

Raw gate logs, headless captures, checkpoint dashboards and dated acceptance reports were
tracked under `docs/verification/` and as `docs/*-YYYY-MM-DD.md` reports until commit
`483d8ca` (2026-09-13). They are unchanged in git history at that commit and are no longer part
of the tree; `docs/verification/` is now gitignored and the xtask harness writes fresh
artifacts there on every run. Provenance sources for the archived Python capture scripts stay
under `tools/archive/` because the xtask provenance checks hash them.

## Accepted milestones

| Date | Milestone | Evidence at `483d8ca` |
| --- | --- | --- |
| 2026-09-09 | ECS rewrite (0.3.0): typed World, ordered schedule, protocol v3/`FUXCTL2` | `docs/ecs-acceptance.md` |
| 2026-09-09 | Native integration and performance pass (PR #4): retained grids, changed-row frames, frame pacing | `docs/native-integration.md`, `docs/performance-and-refactor.md` |
| 2026-09-11 | Strict fux/koh/zor ownership boundaries (PR #10 onward) | `docs/strict-boundary-implementation.md`, `docs/boundary-audit-2026-09-13.md` |
| 2026-09-12 | Pane and layout controls, manual visual acceptance, performance comparison | `docs/pane-layout-implementation.md`, `docs/pane-layout-manual-acceptance.md`, `docs/pane-layout-performance-2026-09-12.md` |
| 2026-09-12 | Control-flow UX: input ownership, modal transitions, one-Escape dismissal | `docs/control-flow-ux-implementation.md`, `docs/control-flow-ux-audit-2026-09-12.md`, `docs/control-flow-ux-final-verification.md` |
| 2026-09-12 | Betamax headless terminal verification | `docs/betamax-verification-2026-09-12.md` |
| 2026-09-13 | Multi-machine supervision on the published koh pin | `docs/multi-machine-supervision-implementation.md`, `docs/multi-machine-manual-acceptance.md` |
| 2026-09-13 | Codebase improvement and lint baseline | `docs/codebase-improvement-report.md`, [lint-baseline.md](lint-baseline.md) |

## Living documents

These stay in the tree and describe current contracts and mechanisms, not dated evidence:

- [design.md](design.md): architecture, entity model, step phases, ordering guarantees.
- [local-control-protocol.md](local-control-protocol.md) and
  [local-attachment-protocol.md](local-attachment-protocol.md): wire contracts.
- [security.md](security.md): the local trust model.
- [service-ownership-contract.md](service-ownership-contract.md) and
  [multi-machine-supervision.md](multi-machine-supervision.md): what fux, koh and zor each own.
- [control-flow-transitions.md](control-flow-transitions.md) and
  [pane-layout-controls.md](pane-layout-controls.md): viewer behaviour specifications.
- [zor-lifecycle-transitions.md](zor-lifecycle-transitions.md): the zor durable lifecycle contract.
- [betamax-harness.md](betamax-harness.md), [controller-trace-testing.md](controller-trace-testing.md),
  [codebase-verification.md](codebase-verification.md) and
  [diagnostics-and-failure-artifacts.md](diagnostics-and-failure-artifacts.md): how to run and
  read the harnesses.
- [lint-baseline.md](lint-baseline.md) and [release-readiness.md](release-readiness.md).

## Known limits

Runtime evidence covers macOS and targeted Linux ARM64. A complete final Linux gate and Android
runtime acceptance are not established. Emulator-specific clipboard and mouse behaviour and koh
relay/NAT scenarios remain manual. Live remote and paid-provider acceptance for zor remain
deferred.
