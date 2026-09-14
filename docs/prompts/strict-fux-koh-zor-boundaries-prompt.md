# Enforce strict ownership boundaries between fux, koh and zor

Implement the boundary improvements described in `docs/boundary-audit-2026-09-13.md`. Read the current code and validate each finding before changing it; the audit is a starting point, not proof that every implementation detail is still current.

Execute the work, verify the resulting behavior, and write a concrete implementation report. Backward compatibility and breaking semver are not concerns. Preserve useful standalone features through explicit feature boundaries rather than deleting them merely to simplify a dependency check.

## Scope and working rules

- Fux and zor live in this workspace. Koh is an independent repository pinned by `tools/xtask/companions.json`.
- Inspect applicable `AGENTS.md` files, working trees, current revisions and existing verification commands first. Preserve unrelated work.
- Implement koh changes in a separate development checkout. Keep `references/koh` as a clean, reproducible integration checkout; do not treat local edits there as a finished dependency update.
- Prepare and verify all local changes before any publication step. This prompt does not independently authorize commits, pushes, PR creation, branch-protection changes or releases. Follow any explicit publication authorization in the active conversation. Without it, deliver the verified koh changes and integration instructions, and identify the remaining publication/pin step accurately.
- A committed companion pin must name an exact commit available from its declared repository. Never publish a pin to a local-only commit or silently change the dependency verifier to accept dirty checkouts. Use explicit development binary paths for prepublication composition tests.
- Keep this task focused on ownership, contracts, feature isolation and their verification. Do not add a multi-machine dashboard, new provider integrations, a scheduler rewrite or a new persistence system.

## Ownership contract

| Owner | Must own | Must not own in the composed system |
|---|---|---|
| fux | Local pane PTYs/process groups, terminal emulation/history, layouts, viewers, pane identity/routing, bounded generic events, input-write receipts and final process evidence | Remote identity/network transport, provider detection, prompts, task outcomes, agent restart policy, Git worktrees |
| koh | Remote identity and authorization, encrypted connectivity, service forwarding, bounded reconnect/resume state | Fux pane/layout models, interpreting application messages, task state, prompt retry, agent restart or application resurrection |
| zor | Agent/provider interpretation, tasks, checks, artifacts, worktrees, supervision and application recovery policy | Fux pane PTY ownership, an alternative authoritative pane model, implementing remote encryption/reconnect |
| local-ipc | Generic private local sockets, peer credentials, bounded framing and deadline mechanics | Fux wire schemas, task/provider semantics or remote network policy |

Fux receipts mean bytes were delivered according to the documented terminal contract. They do not prove that an agent accepted a prompt or completed a task. Final process evidence does not itself prove task success. Koh connection restoration does not authorize resubmitting application operations.

Zor may own provider adapter/check subprocesses and its own dashboard terminal. Koh may retain its standalone remote shell. These are distinct resources, not permission to take over a fux pane's lifecycle.

## 1. Complete the typed zor-to-fux client

Inventory every fux operation consumed by zor, including manager discovery/create/resolve, info, list, capture, events/subscriptions, split, focus, kill, pane location, pin release, final records and input reservation/submission/status. Inspect `crates/zor/src/fux.rs`, `fux/{manager,input}.rs`, `watch.rs`, `observe.rs`, `run.rs` and every task consumer; do not stop at those named files.

Move fux wire construction, decoding, envelope validation and socket naming into the client boundary. Expose typed operations and domain-neutral response types. Make raw JSON exchange private to this boundary. Policy code must not construct fux requests, index fux reply JSON, open fux sockets directly or reconstruct endpoint paths independently.

Validate the fields each operation actually promises: operation/reply kind, correlation ID where present, instance identity, payload shape and required values. Do not invent fields the protocol does not carry. Keep task-specific authority checks in task policy, including exact process identity, origin and whether a mutation is justified.

