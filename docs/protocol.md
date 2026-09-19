# Protocol and discovery

Fux and zor expose authenticated Bevy Remote Protocol (BRP) control methods. Fux separately
exposes an authenticated attachment stream for viewers. This guide explains how to discover
the current typed contract; it intentionally does not copy the complete method/schema tables.
For architectural context see [design](design.md); for credentials and trust see
[security](security.md) and [ownership](ownership.md). Runtime and compatibility evidence is
recorded separately in [verification](verification.md).

## Discover the running contract

Against already-running named servers, the thin CLI clients support:

```sh
fux --help
zor --help
fux --server default rpc.discover '{}'
fux --server default fux/schema '{}'
fux --server default registry.schema '{}'
fux --server default fux/server.info '{}'
zor --server default rpc.discover '{}'
zor --server default zor/schema '{}'
zor --server default registry.schema '{}'
zor --server default zor/server.info '{}'
```

The CLI reads the selected private descriptor and injects its authentication/incarnation
fields; do not paste credentials into example requests or logs. See
[fux CLI](../crates/fux/src/cli.rs), [zor CLI](../crates/zor/src/cli.rs), and
[client](../crates/fux/src/remote/client.rs).

There are three distinct discovery surfaces:

1. `rpc.discover` lists the **installed method allowlist**, including wrapped Bevy reads.
2. `fux/schema` / `zor/schema` describe application method parameter/result names and fields,
   derived from the typed serde declarations and `MethodSpec` entries. This is a field/type
   description using Rust type spellings, **not a complete recursive JSON Schema validator**.
   Follow nested types and serde enum tags/defaults in source when implementing a client.
3. `registry.schema` describes the **allowlisted reflected projection vocabulary** and its
   referenced types. It does not expose the entire authoritative World or define application
   method parameters. Wrapped `world.query`, `world.get_components` and
   `world.list_components` are reads of that restricted projection surface, not a general
   remote editor.

Authoritative source and review fixtures:

| Contract | Source | Fixtures / focused coverage |
|---|---|---|
| Fux method table and authentication | [remote/methods.rs](../crates/fux/src/remote/methods.rs), [schema.rs](../crates/fux/src/remote/schema.rs), sibling `*_methods.rs` modules | [methods.json](../crates/fux/tests/fixtures/brp/methods.json), [schema.json](../crates/fux/tests/fixtures/brp/schema.json), [brp.rs](../crates/fux/tests/brp.rs) |
| Zor method tables and typed operations | [remote/methods.rs](../crates/zor/src/remote/methods.rs), sibling task/resume/machine/plugin/dashboard method modules | [methods.json](../crates/zor/tests/fixtures/brp/methods.json), [schema.json](../crates/zor/tests/fixtures/brp/schema.json), [remote.rs](../crates/zor/tests/remote.rs) |
| Read projections | [fux projection](../crates/fux/src/remote/projection.rs), [zor projection](../crates/zor/src/remote/projection.rs) | Projection allowlists in those modules; wrapped-read coverage in the BRP tests |
| Attachment frames and requests | [wire.rs](../crates/fux/src/wire.rs), [model/messages.rs](../crates/fux/src/model/messages.rs) | [attachment fixtures](../crates/fux/tests/fixtures/attach), [attach.rs](../crates/fux/tests/attach.rs) |
| Provider evidence | [claims.rs](../crates/zor/src/providers/claims.rs), [providers.rs](../crates/zor/src/providers.rs) | [providers.rs](../crates/zor/tests/providers.rs), [resume.rs](../crates/zor/tests/resume.rs) |

The described-struct macros declare serde `deny_unknown_fields` structs and their field
metadata together. Method specs also provide typed deserialize/serialize round trips used by
fixtures. The fixtures are pinned examples and review diffs, not a second implementation.
Changing a public shape requires updating its typed declaration, table and affected fixture
consumers together; manually changing only JSON cannot add a method.

## BRP envelope and authority

Control uses JSON-RPC over the descriptor's loopback HTTP endpoint. A direct request has the
usual `jsonrpc`, `id`, `method` and object `params`. `token` and, for mutations, `instance` are
fields **inside `params`** alongside method-specific fields. Handlers strip the envelope
before deserializing the typed parameters.

* Every handler requires a valid token and `read`. Mutations additionally require `mutate`
  and the current server incarnation nonce. Method-specific administration requires `admin`.
  Attachment uses an independent socket token; a minted BRP `attach` bit is not that credential.
