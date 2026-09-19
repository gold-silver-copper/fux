# Security

This is the security contract of the current **unpublished source alpha**, not a security
certification. Runtime results, platform coverage and known failures belong in
[verification.md](verification.md); ownership and crash semantics are described in
[ownership.md](ownership.md). Active acceptance covers fux and zor, not koh or a future
iroh-ssh transport.

## Trust boundary

fux and zor run as the invoking user. Their BRP servers bind plain HTTP to loopback, with no
CORS allowances. Requests carry a bearer token in `params.token`; mutations also carry the
server `instance` nonce. This is not TLS, kernel peer-credential authentication, or protection
against another process with access to the user's private files. Do not expose either
listener on a public interface or publish descriptors, machine catalogs, plugin credentials,
or credential-bearing command output.

A plugin is **trusted local executable code**: installation may run its build command,
enabling may run startup commands, and actions/hooks run programs as the user. Provider
sidecars and check commands have the same OS-level authority. BRP capabilities limit API
calls made with a particular grant; they are **not an OS sandbox**, filesystem isolation,
environment isolation, or protection against a malicious same-user process. Process-group
ownership is lifecycle management, not confinement: a deliberately escaping descendant is
outside that guarantee.

## Credentials and API authority

- The server descriptor contains the HTTP endpoint, server PID, fresh instance nonce, and a
  server token granting all capabilities. A fux descriptor also contains an independent
  attachment endpoint/token. Tokens are generated from 256 random bits. Descriptor writers
  use a private `0600` temporary file, sync it and rename it into place.
- Runtime directories are created private (`0700`) and checked for ownership and absence of
  group/world access. The shared descriptor reader rejects a non-regular file, group/world
  permissions, or a file larger than 64 KiB. **It does not separately check the file UID or
  require owner bits to equal exactly `0600`**; its metadata check followed by read is not a
  race-hardened open. Keep descriptors in the protected directory. Do not treat an arbitrary
  caller-supplied path as equivalent to that protected placement.
- Handlers authenticate before side effects. Reads require `read`; mutations require the
  appropriate grant and matching instance. Mint/revoke and privileged management require
  `admin`. Minting cannot exceed the minter's capability grant. fux grants may be scoped to a
  workspace; that scope restricts reads, watches and writes, not just mutation calls.
- Built-in reflective reads expose allowlisted projection components, not arbitrary World
  state. Reflective mutation methods are not registered. A READ-only grant cannot use
  mutation, token-management, plugin-management or credential-resolution methods to escape
  its authority. Reading a projection is not permission to act on it.
- Minted grants are runtime-only, bounded by the configured token capacity, and do not
  survive restart or scene restore. Revocation closes token-bound watch streams and discards
  their queued deliveries. The server token itself is not revocable; restarting replaces
  that incarnation's authority.
- Plugin `ZOR_BRP` is activation-specific, with READ/MUTATE rather than ADMIN. `FUX_BRP` is
  run-specific, using a BRP grant scoped to its workspace. Each descriptor path is fixed to
  that activation/run: re-enabling or starting another run does not overwrite a stale child's
  path with a new grant. Disabling revokes grants and stops owned children; bounded retained
  grant records permit cleanup after restart. "Immutable" here means no authority rebinding
  by the host, not an immutable filesystem flag or a sandbox against the plugin itself.
- **Attachment authority is separate.** fux's stream checks its independent attachment
  token and instance, not the `attach` bit of a minted BRP grant. Plugin run/pane descriptors
  omit that server-wide credential; they contain only their scoped BRP grant. Privileged
  viewer/dashboard descriptors are separate. This is least-privilege delegation, not an
  OS sandbox against a same-user plugin reading other private files.

Sources: [`descriptor.rs`](../crates/fux/src/remote/descriptor.rs),
[`token.rs`](../crates/fux/src/remote/token.rs),
[`fux methods`](../crates/fux/src/remote/methods.rs),
[`zor methods`](../crates/zor/src/remote/methods.rs),
[`plugins.rs`](../crates/zor/src/plugins.rs).

## Machine authority and READ isolation

`$XDG_CONFIG_HOME/zor/machines.json` is a credential-bearing catalog, not a public status
file. Its directory is private `0700`; writers produce `0600` files. Catalog reads validate
ownership/private permissions, reject symlinks, use `O_NOFOLLOW`, recheck the opened file,
and enforce a 256 KiB bound. The schema permits at most 32 machines and 64 workspace
attachment bindings per machine. The private adjacent
`machines.json.action-intents.json` stores bounded action evidence.