Represent transport failures, malformed responses, explicit remote failures, pending results and stale identity distinctly. A malformed response must never become success, pending or permission to retry a mutation. Preserve explicit unknown outcomes after lost replies.

Do not prohibit JSON used legitimately for zor's own service protocol, adapter messages, durable data or user output. Restrict the fux wire boundary specifically. Avoid wrappers that merely hide an arbitrary `Value` behind a typed name.

Prefer finishing the existing client modules. Introduce a shared wire-only crate only if the implementation demonstrates a concrete benefit; it must not link application runtimes together or become a shared policy layer. Preserve independently exercised producer/consumer contracts.

## 2. Consolidate RPC framing and absolute deadlines

Remove `run.rs`'s separate exchange implementation and locate any other duplicated fux transport loops. Reuse authenticated, bounded local transport with one absolute deadline covering connect, negotiation, write and reply completion. Streaming subscriptions need a documented distinction between allowed idle time and the deadline for a partially received frame.

First reproduce the suspected slow-partial-response timeout gap with a controlled peer, or document why the current code is already safe. Test partial prefaces/replies, repeated short reads, partial writes, EOF with buffered bytes, oversize frames and malformed envelopes. Ensure progress arriving just before individual socket timeouts cannot indefinitely extend the total operation deadline.

Keep `zor run`'s workflow timeout, exact created-resource identity and cleanup policy in zor. Moving transport code must not broaden which workspaces or panes it can kill. Do not increase deadlines to obtain passing tests.

## 3. Make zor's PTY wrapper explicitly optional

Make the default zor build provide the agent/task CLI without the standalone wrapper. Preserve wrapper functionality behind an explicit opt-in `wrap` feature, unless a small separate executable is clearly simpler.

Remove `portable-pty` from the CLI-only dependency closure. Replace `platform::winsize` returning `portable_pty::PtySize` with an ordinary terminal-size type used by the dashboard. Convert into the PTY library's type only inside the wrapper. Gate terminal emulation and wrapper-only dependencies accordingly.

Verify protocol-only, default CLI, explicit CLI-only and wrapper-enabled builds. Update CLI help, installation examples, packaging, feature tests and README statements to match actual defaults. Do not remove legitimate dashboard terminal handling or provider subprocesses.

## 4. Introduce a genuinely isolated koh gateway build

Provide a gateway-only feature set or package whose compiled dependency closure excludes shell PTY allocation, terminal emulation, predictive rendering and terminal backends. Follow dependencies through `embed`, `server` and `client`: a feature flag around the CLI alone is insufficient if gateway configuration still pulls in those modules.

Keep identity, authorization, encrypted connectivity and generic reconnect/resume behavior in the transport layer. Preserve the standalone shell through an explicit feature composition and verify it still builds and works. Gateway production code must not parse fux/zor payloads or import their domain types; real-application fixtures belong in tests.

Document opaque gateway forwarding as the current fux integration. Remove stale claims that current fux embeds koh's shell/state model. Do not claim that the opaque gateway supplies predictive terminal rendering unless that is actually implemented and verified.

## 5. Centralize endpoint identity without expanding product scope

Introduce concrete endpoint/reference types where they remove existing scattered path construction and identity ambiguity. Distinguish service location, server instance, movable workspace route and immutable process identity. Reuse the existing exact-process routing safeguards.

Document the future remote supervision contract: local zor talks to local fux; a remote controller uses the remote zor service through an independently authorized koh connection. Koh owns transport identity and connectivity; zor owns application identity, reconciliation and retry decisions.

Do not implement speculative unused remote APIs or full multi-machine orchestration. Exercise existing separate attachment/control forwarding in integration tests. Attachment permission grants an interactive terminal, not a security sandbox or observation-only access.

## 6. Enforce boundaries with structural and behavioral checks

Strengthen existing `agent_boundary`, `structure`, fixture and protocol-consumer tests rather than replacing them with weaker snapshots.