* Fux grants can be workspace-scoped. Entity selection checks ownership against that scope;
  workspace-global operations require an unscoped grant. Zor's token grants are currently
  unscoped rather than task-scoped.
* The descriptor's root token has all capabilities. Derived tokens are bounded, held in
  memory and revocable. An instance restart invalidates old mutation authority even if a
  service name or port is reused. Never infer identity from the port alone.
* Template edits compare `LayoutGeneration`; surface updates use their own revision and
  provider guard. Exact process operations carry explicit incarnation/attempt/pane guards.
  These are different checks, not interchangeable counters.
* Zor rejects mutations while the workflow journal is frozen or shutdown is active. Its
  runner's journal barrier also prevents external effect dispatch after commit failure.

The server replaces Bevy's default `RemoteMethods` resource with its authenticated allowlist.
Arbitrary built-in writes, resource mutation and unrestricted reflection are not part of this
API. The custom dispatcher drains a bounded mailbox and answers unknown methods without
stalling subsequent requests. See [fux remote host](../crates/fux/src/remote/mod.rs),
[zor remote host](../crates/zor/src/remote/mod.rs), and
[capabilities](../crates/fux/src/remote/token.rs).

Application error codes include stale generation/revision (`-32001`, fux), unauthorized
(`-32002`), not found (`-32003`), invalid/inapplicable (`-32004`) and uncertain (`-32005`, zor).
A transport error after dispatch does not prove failure. Reconcile the original operation
ID and exact owner before deciding what to do; never silently replay a destructive request.

For the high-level `zor --machine NAME task stop|cancel|resume` path, exit status distinguishes
acknowledgement (`0`), failure (`1`), accepted/pending (`2`) and uncertain (`3`). These are
machine-action results, not a promise that all generic BRP calls or all CLI commands share
that mapping. `zor machine status` exposes retained action evidence. Machine control uses
the local controller's durable intent path for these destructive task verbs; the CLI does
not bypass it with an unrecorded remote mutation. See
[CLI orchestration](../crates/zor/src/cli/operations.rs).

## Limits and retained streams

The shared [bounded HTTP transport](../crates/fux/src/remote/http.rs) caps request bodies at
1 MiB, JSON-RPC batches at 64 and active connections at 256. Request headers and bodies have
separate ten-second read deadlines. Excess connections wait in the OS backlog; temporary
accept errors back off rather than permanently losing the listener. Each host has a
64-entry forwarded mailbox and dispatches at most 1024 requests per update. These are
source limits, not latency or load-test results.

Other bounds belong to their typed subsystem: `server.info` exposes configured model limits;
[fux limits](../crates/fux/src/model/limits.rs), [zor limits](../crates/zor/src/model/limits.rs),
[surface limits](../crates/fux/src/surface.rs) and the method modules define the payload limits.
Do not equate a transport body allowance with permission to allocate that much model state.

