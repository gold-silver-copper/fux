# Capability status — 2026-09-19 checkpoint

This replaces the mid-milestone-6 inventory. Source implementation and acceptance are separate.
Exact commands, failures and final checkpoint results are in [verification.md](verification.md).
The user requested committing/pushing the checkpoint and stopping, not claiming every original
milestone-8 deliverable complete. koh and future iroh-ssh integration are out of scope.

Evidence labels: **U** = unit/property/integration assertions exercised; **R** = real local
process or terminal exercised; **F** = provider fixture, not upstream-service compatibility.
A source implementation is not evidence of paid-provider, WAN, Linux or Windows acceptance.
The original 58 capability IDs are retained below; grouped IDs share the stated boundary.

## fux

| IDs | Capability | Implementation and evidence boundary |
|---|---|---|
| 1 | Persistent terminals, detach/reattach, independent viewers | Implemented; U/R. Viewer/controller death preserves the independently owned remote PTY. |
| 2–3 | Split/close/focus/resize, zoom, move/swap/order, root/workspace transfers | Implemented; layout/property/picking/viewer U. Real multi-viewer scenario covers drag, reparent, resize, zoom and focus; see final scenario result rather than inferring success from source. |
| 4 | Layout export/apply/archive/assets | Implemented; scene/template/asset U and historical runtime evidence. |
| 5–6 | Keyboard/mouse, picking, captured gestures, viewer-local zoom/scroll | Implemented; U/R. Input is pinned to the painted scene; stale geometry cannot retarget it. |
| 7 | Layout validation and structural invariants | Implemented; U/property coverage for bounded scenes, foreign references and stale generations. |
| 8–9 | BRP method allowlist, capabilities, private descriptors, discovered schemas | Implemented; U/R. Independent attachment credentials are not scoped BRP grants. |
| 10 | Retained events, watches, explicit gaps | Implemented; U/R. `fux/events.poll` establishes an authenticated finite cursor baseline. |
| 11 | Input reservation/receipts, exact target identity | Implemented; U/R. Accepted-but-lost input replies reconcile without a second submission. |
| 12 | Final records and explicit forgotten evidence | Implemented; receipts/lifecycle U and runtime task completion paths. |
| 13 | Coherent bounded capture/history | Implemented; U/R. Captured physical rows can soft-wrap text; capture is not provider-native completion evidence. |
| 14 | Exact attachment and multiple viewports | Implemented; U/R. Pane/PID mismatch is refused, not retargeted. |
| 15 | Resource ceilings and bounded transport | Implemented; U/R pressure, slow-viewer and shutdown scenarios. Slot exhaustion can temporarily deny service; no hostile-local-process availability guarantee. |
| 16 | Config/theme/bindings/clipboard | Implemented; U and terminal binding R. Plugin bindings use `plugin:NAME/ACTION`. |
| 17 | Bell/diagnostics/server information | Diagnostics implemented and exercised. Audible bell/device playback not validated. |
| 18 | Session shape/cwd/history restoration, explicit restore/skip | Implemented; session U and historical real-process restore evidence. This restores a recipe/history, not the old OS process. |
| 19 | Exclusive controller leases/read-only attachment observers | Unsupported parity work; not supplied by ordinary BRP capability scope. |
| 20 | Agent transcript retrieval by controlled alternate-screen paging | Partial primitives only: capture/history exist; no complete idle-gated paging/viewport-restoration workflow. |
| 21 | Kitty graphics/image API and IME richness | Unsupported parity work. |
| 22 | Generic plugin/dashboard surfaces | Implemented; U/R, provider/revision/viewer guards, sanitized text, stable provider-node mapping and viewer-local scrolling. |

## zor