The active validated catalog and action-intent file are the only durable machine authority.
Runtime `Machine` entities and observations are rebuilt from it; the workflow journal does
not restore a competing machine configuration. A failed catalog load does not authorize
fallback to stale journal credentials. Catalog edits, rename/removal and reload must be
observed through the active controller rather than guessed from another on-disk copy.

`zor/machine.endpoint` requires ADMIN and resolves the **active** catalog entry. Its response
contains credentials: do not log or share it. `machine.list`, `machine.inspect`, projections,
and watches remain secret-free status surfaces; READ does not disclose endpoint tokens.
Control and attachment are separate bindings. Current transports are explicit direct literal
loopback endpoints; resolution neither starts a tunnel/helper nor permits retries. An
external tunnel, if supplied by an operator, is the operator's trust boundary. There is no
active iroh-ssh transport or remote installation promise.

Sources: [`catalog.rs`](../crates/zor/src/machines/catalog.rs),
[`machines.rs`](../crates/zor/src/machines.rs),
[`machine_methods.rs`](../crates/zor/src/remote/machine_methods.rs),
[`transport.rs`](../crates/zor/src/machines/transport.rs).

## Stale actions, UI input and cleanup

Task actions are checked against the selected machine/controller incarnation and exact
attempt and pane binding (fux instance, workspace, pane and PID when known). An exact viewer
stays bound to that pane and detaches when the target disappears; it must not silently follow
a later task attempt or a neighbor. Layout edits carry a generation where required.

Surface updates require `expected_provider` and a newer revision. Typed pointer/key input
refers to the last-painted revision, not a fresh lookup of whatever now occupies that
position. `SurfaceInput` identifies provider, provider node and revision. `surface.scroll` is
viewer-local. Surface cleanup checks provider identity before closing and uses the returned
layout generation before removing its container, so a late cleanup cannot delete a
replacement. Dashboard closure/detachment removes UI/observation ownership, not a task PTY;
explicit stop/cancel/pane-close operations remain destructive.

Surface text is painted as cells, with control characters replaced rather than emitted as
raw terminal control sequences. This prevents a surface string from smuggling escape
commands through the text path; it is not content moderation, URL trust, or an excuse to
execute an untrusted link handler. Real PTY output goes through the terminal emulator and
explicit viewer control policies, not the surface-text API.

Sources: [`surface_methods.rs`](../crates/fux/src/remote/surface_methods.rs),
[`paint.rs`](../crates/fux/src/viewer/paint.rs),
[`dashboard.rs`](../crates/zor/src/dashboard.rs),
[`supervision.rs`](../crates/zor/src/machines/supervision.rs),
[`plugins.rs`](../crates/zor/src/plugins.rs).

## Durable evidence is not exactly-once execution

A journaled intent/receipt can establish what was requested and what completion was observed;
it cannot atomically commit an arbitrary external program's effects with a local file.

Plugin hooks claim each source/incarnation/cursor durably **before** running matching
commands. Reconnect resumes observation, not mutation replay. A crash after the claim and
before completion can omit some or all matching work; a failed action is not automatically
retried. This is at-most-once dispatch of claimed events, **not exactly-once side effects**.
Retention gaps are reported and claimed explicitly; an incarnation change establishes a new
baseline rather than replaying another server's history.

Likewise, machine mutations commit an operation intent before dispatch. A lost response or
restart while submitting becomes uncertain; reusing a retained operation key does not
resubmit it. Task recovery does not replay a launch or prompt. Inspect evidence and use the
explicit reconcile/resume path only when its preconditions hold. Do not loop on an error,
change the operation key to force a retry, or infer completion from PTY text alone. A fresh
key is a new side-effect authorization, not proof that the old one failed.

Sources: [`hooks.rs`](../crates/zor/src/plugins/hooks.rs),
[`intents.rs`](../crates/zor/src/machines/intents.rs),
[`recovery.rs`](../crates/zor/src/lifecycle/recovery.rs),
[`input_ops.rs`](../crates/fux/src/input_ops.rs).

## Owned child lifetimes

fux owns task PTYs independently of zor and attached viewers. Closing a viewer or losing the
controller does not transfer, restart or kill those task processes. Restarting fux is a
process-loss boundary: a saved session is a recipe/history, not a suspended live process.

Provider sidecars, checks and plugin commands belong to their respective zor adapters, not
those task PTYs. Adapters track their exact spawned child/process group, reap it and cancel
owned work; they do not search globally by command name. Production adapter commands use a
separate guardian process so host `SIGKILL` can still cause owned-group cleanup, unlike an
in-process destructor alone. Plugin termination allows a bounded SIGTERM grace before
SIGKILL. Normal check timeout/cancellation also kills and reaps its owned group. This does
not guarantee cleanup after simultaneous host-and-guardian failure, OS failure, or deliberate
process-group escape, and it cannot roll back side effects already performed.

