# Implement integrated multi-machine navigation and agent supervision

Execute this work in the fux/zor workspace and its pinned koh companion. Deliver a usable
end-to-end workflow, not just a design or a collection of proxy-socket examples:

**Select a saved machine → inspect its agents/tasks → attach to the selected agent's pane →
return to supervision → disconnect/reconnect without duplicate input or accidental restart.**

Include Local and multiple remote machines in one interface. Backward compatibility and
breaking semver are not concerns. Preserve the strict ownership boundaries already enforced
by the codebase. This is one concrete product milestone toward integrated remote parity;
it is not a claim of surpassing Herdr in every capability.

## Start from current evidence

This workspace may already contain a partial implementation. Read
`docs/multi-machine-supervision-implementation.md` and inspect the actual staged, unstaged
and untracked changes before choosing where to resume. Audit and finish existing work rather
than replacing it or assuming its checkpoint notes describe the latest code. In particular,
verify the catalog, typed read client, owned gateway helper and remote CLI routing separately;
their presence does not prove integrated dashboard, attachment or mutation support.

The working tree may also contain newer, unverified dashboard handoff work in
`crates/zor/src/dashboard/handoff.rs`, `dashboard/multi.rs`, machine supervision,
CLI routing and foreground process handling. Inspect it alongside the checkpoint ledger.
First finish and verify authoritative task/attempt selection, asynchronous inspection/actions,
and the dashboard → exact viewer → same dashboard selection flow. Check cancellation races,
foreground process-group ownership, terminal restoration after viewer failure, and buffered
input across handoff. An existing source file or an earlier checkpoint's passing tests is
not evidence that these newer changes compile or work. Then complete the remaining requirements
below; this initial focus does not reduce the milestone's scope.

Before implementation, build a short acceptance matrix mapping each requirement below to
current code, retained evidence and the next missing check. Revalidate newer observed-agent
attachment work separately from task attachment: an unadopted agent must be inspectable and
attachable without creating a task record, while task-only actions remain unavailable.
Fresh service observations must authorize the exact process; caller-supplied filesystem
paths must not grant access. A fixture with forced provider classification proves routing,
not real provider recognition or breadth.

Prioritize the remaining end-to-end gaps once existing handoff work is verified: catalog
reload, machine-aware notifications, actual transport loss and expiry, lost mutation replies,
independent controller/service/process restarts, narrow-terminal rendering, the two-host
manual walkthrough and ordinary CI integration. Preserve verified behavior while completing
these gaps; do not stop at a new checkpoint or count documentation as implementation.

Locate any companion development checkout and in-flight verification before starting duplicate
jobs. A temporary koh checkout is development evidence only: the clean published companion
pin remains the integration baseline until publication is explicitly authorized. Preserve
useful local companion changes in a reviewable patch with exact base revision if publication
is outside scope. Clearly distinguish implemented, verified, pending and externally blocked
requirements in the ledger.

Read applicable `AGENTS.md` files and inspect working trees, revisions and current CI before
editing. Preserve unrelated work. Read:

- `docs/service-ownership-contract.md`
- `docs/strict-boundary-implementation.md` and its verification/publication evidence
- `docs/capability-audit-2026-09-12.md`, treating its dated findings as hypotheses to recheck
- `crates/zor/REMOTE.md`, README, CLI, service/service_tasks, dashboard, watch and task routing
- `crates/zor/src/fux/`, fux's CLI/attachment/viewer protocols, and `local-ipc`
- `tools/xtask/companions.json`, dependency/boundary checks and composition CI
- The pinned koh gateway CLI, authorization, reconnect/session behavior and real-service tests

Some older remote documentation still describes netmon EPERM as blocking local verification;
later retained tests passed. Reconcile contradictory documentation with actual revisions and
results. Do not repeat stale limitations or infer WAN reliability from loopback tests.

Record the starting ownership/API inventory and baseline test/CI state. Implement using actual
supported contracts. Do not invent command flags, assume a local directory override is a full
remote-client abstraction, or reuse the old audit as proof that a feature is still missing.

## 1. Preserve resource ownership

| Owner | Responsibility |
|---|---|
| fux | Local pane processes/PTYs, terminal emulation/history, layouts, viewers, generic attachment/control, exact pane identity and movable routes |
| koh | Remote identity, endpoint authorization, encrypted connections, opaque service forwarding, bounded transport reconnection/resume |
| zor | Saved supervision targets, agent/task interpretation, aggregate dashboard, task actions, application identity/reconciliation and recovery policy |
| local-ipc | Generic local socket authentication, bounded framing and deadlines |

The controller must not embed koh's shell stack or add network/PTY/emulator dependencies to
zor's default CLI closure. Prefer an explicitly owned koh gateway process or another small,
well-defined process interface. Koh may expose structured transport status; it must not learn
about tasks, prompts, fux panes or agent restart. Fux must not gain machine catalogs, remote
credentials, provider detection or task policy.