| IDs | Capability | Implementation and evidence boundary |
|---|---|---|
| 23 | Task/attempt/prompt/operation model and relationships | Implemented; U, invariant checks and runtime task workflows. |
| 24 | Journal, bounded extraction, atomic persistence/archive | Implemented; U. Failed persistence prevents effect dispatch. Machine configuration is deliberately not duplicated in this journal. |
| 25 | Durable prompt/input delivery and identity reconciliation | Implemented; lifecycle/receipt U and lost-reply R. No blind input replay. |
| 26 | Native response correlation | Implemented correlation/claim parsing; U/F. Live upstream acceptance remains separate. |
| 27–30 | Verified seals, committed-tree sources, bounded checks, artifact capture | Implemented; U with real temporary Git repositories and subprocesses. This is not representative-project or paid-provider acceptance. |
| 31–32 | Dependency groups and owned worktrees | Implemented; U with real Git. Git admission refusal now produces a scheduled completion even when inbound transport is full. |
| 33 | Startup reconciliation, uncertainty and lost evidence | Implemented; U/R. In-flight effects are not automatically repeated. |
| 34 | Task forgetting/compaction and general uncertain-effect administration | Partial: archive and typed reconciliation exist; do not claim a complete task-forget/general compaction API. |
| 35 | Owned helper/check/provider cleanup | Implemented; U/R, bounded cancellation and guardians. Signal authority ends before reaping; no OS sandbox or deliberate group-escape guarantee. Git has in-process cleanup, not the helper guardian's hard-parent-death guarantee. |
| 36 | Legacy standalone adapter capabilities API | Not established as a standalone parity API; schema discovery and typed provider configuration are not equivalent evidence. |
| 37 | Codex native adapter | JSON-RPC initialization/thread/turn and correlated claims implemented; U/F. Full interrupt/history/recreation parity and live upstream compatibility are not claimed. |
| 38 | OpenCode armed-input ancestry and guarded resume | Implemented sidecar contract and explicit native resume; U/F/R fixture. Real upstream bridge/service compatibility remains unvalidated. |
| 39 | Claude generic launch/passive rules | Generic process and observation paths exist. Native resume is explicitly refused; no full native Claude adapter claim. |
| 40 | Passive discovery/rules and observation states | Implemented; asset/provider U and machine/dashboard R. |
| 41–43 | Authenticated BRP, secret-free projections, retained event watches | Implemented; U/R, typed fixtures and generated schemas. Arbitrary World mutation stays disabled. |
| 44 | Machine catalog and separate control/attachment bindings | Implemented; U/R, private atomic catalog, stable IDs and last-valid reload. CLI resolves active catalog authority through ADMIN-only endpoint access. |
| 45 | Independent machine supervision/freshness | Implemented; U/R. Wrong token, expired incarnation, offline endpoint and healthy peers remain independent. |
| 46 | Hosted dashboard, attention/selection, exact handoff/return | Implemented; U/R keyboard, mouse, local scrolling, stale refusal and exact selected task attachment. Closing dashboard does not cancel the task. |
| 47 | Durable remote operation/resume intents | Implemented; U/R before-forward and accepted-reply-lost crashes, restart and read-only reconciliation without replay. |
| 48 | Exact destructive-call ownership guards | Implemented; U/R using machine/controller incarnation, attempt, fux instance/workspace/pane/PID. |
| 49 | Legacy service lane layout/deadlines | Replaced by bounded independent workers/adapters and explicit deadlines; the old fixed lane counts are not preserved as an API or separately claimed. |
| 50 | Plugin host/actions/hooks/panes/links/logs | Implemented; U/R. Scoped descriptors omit universal attachment secrets; disable revokes grants and cleans owned children. Hook claims precede dispatch; crashes can omit work, never imply exactly-once side effects. |
| 51 | koh composition / future iroh-ssh transport | Out of scope, not an acceptance blocker. |
| 52 | Multi-stack and terminal scenario harness | Implemented, thirteen scenarios. Final recorded run is authoritative; earlier failing probes are retained as diagnostic evidence. |

## Delivery and external acceptance

| ID | Capability | Status |
|---|---|---|
| 53 | Representative-repository source-bound verification | Temporary-repository checks/seals exercised; realistic representative-project acceptance remains. |
| 54 | Platforms/distribution | This execution validates macOS/Apple Silicon. Linux CI is configured, not observed here. Native Windows and Android acceptance remain unsupported/unvalidated. |
| 55 | Design/protocol/ownership/security documentation | Updated source-grounded documents and typed protocol fixtures. Verification/handoff distinguish observed runs from recipes. |
| 56 | Composed package and clean install/update | Source installation/update instructions and CI paths added. A successful fresh install/package smoke must be recorded before claiming delivery acceptance. No release published. |
| 57 | Comparative release performance | Measurement harness and old-main/candidate release builds exist. No valid completed comparison yet; no performance improvement claim. |
| 58 | Long-running retention/archival sustainability | Bounded retention and local pressure tests exist. Multi-day soak, thousand-cycle and broad archival administration evidence remain. |

No aggregate “parity percentage” is reported: these rows deliberately separate implemented
mechanisms, actual execution, fixtures and unavailable external acceptance.
