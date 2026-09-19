# Design

This describes the implemented fux/zor architecture, not an acceptance verdict. See
[verification](verification.md) for executed evidence and [capability status](capability-status.md)
for coverage. The [protocol](protocol.md), [ownership](ownership.md), and
[security](security.md) documents describe the external and authority boundaries.

## Apps, Worlds, and composition

There is one **scheduled, authoritative World per App**: the fux server owns terminal
sessions, the zor server owns workflow policy, and each fux viewer owns its presentation and
input state. CLI control commands are clients, not additional model Worlds. Zor uses fux's
public protocol rather than reaching into a fux World. An entity ID in one World is not an
identity in another World.

Inert Worlds are an explicit exception, not parallel authorities. Scene loading and journal
restore deserialize into staging Worlds before validation and commit. The dashboard's
`SceneWorld` retains provider-local scene entities solely to compute stable scene deltas;
it runs no App or lifecycle schedule. It borrows the main App's scene/asset resources during
scene resolution. See [scene templates](../crates/fux/src/scene/templates.rs),
[scene loading](../crates/fux/src/scene/mod.rs), [journal](../crates/zor/src/journal.rs), and
[dashboard scene](../crates/zor/src/dashboard/scene.rs).

Plugin construction order installs dependencies; schedule sets establish execution order:

* [Fux assembly](../crates/fux/src/app.rs) installs task pools, states, time, logging, assets,
  scenes and diagnostics, then config/layout assets, model, layout, terminal ingest,
  lifecycle, pointer, surface, attachment projection, input receipts, finals and events.
  The full server adds BRP, attachment and session-persistence plugins. The headless builder
  shares the model plugins without OS-facing transports.
* [Zor assembly](../crates/zor/src/app.rs) installs task pools, states, time, logging, assets
  and diagnostics. Its core composes model, journal, git, worktrees, groups, lifecycle,
  checks, plugins, dashboard and machines; providers and the BRP host complete the server.
* [Viewer assembly](../crates/fux/src/viewer/mod.rs) installs assets and the shared UI stack,
  input focus/dispatch/navigation, replication, chrome, prompts, choosers and painting.
  Viewer `Mode` is a native state, with computed `Modal`; server `ServerMode` is likewise a
  native state. Per-task and per-pane lifecycles are entity components, not global states.

The runners feed bounded batches into `Messages<Inbound>`, call `App::update()`, then apply
`Effect` messages through adapters. Fux's ordered phases are ingest (`First`), requests then
completions (`PreUpdate`), lifecycle then instance layout (`Update`), Bevy UI layout then
projection (`PostUpdate`), and effects/clearing (`Last`). Zor replaces layout with workflow
lifecycle and orders projection **before journal commit** in `PostUpdate`. The BRP schedule
`RemoteLast` is moved immediately after `First`, ahead of request processing. See the
[fux phases](../crates/fux/src/model/mod.rs), [zor phases](../crates/zor/src/model/mod.rs),
[shared runner](../crates/fux/src/runner/mod.rs), and [zor runner](../crates/zor/src/runner.rs).
Messages are step-local; durable facts live in components or explicit retained logs.

Adapter admission may return an immediate typed completion. The runner inserts it directly
into the next scheduled inbound batch and wakes that update; it does not enqueue it through
an already-full transport channel. In particular, Git capacity/shutdown refusal cannot lose
an already-journaled request or leave a worktree flight waiting forever. Accepted external
work keeps its normal bounded asynchronous completion path.

## Native Bevy mechanisms, not a second framework

Bevy is pinned to **0.19.1**; the reference checkout is
`b56fc29d3016e641754765244b5ba3f9cc504671`. The detailed
[Bevy source patterns](bevy-source-patterns.md) document records mechanisms and pitfalls,
but its proposed fux uses are not a substitute for the current source. In particular, the
implemented multi-viewer design clones per-viewer instances rather than selecting a single
camera for a shared template.