Zor may own its connection-helper processes and coordinate a fux viewer process. Killing a
controller or helper must not kill remote fux/zor services or pane processes. Clean up only
processes and proxy sockets whose ownership this controller can prove. Preserve existing
standalone fux, koh shell and optional zor wrapper behavior.

## 2. Add saved machines and explicit routing

Provide a small durable machine catalog with a stable machine identifier separate from its
editable display name. Local is a first-class target. Remote profiles reference authenticated
koh service identities, the client credential location and connection settings actually
supported by koh. Never store secret key material in dashboard snapshots, command output or
logs. Aliases and endpoint advertisements do not confer trust.

Control and attachment are distinct service bindings with independently explicit grants.
Adding an attachment endpoint must not silently enable zor control, and vice versa. Support
more than one remote machine and more than the default remote workspace. Handle duplicate
names, missing profiles, unavailable credentials, invalid bindings and profile edits clearly.
Persist configuration atomically in the owning user's configuration location.

Provide discoverable CLI operations for adding/listing/inspecting/removing machine profiles
and selecting a machine for supported task commands. Choose consistent final names after
inspecting the existing CLI; document the actual commands. A target-selection error must
never silently fall back to Local. Profile removal removes configuration and owned local
connections, not remote tasks or services.

Separate service location, authenticated transport identity, zor service incarnation,
task/attempt identity and fux process identity. Bind cached results and pending actions to
these identities. The same task name, workspace name, pane number or PID on another machine
must never match the selected target.

## 3. Build a real remote zor client boundary

Extract or complete a typed service-client interface usable by Local and remote proxy
endpoints. Reuse existing service operations for snapshots, task inspection/results and
supported task actions; extend the protocol only where the user flow requires it.

Keep request construction, framing, response validation and transport adaptation outside
dashboard/task policy. Validate correlation, protocol version, reply shape and service
incarnation. Use bounded requests/responses and absolute deadlines. Remote selection must
not invoke local service auto-start or local filesystem task access through an overloaded
`--directory` path. Task paths and provider state belong to the selected host's zor service.

Provide a documented capability response or equivalent explicit negotiation for operations
that differ between service versions/configurations. Unsupported actions must be clearly
unavailable, not simulated locally. Keep process-delivery receipts, native provider acceptance
and verified task success distinct throughout remote serialization and presentation.

## 4. Make multi-machine supervision usable

Extend the existing dashboard into a coherent Local/remote navigation flow. At minimum:

- Show saved machines and their connection/observation state; provide both machine-scoped
  inspection and an aggregate agent/task view with unambiguous machine attribution.
- Preserve selection by identity while updates arrive. Support switching between two machines
  with identically named tasks/workspaces without selection or action leakage.
- Display task/provider state, relevant attention/check failures and evidence freshness.
  Disconnected or stale cached observations must never look like fresh authoritative state.
- Keep healthy machines and Local responsive when another host is slow, offline, unauthorized
  or repeatedly reconnecting. Bound work, buffers, retries and concurrent requests per host.
- Expose inspect/result and the existing applicable supervision actions through consistent
  machine routing. This includes explicit stop/cancel or resume only where the selected
  service/task already supports that operation. Do not invent provider recovery support.
- Make pending, failed and unknown-outcome actions distinguishable. The user must know which
  machine and task an action targets and why an action is unavailable.

Avoid per-frame reconnects, serialized global polling or rebuilding selection from row
indices. Document freshness and retry behavior in terms that tests can assert. Notification
coalescing must include machine/service identity and must not repeatedly alert from stale
cached evidence. Preserve existing privacy controls on notification content.

## 5. Attach to the selected remote pane and return cleanly

Implement the complete handoff from a selected observed agent/task to the existing fux
viewer over its explicitly authorized koh attachment connection. Revalidate the task's
current exact process and workspace route before handoff. A cached pane number or a
machine's default workspace is not sufficient authority.

Koh currently forwards one configured local service socket. Explicitly solve how a selected
workspace maps to its authorized attachment service. Use the smallest generic attachment
routing/binding mechanism needed; do not turn koh into a fux-aware router or permit arbitrary
remote filesystem socket access. Persisted bindings may be explicit, but multiple workspace
routes must be supported and missing authorization/bindings must produce an actionable error.

Where fux needs a new primitive, make it generic: explicit proxy-socket attachment, exact
initial pane selection, viewer-scoped focus or a bounded handoff result. Do not put agent
semantics into it. Viewer selection must not steal another viewer's focus unless the user
explicitly requests a shared mutation. Stale/moved/replaced targets must be re-resolved or
rejected; never attach to the wrong process as a fallback.

Suspend dashboard terminal handling before starting the viewer. On detach, viewer failure,
connection loss or cancellation, restore terminal modes/cursor and return to the same
machine/task selection, refreshed against current identity. Do not accidentally send the
key that dismissed an overlay or detached the viewer into a pane. Keep Escape/back/detach
behavior consistent and document it. Human attachment remains an interactive terminal with
the remote account's privileges, not an observation-only sandbox or an exclusive controller
lease unless such a lease is separately implemented and verified.

