# Execution trace: multi-machine-navigation-and-supervision-prompt.md

This maps every requirement of the acceptance-contract prompt to the code that implements it
and the automated verification that exercises it. "CI" = the `Pinned koh composition` job runs
it against the clean published koh pin with no skip. "CP-N" = retained checkpoint N evidence.

## Sections 1-6 (ownership, catalog, client, dashboard, attachment, reconnect)

| Prompt section | Implementation | Verification |
|---|---|---|
| 1 Preserve resource ownership | `machines/connection.rs` (koh helper only), fux `--target-*`/`--report-exit`, koh opaque gateway | `verify-boundaries` (15 PASS), structure ownership/import tests, `zor-multi-machine` helper accounting + remote survival (CI) |
| 2 Saved machines and routing | `machines/catalog.rs`, `zor machine add/list/inspect/rename/remove/control/bind` | `zor-multi-machine` profile lifecycle, duplicate/Local-alias refusal, unknown-selector refusal (CI); CP-1/3/7/9 |
| 3 Real remote zor client boundary | `service/client.rs`, `supervision-v1`, `task-read/supervise/attachment/resume*-v1`, capability negotiation | zor client contract tests (schema/incarnation/deadline/malformed); `zor-remote-resume` (CI); CP-4/5/6/22 |
| 4 Usable multi-machine supervision | `dashboard/multi.rs`, `machines/supervision.rs`, `dashboard/attention.rs` | `zor-multi-machine` aggregate + independent hosts + selection isolation (CI); reload/notifications CP-9/10/11 + fresh runs below |
| 5 Attach to selected pane and return | `dashboard/handoff.rs`, fux exact attachment primitive | `zor-multi-machine` exact attach/input/detach/return/missing-binding/killed-viewer/cancel (CI); CP-5/6/8 |
| 6 Reconcile disconnection and restart | transport classification in `handoff.rs`, incarnation guards, `machines/intents.rs` | restart controller/zor/fux fresh runs below; transport loss/expiry CP-12; lost-reply CP-23/24 |

## Section 7 demonstrations (1-12)

| # | Demonstration | Evidence |
|---|---|---|
| 1 | Add/save/reload/list/select; edit/remove without affecting owners | `zor-multi-machine` (CI) + `--reload-catalog` fresh run |
| 2 | Same-named tasks on both remotes stay distinct | `zor-multi-machine` distinct fux incarnations + isolated input (CI) |
| 3 | Slow/offline host does not block navigation/Local/healthy host | `zor-multi-machine` unreachable `offline` + unauthorized `intruder` (CI) |
| 4 | Unauthorized control and attachment fail independently before local access | `zor-multi-machine` intruder control refusal + unauthorized attachment binding (CI) |
| 5 | Attach exact pane, input, detach, return, terminal restored | `zor-multi-machine` (CI) |
| 6 | Relocation/stale cannot send input or stop a replacement; missing binding no fallback | `zor-multi-machine` missing binding (CI) + `--restart-fux` fresh run |
| 7 | Transport loss during attachment and around a mutation reply | CP-12 (`--transport-faults`, dev koh) + `zor-remote-resume-lost-reply` CP-23/24 |
| 8 | Real session expiry, then explicit reconnect without a new pane | CP-12 (`--transport-faults`, dev koh) |
| 9 | Restart controller, remote zor and remote fux separately | `--restart-controller`, `--restart-zor`, `--restart-fux` fresh runs |
| 10 | Closing dashboard preserves remote services/tasks/panes; cleans owned helpers | `zor-multi-machine` remote survival + helper cleanup (CI) |
| 11 | Malformed/oversized/partial replies obey deadlines, no successful action | zor client contract tests (CP-1/4/5) |
| 12 | Dependency/import, local task, wrapper and standalone shell contracts pass | `structure` tests + `verify-boundaries` + standalone package job |

## Section 8 (documentation and completion)

- `docs/multi-machine-supervision.md`: commands, walkthrough, keybindings, config schema,
  identity/freshness/reconnect semantics, supported operations and limitations.
- `docs/multi-machine-manual-acceptance.md`: two-host checklist with cleanup.
- `docs/multi-machine-supervision-implementation.md`: design decisions, per-checkpoint
  verification, provenance, and the acceptance matrix.

## Externally blocked / not claimed

- Distinct koh transport states require the optional development status extension
  (`checkpoint-27/koh-development.patch`, applies cleanly to the published pin); publishing it
  needs authorization. Items 7 and 8 use that build (CP-12) and remain retained dev-koh evidence.
- `zor-remote-resume-lost-reply` is 3/4 on the published pin (reply-gate timing), retained as
  dev-koh evidence rather than an always-green CI gate.
- Physical-host, WAN, relay, NAT, mobile, SSH bootstrap and Herdr parity are not exercised.

## Fresh acceptance runs (checkpoint 27, published koh pin)

Re-run this session, sequentially (the Python fixtures share a `/tmp/zor-gw-*` glob and must
not run concurrently), each Local + two isolated remote stacks through real koh:

| Demonstration | Fixture flag | Result | Log |
|---|---|---|---|
| 7.1 reload/list/select, independent removal | `--reload-catalog` | passed | `acceptance/reload-catalog.log` |
| 7.9 controller restart (distinct from remote restarts) | `--restart-controller` | passed | `acceptance/restart-controller.log` |
| 7.9 remote zor restart, new incarnation, stale-selection refusal | `--restart-zor` | passed | `acceptance/restart-zor.log` |
| 6/7.9 remote fux replacement, refused old attach, zero replacement input | `--restart-fux` | passed | `acceptance/restart-fux.log` |

Binaries: fux `a6f293a4…`, zor `a41f25d8…`, published koh `88698a63…` (see
`source-provenance.json`). Items 7 and 8 (transport loss/expiry) remain retained development-koh
evidence in checkpoint 12, since distinct transport states need the unpublished status extension.
