# Local observation and task service API, version 1

Run `zor status --start` to start a background passive observation service when absent and
read its snapshot. Subsequent `zor status` calls read the existing service. `zor serve` runs
the same service in the foreground for direct supervision. `zor shutdown` requests its shutdown
without requesting pane termination. Already-active recorded-stop recovery may finish before shutdown.
Both commands resolve `$XDG_RUNTIME_DIR/zor` on Unix; on macOS, an absent or relative
XDG runtime falls back to `$HOME/Library/Caches/zor/runtime`. `--directory /absolute/path`
selects a different private zor directory. `zor serve --runtime /absolute/path` selects fux's
runtime independently. Global `--rules` and `--agent` options apply to the service's observer.
When reading an existing service, the status client does not load local rules or start another
observer. `--start` validates rules before creating a new service.

The task API shares the durable CLI coordination model, including adoption, prompt submission,
receipt reconciliation, explicit reports, one-shot waits, managed launches/stops, worktree
creation, owned cleanup, retained results, explicit verification, handoffs and dashboard summaries.
Broader recovery, agent coverage and remote exposure remain unfinished. The service is
independently stoppable; fux does not import, start or supervise it. Observation alone grants no
pane lifecycle authority.

## Endpoint and framing

The endpoint is `control.sock` inside an owned private directory (0700); the socket is 0600.
Both peers verify the kernel-reported effective UID. An exclusive nonblocking flock on a
private regular `service.lock` file prevents duplicate services. While holding that lock,
startup may replace an owned stale socket, but refuses non-socket/foreign endpoints and
symlink locks. Graceful shutdown removes only the socket inode it created. The lock file stays
in place so a competing process cannot acquire a different lock inode during shutdown.

Send one UTF-8 JSON object followed by LF. One connection handles one request and response;
there is no fux preface and no multiplexing of requests:

```json
{"v":1,"id":7,"op":"snapshot"}
```

Required fields are version `v: 1`, unsigned 64-bit caller `id`, and `op` (`snapshot`, `ping`, `shutdown` or `task`).
Unknown fields and operations and malformed JSON fail validation. Additional requests on the
same connection are unsupported: if they arrive separately, the first may be answered before
the connection closes; combined JSON objects fail validation.
`shutdown` additionally requires `service_instance` matching the running service. Missing or
stale identity returns `service-instance-conflict` without stopping it. A valid request returns
`stopping: true` with the usual completed response: shutdown is accepted, not necessarily finished.
The CLI reads the identity before issuing this request, so a replacement service cannot be
stopped accidentally. `task` also requires the current `service_instance`. That field is invalid on snapshot/ping.

`ping` returns `v`, `id`, `status: "completed"` and `service_instance`. `snapshot` additionally
returns:

| Field | Meaning |
|---|---|
| `service_instance` | Random 128-bit token encoded as 32 hex digits; changes on each service start |
| `sequence` | Nonwrapping scan/invalidation publication counter, scoped to this service instance; zero before first scan |
| `stale` | Initial scan pending, or scan duration plus time since publication exceeds five seconds |
| `published_age_ms` | Time since the scan was published, or null before the first scan |
| `snapshot` | Complete registry: observations, removed handles, problems, rule generation, scan/setup duration, event stream and failure counts |

Snapshot fields follow [OBSERVATION-CONTRACT.md](OBSERVATION-CONTRACT.md). A response is coherent
at its construction boundary. Observation age includes scan and subscription setup duration plus time since publication.
Event invalidation clears state/rule without resetting that age.
It excludes later socket/consumer delay; clients must add their own request elapsed time and time
held before using evidence. A fresh snapshot can contain unknown observations or discovery errors:
`stale: false` is not a claim that every pane was observed or that an agent/task succeeded.

When stale, all included observations become `unknown`, lose their matched rule, and carry a
stale-evidence problem. Handles remain useful for correlation, not for asserting continued
liveness. Before the first scan the list is empty and `problems.service` says initialization is
pending. A failed fux scan publishes empty observations and a manager problem instead of keeping
old verdicts. Native OS inspection is not advertised as hard real-time; overdue scans cannot
keep returning apparently current state through the API.