`+watch` methods use `text/event-stream`, one `data:` line per item. They carry retained event
cursors and explicit gaps when a cursor is outside retention. A gap requires state
reconciliation; it is not a replay instruction or evidence that no intervening mutation
occurred. Revocation and bounded slow-reader handling can close streams. Reconnection resumes
observation, not mutation authority. See [fux watch](../crates/fux/src/remote/watch.rs),
[zor watch](../crates/zor/src/remote/watch.rs), and
[zor's fux event consumer](../crates/zor/src/fux_client.rs).

## Control is not attachment

A control endpoint addresses BRP. An attachment endpoint transports a viewer's instance scene
and input. Saved machine control credentials do not imply a usable attachment route; an
explicit workspace attachment binding is resolved privately through admin-only
`zor/machine.endpoint` from the active catalog. List/inspect/watch data do not disclose that
descriptor or its token.

The attachment socket uses a four-byte big-endian length prefix and serde JSON frames, capped
at 8 MiB. `Hello` authenticates token, fux instance, workspace, viewport and optional exact
pane/PID target. A successful `Welcome` establishes the viewer identity. Subsequent client
frames are typed viewer requests or applied-frame acknowledgements; server frames are scene
deltas or a typed `Bye` reason. Exact attachment refuses retargeting and closes on exact-target
loss rather than following a replacement pane.

`SceneFrame` contains allowlisted `DynamicWorld` RON components with server-stable IDs,
explicit despawns, root/target selection and terminal row deltas. The first/resync frame is
full; later deltas are relative to the attachment baseline. Receiver-side entity remapping
is required: raw Bevy entity bits are not universal IDs. `Ack` bounds outstanding frames; it
does not say that the terminal has painted the frame.

Pointer events and `SurfaceKey` carry the revision of the last completed paint at the time
input was read. Input admitted for an obsolete scene must not act on a new row, provider or
geometry. `SurfaceInput` supplies provider identity, optional provider-local node identity
and provider revision in addition to fux/viewer identity. Providers must use those identities,
not screen row positions or remapped local entity bits. Surface updates require
`expected_provider`; viewer-local `surface.scroll` also guards provider and revision.
The [design's scene/input section](design.md#scene-deltas-selection-handoff-and-painted-input)
explains the dashboard selection/handoff boundary.

## Native provider evidence and resume boundaries

Zor's native-channel contract is in [claims.rs](../crates/zor/src/providers/claims.rs):
Codex uses JSON-RPC lines (`initialize`, `thread/start`, `turn/start` and correlated events);
OpenCode uses **zor's sidecar JSONL protocol** (`hello`, `bound`, `report`, `state` frames).
Native claims must correlate report tokens, fux input receipts and retained producer lifetime.
Passive terminal classifiers inform observation, never impersonate native report authority.

`zor/task.resume-status` is a read-only eligibility query. `zor/task.resume` repeats eligibility
checks and records an explicit resume intent; `fux_instance` is distinct from the zor
`instance` in the authentication envelope. Stable operation IDs retain the same intent on
retries; recovery reconciles it rather than reissuing a launch or prompt.

Implemented native-session recreation is deliberately narrow:

* **OpenCode only:** an open managed task with a retained finished attempt and observed
  process exit, no current attempt/stop intent, no unresolved checks, group membership,
  operations or prompt delivery; a retired provider sidecar and an unambiguous native session
  binding correlated to the old receipt and producer lifetime are required.
* The original command must be a direct executable whose basename is `opencode`, with no
  arbitrary original arguments or wrapper. Recreation uses `--session` with the retained
  session ID. Previously resumed commands must match that same retained intent. The cwd and
  captured HOME/XDG storage namespace must still agree with the launch/provider context.
  Lost/missing processes alone do not prove absence.
* **Codex is refused for native resume:** the current sidecar initiates `thread/start`, not
  `thread/resume`. Having a Codex native evidence channel does not make session recreation
  supported.
* **Claude is refused for native resume:** it has no native channel in this implementation
  and remains passive. Passive recognition supplies no native session or response authority.
* No-provider and unbound attempts are also ineligible. Resume never replays an earlier
  prompt or input submission.

Sources: [resume policy](../crates/zor/src/lifecycle/resume.rs),
[resume methods](../crates/zor/src/remote/resume_methods.rs), and
[provider kinds](../crates/zor/src/providers.rs). The
[external resume scenario](../tools/xtask/src/scenarios/resume.rs) uses deterministic executable
fixtures speaking zor's OpenCode sidecar protocol. It is **not a live upstream OpenCode
compatibility test**, nor a live Codex/Claude compatibility certification. Consult
[verification](verification.md) for which runs actually completed.

## Key breaking changes in this rewrite

Consumers must migrate to the current typed fixtures and discovered method table; no legacy
shim is implied.

* Control calls are authenticated typed BRP methods, not unrestricted World mutation or the
  previous ad-hoc request surface. Unknown typed fields are refused where declared by the
  serde contract.
* Plugin prefix bindings use **`plugin:NAME/ACTION`**. Internal qualified action IDs such as
  `NAME__ACTION` are not an alternative binding spelling. Inspect effective bindings with
  `fux bindings` and `zor plugin bindings`.
* Surface updates require `expected_provider`; surface events include provider identity,
  provider node and revision. Pointer/surface-key request shapes include painted revision.
* Attachment scene deltas, explicit despawns, exact-target identity and acknowledgement are
  separate from control reads and event cursors. A new applied frame cannot rewrite the
  identity of already-read terminal input.
* Durable machine authority is the catalog plus intent file, not workflow-journal `Machine`
  entities. Private endpoint resolution uses the activated catalog; ordinary projections
  remain secret-free. Remote action acknowledgement and uncertainty are distinct outcomes.

These notes describe contracts, not final gates or benchmark results.