- Check resolved dependencies across supported feature configurations and relevant platform targets, including renamed dependencies. Assert the minimal zor/gateway dependency closures explicitly.
- Add parsed import/module rules where appropriate: fux transport/provider exclusions, zor policy consuming only its typed fux client, and koh gateway exclusion of shell/rendering modules.
- Keep reviewed process-spawn ownership rules resource-specific. Do not ban all subprocess creation in zor.
- Require executable contract coverage for consumed operations, including wrong identity, malformed envelopes and lost-response behavior. A source mentioning a wire name is not sufficient evidence of correct use.
- Update declaration/consumer inventories only after explaining each changed item's owner, consumer and need. Do not simply regenerate them to silence failures.

## 7. Make composition verification part of ordinary CI

Add a distinct composition job to normal push/PR verification for changes affecting these boundaries. Keep standalone fux build/package verification independent of external repositories. Update the structural CI rules to permit the explicit composition job.

Use the exact clean koh pin and the exact fux/zor binaries built for that run. Set required-binary flags so missing prerequisites fail rather than silently skip. If using path-based change detection, cover manifests, lockfiles, shared IPC, protocol producers/consumers, feature configuration, companion pins and verification tooling; otherwise run the job unconditionally. Required status checks must not become permanently pending because an entire workflow was filtered out.

Run both gateway-only composition tests and the relevant standalone koh shell checks. Keep filesystem path overrides for development separate from the published reproducible pin. Do not claim branch protection is configured merely because a workflow job exists.

## Behavioral acceptance

Preserve or add focused real-process coverage proving:

1. Stopping zor preserves fux pane processes and local attachments.
2. Stopping or disconnecting koh preserves local fux and zor service state.
3. Unauthorized peers cannot cause the gateway to open the local service; attachment authorization does not automatically authorize the separately configured zor-control endpoint.
4. Gateway forwarding preserves arbitrary payload bytes, and transient reconnect does not duplicate delivered input within its documented resume guarantees.
5. An expired transport session is reported as such; it does not silently create a pane or restart an agent.
6. Pane relocation changes routing without changing exact process/origin identity; stale targets cannot mutate replacement processes.
7. Lost create/input replies reconcile the existing operation instead of blindly issuing a second mutation.
8. Fux input delivery, native agent acceptance and verified task success remain separate states.
9. Agent OSC reports have no agent-semantic effect on fux, while ordinary terminal behavior remains correct.
10. RPC absolute deadlines hold under slow partial delivery, and malformed replies never become successful policy transitions.
11. Minimal feature builds actually exclude the disallowed dependency/module capabilities; standalone opt-in features remain usable.

## Verification and handoff

Start with targeted checks and then run affected repository formatting, strict linting, feature/build/package checks, boundary fixtures and real-process integration suites. Use the existing `verify-codebase` workflow where applicable; inspect its commands before relying on it. Run relevant headless terminal scenarios to catch dashboard/wrapper regressions. A new broad PNG campaign is unnecessary unless rendering behavior changes.

Review the complete implementation in a separate pass, validate findings against current code, fix confirmed in-scope issues and rerun affected checks. Do not weaken assertions or remove scenarios to get green results.

Write `docs/strict-boundary-implementation.md` with:

- Final ownership and feature/dependency matrix.
- Every migrated client operation and any justified remaining raw boundary access.
- Findings confirmed, rejected or deferred, with evidence.
- Exact verification commands, results, repository revisions and companion provenance.
- Changes to installation/default features and any user-visible behavior.
- Publication/pin/hosted-CI state and concrete remaining steps, if any.

Treat unrelated historical failures as documented limitations unless new evidence connects them to this change. Do not claim the old unexplained pressure timeout was fixed by these changes, and do not make diagnosing it a new completion requirement. New failures caused by this implementation must be investigated and resolved.

Completion means the implemented ownership rules are enforced by code, dependency configuration and passing relevant contracts. Report any blocked companion publication or unavailable external verification explicitly; do not describe a development override as a finished pinned integration.