Clients may skip any number of publications. There is no event replay in this API. `removed`
is the difference from the preceding *internal scan*, not from that client's preceding request;
compare complete observation lists to reconcile. A changed service instance invalidates the
publication sequence, but fux handles can still identify unchanged running panes. The service
uses fux events to invalidate and refresh its registry. This does not create a replay stream
for service clients; they still reconcile full snapshots.

Malformed requests return `status: "failed"`, `id: null`, and `error: "invalid-request"`.
Unsupported versions return `incompatible-version` with the parsed ID. Internal observation
failure can return `observer-failed` or stop the service. Requests exceeding the byte limit,
capacity overload, expired clients and responses exceeding their bound are disconnected; a
missing reply is a transport failure, never an empty successful snapshot. Validation/instance/queue-admission rejections have no task effects. A task execution failure,
response loss or oversized reply can follow a committed operation; reconcile its durable IDs.

## Durable task operations

Task requests use a nested tagged payload and the current service incarnation:

```json
{"v":1,"id":41,"op":"task","service_instance":"SERVICE_INSTANCE","task":{"action":"list"}}
```

`task` is required only with `op: task`; other operations reject that payload. Unknown fields are
rejected in every action, including the empty list payload. All task actions require incarnation
matching before queue admission, even reads. Task state survives service replacement; the service
incarnation is a connection/configuration guard, not the task identity.

| Action | Payload fields after `action` |
|---|---|
| adopt | id, title, instance (fux), workspace, pane, optional agent |
| start | id, title, instance (fux), workspace, exactly one of cwd or worktree, argv, optional agent and integration (`"opencode"`) |
| register-adapter | id (managed launch), marker, producer; exact retry only after lifetime registration |
| heartbeat-adapter | id, marker, producer, sequence, optional observation {state, operation, input_operation, message {session, id}}; registered producer pulse, exact retries never extend freshness; see INTEGRATIONS.md |
| adapter-status | id; read-only pinned pane/producer endpoint probe with a shared two-second I/O budget; see [availability states](INTEGRATIONS.md) |
| require-artifact | id (managed task), artifact (task-scoped requirement name), path (relative to launch cwd) |
| artifact-collect | id (task), artifact (stable global ID), path (relative to launch cwd), optional requirement |
| artifact-inspect | artifact |
| result | id (task or pending launch; retained evidence view, see RESULTS.md) |
| source-collect | id (managed task with owned worktree), source (stable global ID), revision |
| verify | id (managed task), source (retained source ID); seals declared check/artifact evidence as documented in RESULTS.md |
| handoff | id (receiving task), operation, predecessors (1..8 distinct verified task IDs), text, timeout_ms; prepares a prompt as documented in HANDOFFS.md |
| group-create | id, concurrency, members (`[{"operation":"prepared-operation","after":["prerequisite-task"]}]`; after defaults empty); see GROUPS.md |
| group-inspect, group-step, group-cancel | id (group ID); step advances one operation, cancel may require bounded retirement retries |
| group-run, group-pause | id; enable/pause durable automatic group advancement; see GROUPS.md |
| group-list | no fields; compact retained group summaries |
| recover | optional after (task ID cursor); resumes one recorded managed stop as documented in RECOVERY.md |
| overview | no fields; read-only task/launch/group attention rows (at most288, including32 groups), integrations (target, producer, heartbeat), runtime, state_directory and journal generation for DASHBOARD.md; group rows include scheduling intent, failure and bounded pending-retirement evidence |
| source-inspect | source (metadata and file inventory, see SOURCES.md) |
| source-file | source, path (retained file bytes) |
| changes-collect | id (managed task with an owned worktree), changes (stable global ID) |
| changes-inspect | changes |
| require-check | id (task), check (task-scoped requirement name), argv |
| check | id (task), check (stable execution ID), timeout_ms, argv, optional requirement, source, and artifacts (map of required name to global artifact ID; max eight) |
| check-inspect | check |
| launch-reconcile | id (launch/task ID) |
| resume | id (task), operation (stable distinct ID), instance (target fux incarnation); retained launch runtime/workspace/cwd only |
| list | none |
| inspect, cancel, stop, forget | id (task ID; stop requires reconciled managed ownership) |
| prepare | id (task ID), operation, text, timeout_ms |
| reserve, submit, reconcile, wait, abandon, discard-prepared | operation |
| bind-report | operation, token, producer, sequence, input_operation, message (`{"session":"native-session","id":"native-user-message"}`); managed prompts only |
| report | operation, token, producer, sequence, input_operation, kind, optional message (same object; required after bind-report) |
| worktree-create | id, repo, branch, base |
| worktree-list | none |
| worktree-inspect, worktree-reconcile | id |
| worktree-remove | id, optional force (default false) |