## 6. Reconcile disconnection and restart without replay

Treat these as different events:

1. Transient koh connection loss while a retained byte stream can resume.
2. Expired koh session requiring an explicit fresh connection.
3. Local controller restart with saved profiles but no trustworthy live snapshot.
4. Remote zor restart with a new service incarnation.
5. Remote fux restart or pane exit/replacement, invalidating process authority.

Use koh's existing resume guarantees rather than a second byte-replay layer. Do not replay
keyboard input, task submissions or destructive actions merely because transport reconnected.
A lost mutation reply leaves an unknown outcome. Retain and reconcile the operation identity
using existing application evidence; if the protocol lacks sufficient evidence, add the
minimal bounded receipt/idempotency contract in zor or refuse automatic retry explicitly.
Do not advertise exactly-once behavior beyond the proven retention and failure model.

After an incarnation change, invalidate cached authority and refresh capabilities/state
before enabling actions. Old task records may remain visible as historical evidence but
must not authorize replacement panes. Returning online must not duplicate notifications.
An explicit application resume/restart is a separate user or documented zor-policy decision;
connectivity alone never resurrects a pane or agent.

## 7. Verification that exercises the product flow

Add focused tests to existing suites and a required real-process composition scenario. Use
the exact built fux/zor binaries and the exact clean published koh pin; missing required
binaries must fail. If koh changes are needed, develop in a separate checkout, verify there,
and follow publication authorization before advancing the pin. Never dirty `references/koh`
or pin a local-only commit to make CI pass.

The headless scenario must run Local plus at least two isolated remote service stacks with
real koh connections. Loopback stacks are useful deterministic evidence, not a substitute
for claiming actual multi-host/WAN coverage. Demonstrate:

1. Add/save/reload/list/select machine profiles; edit/remove without affecting remote owners.
2. Same-named tasks on both remotes remain distinct in dashboard, CLI and mutations.
3. A slow/offline host does not block navigation, Local or another healthy host.
4. Unauthorized control and attachment access fail independently before local service access.
5. Select a real task in a non-default workspace, attach to its exact pane, enter input,
   detach and return to the original dashboard selection with terminal settings restored.
6. Relocation and stale selection cannot send input or stop a replacement process; missing
   attachment bindings never fall back to an unrelated workspace.
7. Force actual transport loss during attachment and around a task mutation reply. Verify
   retained input counters/operation evidence prove no duplicate delivery or blind retry.
8. Exercise real session expiry, then reconnect explicitly without creating another pane.
9. Restart controller, remote zor and remote fux separately; validate the distinct identity,
   stale-state and recovery behavior rather than treating all restarts as reconnection.
10. Closing the dashboard or its helper preserves remote services, tasks and pane processes;
    owned local helper processes and private sockets are cleaned up within bounds.
11. Malformed/oversized/partial replies obey deadlines and cannot trigger successful actions.
12. Existing dependency/import, local task, wrapper and standalone shell contracts still pass.

Use Betamax to capture and inspect normal multi-machine navigation, duplicate task names,
offline/stale/unauthorized states, pending/unknown outcomes, narrow layouts, attachment handoff
and return. Test meaningful behavior as well as rendering; a PNG alone proves neither
routing correctness nor absence of duplicated input. Preserve failure artifacts and exact
binary/source provenance. Do not weaken assertions or inflate timeouts to obtain green tests.

Run targeted formatting/lint/tests first, then the affected feature/package checks and the
existing full gate. Add the reproducible composition scenario to ordinary CI while keeping
standalone builds independent of companion availability. Inspect current hosted failures and
separate preexisting unrelated failures from regressions introduced by this work.

## 8. Documentation, review and completion

Write `docs/multi-machine-supervision.md` with actual installation/profile/server/client
commands, the minimal supported walkthrough, authorization setup, keybindings, configuration
schema, identity/freshness/reconnect semantics, supported operations and limitations. Include
a step-by-step two-host manual acceptance checklist with cleanup commands and expected results.
Do not claim WAN/relay/NAT/mobile or cross-platform validation unless actually exercised.

Update ownership/remote docs, help text and capability descriptions to reflect implemented
behavior. Write `docs/multi-machine-supervision-implementation.md` recording design decisions,
verification commands/results, reviewed Betamax artifacts, repository/binary provenance,
publication/CI state and remaining limitations.

Review the complete implementation in a separate pass, validate findings against current
code, fix confirmed in-scope issues and rerun affected checks. If PR work is explicitly
requested, follow the repository's PR completion gate. This prompt itself does not authorize
commits, pushes, PRs, releases, remote deployment or modifications to existing user services;
follow explicit authorization in the active conversation.

Completion requires the real select → inspect → attach → return → reconnect workflow to work
across Local and two isolated remote stacks, with strict ownership, independent failure
handling, preserved exact identity and no blind application replay. Report unavailable
external validation honestly. Broad provider coverage, general server/layout restoration,
SSH bootstrap, live PTY handoff, distributed scheduling and universal Herdr parity remain
separate milestones; do not silently expand this task into them.