| Mechanism | Pinned Bevy source | Current use |
|---|---|---|
| Relationship hooks and linked lifetime | `bevy_ecs/src/relationship/mod.rs:149–240,270–278` | `RootOf/Roots`, `InstanceOf/Instances`, `Shows/ShownBy`, task/attempt relations; mutate the forward relation, not a hand-maintained reverse index. `linked_spawn` is used where lifetime really follows the owner. |
| Linked cloning and entity remapping | `bevy_ecs/src/entity/clone_entities.rs:483–502,884–888` | `EntityCloner` clones template trees over `Children`; instance links retain their template source. |
| UI layout and stack | `bevy_ui/src/lib.rs:142–241`, `layout/mod.rs:77–176`, `stack.rs:54–141` | Native `Node` flex/grid, `ComputedNode` geometry, `UiGlobalTransform`, stack ordering and cell-space picking. |
| Pointer messages and bubbling events | `bevy_picking/src/backend.rs:93–126`, `events.rs:74–122` | Per-viewer custom pointer IDs, cell hit backend, native pointer observers. No window-input backend or OS window is required. |
| State transitions | `bevy_state/src/state/transitions.rs:81–92` | Server and viewer modes use ordered native state schedules; separate entity lifecycles remain typed components. |
| Asynchronous assets | `bevy_asset/src/server/mod.rs:573–604,2056–2072`, `io/source.rs:229–256` | Typed config/layout/rule/catalog assets. A waking asset source wakes the otherwise event-driven runner for watcher events and load completions. Invalid reloads retain the last accepted state. |
| Scene resolution | `bevy_scene/src/scene.rs:48–79` | `bsn!` scenes resolve through the native `Scene` contract; layout import, surfaces and journal use allowlisted `DynamicWorld` serialization with entity remapping. |
| Registered remote systems | `bevy_remote/src/lib.rs:805–830,954–993` | Method handlers are registered systems; transports deliver `BrpMessage`s, not World references. The product replaces the method table with authenticated handlers and a bounded dispatcher. |
| Owned asynchronous tasks | `bevy_tasks/src/task_pool.rs:549–568` | Adapters retain finite `Task` handles and report typed completions instead of blocking the World on network I/O. |