`abandon` may commit cancellation before a bounded adapter retirement fails. Retry the same
operation to finish retirement; it never retracts submitted input. See [TASKS.md](TASKS.md).

Worktree fields and recovery semantics are described in [WORKTREES.md](WORKTREES.md).

Field semantics and constraints match [TASKS.md](TASKS.md). Adopt and start use the service's configured fux
runtime; it does not accept a request-supplied runtime. Existing adopted records retain their own
validated runtime. `wait` performs one check; blocking follow waits are currently CLI-only. A
report remains an attributed integration claim. Only explicit `verify` seals the declared
source/check/artifact policy and marks the task verified; see [RESULTS.md](RESULTS.md).

Global `--state-directory /absolute/path` configures the service's shared task journal. Without it,
tasks use the normal XDG_STATE_HOME/HOME fallback. Observation startup does not initialize absent
task state. The coordination worker reads existing state to recover recorded stop intents;
invalid state prevents recovery and is surfaced when a task request uses it. Request payloads cannot select a
state directory. `status --start` passes explicit state configuration to a new background service;
a running service retains its existing configuration. Local CLI callers must select the same
state directory to share coordination and writer exclusion.

Successful replies contain `v`, caller `id`, `status: completed`, `service_instance`, and `value`
with the same JSON result as the corresponding CLI transaction. `task-busy` means external journal
contention; other execution errors use `task-failed` with a bounded message. Queue saturation uses
`task-overloaded`. Jobs whose connection deadline has passed before execution use
`task-expired-before-start` and have no task effects. A response exceeding 512 KiB produces
`response-too-large`; the task may already have committed. Large inspect responses currently fail
explicitly; task-history pagination remains unfinished.

The transport `id` is response correlation, not a durable deduplication key. Reuse retained task
and prompt operation IDs after an ambiguous reply and inspect/reconcile them. A complete task
request may continue after its client disconnects. Connection expiry does not cancel a started
job or retract terminal bytes. Already-started calls use their owning operation's RPC and storage
contracts and can finish after the client is gone. `completed` describes that operation's return,
not overall task completion or application processing.

One coordination worker executes non-check task jobs, with at most eight additional queued jobs.
Two dedicated check workers share a separate queue of at most four waiting requests. Only `check`
execution uses that pool; check-inspect, task inspect/result/cancel and other operations use the
coordination worker. These limits are per service instance, not global across CLI callers or
other services sharing a journal. No worker owns a pane PTY. The
API loop never executes task filesystem/RPC work and checks pending results on its bounded poll
cadence. Task clients have up to 15 seconds from acceptance; input must still finish within the
initial three-second frame deadline. Queued jobs are checked for expiry before execution. During
shutdown active jobs may finish, queued jobs in both queues are discarded, and missing replies require
reconciliation. The service waits for active work on graceful exit; native filesystem operations
are not claimed to have a hard deadline. A running check command releases the journal lock;
preflight/publication can still briefly contend with coordination and return `task-busy`.
Cancellation does not terminate an already submitted check. A queued check that begins after
task cancellation is refused before command execution. SIGKILL recovery follows the durable task journal.

A separate single scheduler advances groups explicitly enabled with `group-run`; it does not
consume check-worker capacity. It selects at most one eligible operation, then waits one second
after that attempt finishes. Selection rotates across runnable groups, and existing admission,
verification, input receipts and deadlines remain authoritative. Failure pauses scheduling with
a bounded group diagnostic; Busy contention retries later. Startup resumes retained enabled
groups without creating absent task storage. Shutdown stops further selection and joins an
already-selected bounded submission. See [GROUPS.md](GROUPS.md) for pause races, crash limits
and the distinction between enabled intent and current service availability.

These are local same-UID control operations. Remote task exposure must receive its own koh service
authorization; an attachment grant must not implicitly expose this endpoint. See
[remote composition](REMOTE.md) for distinct endpoints, grant scope and current runtime
verification limits.