Signal authority is retired before a child's PID is released. Helper adapters observe exit
with `waitid(WNOWAIT)`, kill the owned group while the unreaped leader still reserves its PID,
then reap. PTY signalling and nonblocking reaping share one lock. Delayed completions or
grace timers must never signal a numeric group ID after reaping; `ECHILD` ends that authority.

Sources: [`pty.rs`](../crates/fux/src/pty.rs),
[`host.rs`](../crates/zor/src/plugins/host.rs),
[`provider adapter`](../crates/zor/src/providers/adapter.rs),
[`check runner`](../crates/zor/src/checks/runner.rs).

## Bounded transports and remaining denial of service

The default HTTP acceptor shared by fux and zor limits request bodies to 1 MiB, batches to 64
requests and concurrent connections to 256. Request headers and body each have a 10-second
deadline; accept failures such as `EMFILE` back off instead of permanently ending the
listener. Oversized bodies are discarded within the deadline and refused, not buffered
whole. The Bevy reference transport is not the bounded-default guarantee.

Each server limits open watches to 256. fux's reflected observer buffer holds 256 event
bodies, dropping the oldest with an explicit dropped count on overflow. Retained event
streams use cursors and report gaps rather than promising unlimited retention. The shared
SSE client bounds headers at 32 KiB and each chunk/event record at 4 MiB before allocation;
a quiet accepted watch may wait indefinitely. Ordinary calls have a 30-second whole-call
deadline and a 64 MiB reply bound. Reconnecting a watch does not retry a mutation.

The attachment listener requires a valid Hello within 5 seconds, caps frames at 8 MiB and
caps connections at twice the configured viewer limit. The World decides workspace and
exact-target admission after authentication.

These bounds limit memory and retained work, **not availability against a hostile local
process**. Unauthenticated peers can hold connection slots and reconnect; authorized peers
can spend server time within the bounds. Long-lived watches occupy connections. fux does
not raise the OS file-descriptor limit. The loopback/private-file model and configured
resource limits are therefore necessary deployment assumptions, not a multi-tenant DoS
solution. See [verification.md](verification.md) for measured results rather than deriving
performance claims from these constants.

Sources: [`http.rs`](../crates/fux/src/remote/http.rs),
[`client.rs`](../crates/fux/src/remote/client.rs),
[`fux watch`](../crates/fux/src/remote/watch.rs),
[`zor watch`](../crates/zor/src/remote/watch.rs),
[`attach`](../crates/fux/src/attach/mod.rs).

## Disk data and operational handling

| Location | Writer/placement policy | Sensitive content |
|---|---|---|
| Runtime `fux/`, `zor/` | Private owner-checked directories, created `0700` | Live bearer descriptors |
| `<runtime>/<app>/<server>.brp.json` | `0600`, temporary file + sync + rename | Endpoint, token, PID, incarnation; fux attachment token |
| `<state>/fux/session/<server>.scn.ron` | `0600`, atomic session save under private state | Allowlisted layout, launch attribution, names/cwd, bounded historical screen |
| `<config>/fux/layouts/<name>.scn.ron` | `0600` under private layouts directory | Allowlisted layout/launch data |
| `<state>/zor/journal.scn.ron` | `0600`, temporary file + fsync + rename + directory sync | Allowlisted workflow/policy state, not machine authority |
| `<state>/zor/archive/<date>.scn.ron` | `0400`, merged through a replacement file | Closed-task history |
| `<config>/zor/machines.json` and adjacent action intents | Private directory `0700`; file `0600`, atomic synced write | Catalog credentials and action evidence |
| `<state>/zor/plugins/<name>/` | Under private zor state; descriptors written `0600` | Copied/linked plugin references, durable plugin state, logs, cursors, scoped descriptors |

The fux configuration directory itself is not made private by `Paths::prepare`; zor's is,
because its catalog contains credentials. XDG fallbacks and recovery steps are in
[installation.md](installation.md). Private directory checks reject unsafe existing
permissions rather than silently broadening access. Keep backups private too.

Scene extraction excludes live PTY handles, sockets, bearer grants and the server nonce.
That is **not** a blanket claim that all saved data is secret-free: cwd/argv, launch data,
terminal history, task/check output, plugin state and logs may contain secrets supplied by
the user's commands. Review before sharing any state or diagnostic output. A hand-edited
session is validated, then launched through normal pane creation; restoring it still runs
commands and must be treated as executing user-controlled code.