All Bevy paths above are under `crates/` in the
[pinned upstream tree](https://github.com/bevyengine/bevy/tree/b56fc29d3016e641754765244b5ba3f9cc504671/crates).
The use of these mechanisms, not a dependency count, is the architectural claim.

The shared [UI stack](../crates/fux/src/layout/mod.rs) enables `UiPlugin`, transforms and the
picking/interaction plugins, with window picking disabled. It initializes the text/image
resources that native UI systems validate, but fux paints terminal cells itself; it does not
install a GPU renderer or use Bevy text layout for terminal text. This resource-only dependency
exception is recorded in [dependencies](dependencies.md).

## Templates, instances, and identity

A workspace owns ordered template roots (tabs). Template nodes describe layout and pane
placement, not a terminal emulator per view. Templates remain under the workspace and are
not native UI roots. Each viewer showing a root gets a parentless instance clone targeted at
that viewer's camera and viewport. `LayoutGeneration` invalidates structure; cloning runs
only when the root/viewer binding or generation changes. `ViewState` reapplies zoom, scroll
and display overrides by stable template `NodeId` after cloning.

A pane is shared process/terminal state. `Places` connects a template leaf to its pane;
`Shows` connects each instance leaf to that same pane. After native layout,
`fold_pane_sizes` derives the pane's PTY size from the minimum visible instance content sizes.
Changing a viewer viewport or zoom does not create another process. `Targets` is the server's
per-viewer input target, distinct from viewer-local `InputFocus` used by chrome. Exact-target
attachments cannot retarget and detach if that exact pane/process identity is lost.

Sources: [relationships](../crates/fux/src/model/relations.rs),
[instance synchronization](../crates/fux/src/layout/instances.rs),
[size folding](../crates/fux/src/layout/size.rs), and [layout operations](../crates/fux/src/layout/ops.rs).
Stable model IDs, provider source IDs, server instance entity IDs and viewer-remapped entity
IDs are deliberately different namespaces. Bevy `Changed<T>` is local invalidation, never a
wire revision or permission to replay an operation.

## Typed lifecycle, evidence, and durability

Zor separates `TaskState`, `AttemptState`, `Delivery`, `WaitState`, ownership and operation
phase. A submitted input receipt does not imply a response; a response does not imply verified
work; a passive screen classification does not prove process completion. The exact
`PaneHandle` includes fux incarnation, workspace/stream, pane and observed PID. Final process
evidence retains exit status when observed, sequence and bounded output. Receipt-correlated
`Binding` and `ResponseEvent` facts are separate from provider `Observation`. Verification
is performed through checks and the verification seal, not a dashboard label or “idle” text.
See [model components](../crates/zor/src/model/components.rs),
[model contract](../crates/zor/docs/model.md), [waits](../crates/zor/src/lifecycle/waits.rs),
[recovery](../crates/zor/src/lifecycle/recovery.rs), and [verification](../crates/zor/src/checks/verify.rs).

The workflow journal is an allowlisted native scene snapshot with invariant checking and
atomic replacement (write, file sync, rename, directory sync). A corrupt committed document
is preserved and freezes mutation. A failed commit leaves the journal dirty. Before draining
an effect batch, `ZorHost::after_step` exits with failure if a non-frozen journal is still
dirty, or if a frozen journal has effects waiting. **No external effect batch is authorized by
a failed journal commit.** The in-memory change or a preliminary BRP reply is not proof that
an external action completed; clients reconcile retained evidence after a loss.

Shutdown is also a lifecycle, not simply dropping an empty queue. Zor asks adapters to stop
owned work, then keeps updating until adapter work, inbound/control completions **and
newly generated effects** drain. The bounded deadline is five seconds; expiration is failure,
not a clean drain. Zor does not kill fux-owned task panes as a side effect of its shutdown.
Fux has its own pane shutdown policy. The finite `FuxAdapter` retains up to 256 outstanding
`Task<()>` calls, reaps completed handles, enqueues completion before a task finishes and
uses an async call deadline. It neither detaches finite mutation calls nor silently retries
lost replies. Sources: [journal](../crates/zor/src/journal.rs),
[runner barrier and drain](../crates/zor/src/runner.rs), and
[FuxAdapter](../crates/zor/src/fux_client.rs).

### Explicit execution exceptions

* Bounded synchronous atomic persistence, credential/descriptor reads and bounded log tails
  remain at their documented call sites. Atomic durability is not delegated to an unobserved
  background write.
* Kernel PTY/process-group/signal adapters remain OS-specific (`portable-pty`, `nix`, safe
  descriptor duplication and the signal self-pipe). Bevy has no PTY or signal ownership API.
  The same narrow native module wraps `waitid(WNOWAIT)` because nix does not expose it on
  macOS; this preserves an unreaped leader's identity until owned-group cleanup completes.
* Inert staging/scene Worlds described above have no independent lifecycle authority.
* Existing viewer BRP calls use dedicated threads, bounded to 32 outstanding calls and a
  32-entry reply channel. Their synchronous client must stay off the scheduled World and
  the asset/I/O pool; calling it inside a shared pool task would not make it asynchronous.

See [PTY adapter](../crates/fux/src/pty.rs), [signals](../crates/fux/src/runner/signals.rs),
[viewer BRP ownership](../crates/fux/src/viewer/mod.rs), and [plugin host](../crates/zor/src/plugins/host.rs).

## Direct machines and durable action authority

The private `machines.json` catalog is desired endpoint configuration. Activated catalog
bindings create machine entities whose workers own network observation and action queues.
Each direct machine has an independent bounded worker: one-second polling, six-second read
budget and five-second freshness. Failure preserves the last view as stale without renewing
its timestamp. Another machine's failed authorization does not become this one's state.

**Catalog plus machine intent log are the sole durable machine authority.** Machine entities,
workers and observations are not serialized in the workflow journal. Catalog activation
retires changed bindings and reconciles pending action evidence; a workflow restore cannot
resurrect removed credentials or endpoints. `zor/machine.endpoint` is an admin-only private
resolver over the activated catalog, not a public machine projection and not a pending draft.
Machine list/inspect/watch projections omit credentials. The config directory is private
0700 and catalog writes are 0600. Direct control endpoints and attachment bindings are
separate: a saved control route alone is not an attachment destination.

Actions persist intent before dispatch; workers recheck incarnation/current attempt/exact
pane identity before one mutation. Accepted/pending, acknowledged, failed and uncertain are
separate results. A lost reply is not permission for an automatic retry. See
[machines](../crates/zor/src/machines.rs), [catalog](../crates/zor/src/machines/catalog.rs),
[intents](../crates/zor/src/machines/intents.rs),
[supervision](../crates/zor/src/machines/supervision.rs), and [transport](../crates/zor/src/machines/transport.rs).
Future `iroh-ssh` integration is outside this direct-transport contract.

## Scene deltas, selection, handoff, and painted input

The server attachment projection exports allowlisted instance components as a full scene or
delta, explicit despawns, root order/target changes and terminal row deltas. The viewer
remaps stable server entity IDs into its own World and acknowledges applied revisions to
bound the stream. Acknowledged is not painted.

Typed pointer and surface-key input carry the **last completed paint revision captured when
the terminal event was read**, not whichever frame happens to be newest when the input is
processed. Server admission compares the captured input scene to current authority; obsolete
input cannot activate replacement rows or continue an old gesture on new geometry. Output-only
terminal progress can preserve admissibility when the input scene is unchanged. Surface
input also carries provider identity, provider-local source node and provider revision;
`surface.update` requires `expected_provider`. `surface.scroll` changes the named viewer's
instance state, not shared provider state.

The zor dashboard keys rows by stable row identity and preserves provider entity IDs across
text updates and reorder. Structural changes export a full scene; text-only changes export
changed rows. Selection and input are checked against the acknowledged provider scene;
input is refused while a scene update is pending. Row activation re-derives fresh target
authority rather than trusting an old row number. Attach produces a viewer-bound handoff;
the CLI revalidates the selected binding and opens an exact-target fux attachment. Returning
from attachment resumes the dashboard, while closing the dashboard removes its own UI and
does not stop the selected task.

Sources: [attachment projection](../crates/fux/src/attach/projection.rs),
[replication](../crates/fux/src/viewer/replicate.rs), [pointer admission](../crates/fux/src/pointer.rs),
[surfaces](../crates/fux/src/surface.rs), [dashboard](../crates/zor/src/dashboard.rs),
[dashboard scene](../crates/zor/src/dashboard/scene.rs), and
[handoff CLI](../crates/zor/src/cli/operations/dashboard_cli.rs).