## Background startup

`status --start` prepares the private directory, reads an existing service if available, and
otherwise validates local rules before launching the same executable with an inherited private
Unix socket as its bootstrap channel. The child starts a separate Unix session, uses no parent
terminal streams, and changes cwd to the service directory. Explicit rule directories are made
absolute before launch. HOME/XDG configuration and global rule/agent options only configure a
new service; they do not replace an existing service's configuration. Use SIGHUP for reload.

The child takes the service lock and prepares its worker before reporting a random ready
identity. It accepts no public requests until the parent sends activation. EOF or a timeout
before activation cleans the child service; other callers can retry. Receiving activation
commits its lifetime: failure to deliver the acknowledgement cannot revoke it. The caller then
reconciles through the public endpoint. An error after activation may mean the service is still
running; it is never resolved by killing an already activated service. Concurrent starters
converge through the service lock and a valid response from the winner. The parent reaps its
child asynchronously when it eventually exits, without owning that service's shutdown policy.

Bootstrap frames are capped at 4096 bytes. The child waits up to three seconds per bootstrap
I/O phase; the parent applies a five-second bootstrap read deadline. Configuration loading and
native OS operations are not described as a hard end-to-end timing guarantee. The child reports
bounded startup diagnostics through the private channel; it does not retain an unbounded log.
These are local process controls, not a promise of durable exactly-once task startup.

## Bounds and lifecycle

The service admits at most 32 clients, each with a 256 KiB request limit, 512 KiB response
limit and three-second initial request lifetime from acceptance. Admitted task requests extend
the total connection lifetime to 15 seconds; ordinary observation/control clients retain three seconds. Reads/writes are nonblocking. A slow
client cannot stall the observation thread or another admitted client's response. Clients above
capacity are closed and may retry later. One bounded registry is shared, with one scan
worker, one coordination worker, two check workers and one group scheduler; readers do not start extra scans. The API loop polls with a maximum 250 ms sleep and the
observer polls fux event streams with a maximum 100 ms signal-check interval. Events coalesce
for 100 ms; fallback discovery is due three seconds from scan start. Scan/setup shares a
four-second RPC budget, with at most two seconds for discovery/captures. Even an overdue scan
is followed by a bounded nonblocking event drain. See [OBSERVATION-CONTRACT.md](OBSERVATION-CONTRACT.md)
for stream/frame/work bounds and the current full-scan cost. Native probes and rule evaluation
are outside a hard OS scheduling bound.

SIGHUP atomically reloads the service's configured rule collection. Failure preserves the prior
collection/generation and reports a rules problem. SIGTERM/SIGINT stop the API, wake and join the
observer/task workers, and clean its socket. SIGKILL may leave a stale socket; the next instance recovers it
under the lock and rediscovers live panes. Observation grants no authority to clean up those
panes or their directories. Shutdown waits for an active native probe/scan to finish; it does
not claim a hard deadline for uninterruptible OS inspection.

The status CLI exits successfully on a valid API response even when the snapshot reports fux
unavailable. Controllers must inspect `stale`, observation states and `problems`. Without `--start`, a missing or incompatible service fails with an actionable error. An explicit
`--start` only creates zor when the endpoint is missing or refuses connections; a live endpoint
with incompatible responses, peer identity errors or timeouts is reported without replacement.
Fux is never implicitly started by these observation commands.

Run the combined real-process scenario with independently built binaries:

```sh
python3 tests/verify/zor_service.py target/debug/fux zor/target/debug/zor
```

It covers duplicate startup, private endpoint modes, malformed/version/unsupported requests,
partial-client eviction, service-side reload despite invalid caller rules, kill/restart with the
same pane identity, fux loss, and graceful cleanup. It also covers concurrent background
startup over a stale socket, invalid startup configuration, caller disappearance on either side
of activation, survival after callers exit, stale shutdown rejection and refusal to replace a
live incompatible endpoint. Unit tests cover stale publication suppression
and endpoint ownership/locking. Task tests cover shared CLI/API state, large prompt frames, lost
submission replies, stale service identities, restart recovery, stalled-worker isolation, overload
rejection without effects, expired jobs and background state-directory propagation. These tests establish service contracts, not real-agent adapter
coverage or comparative superiority over herdr.
