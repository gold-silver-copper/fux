# Where fux breaks under hostile input (hunt 5)

> **Status: both findings are fixed.** 001 in `938f257`, 002 in `334752a`
> (with `8e3a341`, which refuses a non-socket at the path before creating a
> lockfile beside it); see
> "Fixed" under each finding and "Found while fixing" at the end. The analysis
> below is the state before those commits and is kept as it was found.

This run removed the conditions earlier hunts kept: it spoke to the loopback
port with raw sockets, called stock reflection methods no scenario had called,
made the filesystem an adversary, starved file descriptors, drove a real
frontend with garbage bytes, reordered lifecycle events, and pushed untrusted
strings at the chrome. It read `src/` to aim at the unwraps and casts the
2026-09-20 review counted. It finds; it does not fix.

**Two distinct breaks, in two classes. No other class had a finding.** Every
other input was correctly refused with a JSON-RPC error, an `error:true`
notice, a `failed` process status, or a documented availability message. The
counts per area are in the table at the end.

Severity classes (from the hunt brief):

1. server exits/panics/aborts  2. request hangs  3. child/PTY outlives its owner
4. outer terminal left un-restored  5. unbounded growth  6. silent invariant break
7. untrusted byte reaches the terminal unencoded  8. refused input changes state,
or an input a web page can send

---

## 001 — A `Viewer` component on a layout entity aborts the server (class 1)

**The break.** Two BRP requests, both accepted, abort the whole server with a
stack overflow, taking every attached session and child process with it:

1. `world.insert_components` a `fux::model::Viewer` onto an existing **tab**
   entity (a `Workspace` or `PaneView` entity works identically).
2. `world.insert_components` any viewer relationship (`Viewing`, `OnTab` or
   `Focused`) onto a real viewer.

No `fux.frame` is needed; the second insert is enough. stderr ends:

```
thread 'main' (…) has overflowed its stack
fatal runtime error: stack overflow, aborting
```

**Where it goes wrong.** `src/navigation.rs`, `repair` (around line 357):

```rust
let viewers: Vec<_> = world
    .query_filtered::<Entity, With<Viewer>>()
    .iter(world)
    .collect();
```

`repair` treats every entity carrying a `Viewer` component as a viewer to be
made consistent. A `Viewer` on a layout entity (a tab) enters this set. `repair`
then inserts `Viewing`/`OnTab`/`Focused` onto that tab-as-viewer, and those
inserts re-fire the component hooks `remember_tab` and `remember_focus`
(`src/navigation.rs:290-319`), each of which calls `repair_later`, which queues
`repair` again. A tab entity never satisfies the viewer-consistency conditions,
so the queued repairs recurse without converging until the thread's stack
overflows. The comment at `src/navigation.rs:355-356` — "the insertions here
cannot trigger another repair" — is the false assumption: it holds for real
viewers, not for a `Viewer` sitting on a layout node.

**Smallest input that does NOT break, to bound it.** A `Viewer` component on a
tab entity *alone*, with no subsequent relationship insert, does not crash: the
tab-as-viewer is inert until something queues `repair`. So does any relationship
insert on a real viewer when no layout entity carries a stray `Viewer`. Both
halves are required.

**Class of inputs the fix must cover.** Any `Viewer` component present on an
entity that is also a layout node (`Tab`, `Workspace`, `PaneView`, `Split`), for
every path that queues `repair` (relationship insert/remove, tab removal,
child-added). The fix is to make `repair` select only genuine viewers (exclude
entities that also carry a layout-node component), or to make repair converge /
be depth-bounded so a pathological entity cannot recurse it. The README already
warns that raw component mutation "can bypass normal transitions"; this is the
case where it aborts rather than degrades.

**Reproduction (as recorded before the fix).** `fux-fuzz/repro/001-viewer-on-tab-repair-recursion.sh <fux>`
(no agent, no seed; exit 0 = reproduced). Also in the harness as the then
failing `hostile` scenario and the trace
`fux-fuzz/traces/open/001-viewer-on-layout-entity.json`:

```
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario hostile
```

---

**Fixed in `938f257`.** The impossible state is removed where it is created,
following the cycle guard in `normalize_on_child_added`: an observer on the
insertion of `Viewer` and of each layout component (`Workspace`, `Tab`,
`Split`, `PaneView`) removes the `Viewer` from any entity that has both, in
either order or in one insertion. The layout role wins; the node also loses
the viewer-only state it gained (the relationships repair gave it, `Memory`,
paste `Ownership`, and any presentation, prefix, overlay or selection). Passes
over viewers (`repair` and the frame passes) select only entities with
`Viewer` and no layout component, so the moment before the removal is safe
too. `repair` no longer relies on "cannot trigger another repair": a request
made while a pass runs only marks another pass, and repair stops after at most
sixteen, with a warning. Rejected: narrowing repair's query alone, which stops
this crash but leaves the stray `Viewer` for the frame passes.

Regression coverage: unit tests in `src/navigation/tests.rs`; integration
tests `a_viewer_on_a_{tab,workspace,pane_view,split}_is_removed_and_the_server_survives`
in `tests/remote_lifecycle.rs`, each relationship trigger and insertion order
on a fresh server; the `hostile` scenario, which now also asserts the Viewer
is gone and the viewer consistent, runs in the default smoke; the trace moved
to `fux-fuzz/traces/viewer-on-layout-entity.json` and replays green. The
repro script now speaks to the Unix socket and exits 0 reproduced (abort,
no answer, or the Viewer kept), 1 verified not reproduced (answers before and
after the trigger and the tab carries no Viewer), 2 setup failure; against
the fixed build it exits 1.

---

## 002 — A web page can drive the server: no CORS or Content-Type gate (class 8)

**The break.** The BRP HTTP endpoint checks neither `Content-Type` nor
`Origin`. A browser "simple request" — a `POST` with `Content-Type:
text/plain;charset=UTF-8` and any `Origin`, which the browser sends with **no
CORS preflight** — is delivered and executed. The response advertises no
`Access-Control-Allow-Origin`, so page JavaScript cannot read the reply, but the
side effect has already run server-side. A cross-origin request that splits a
pane with `program: "touch MARKER; sleep 30"` creates the marker and starts the
process; a `fux.frame` request returns the full screen.

Consequently, **any web page the user visits while a fux server runs on the
default loopback port can run arbitrary programs on the local machine**, and can
drive every other command (split, close, move, rename, load_layout). The reply
being unreadable does not blunt a fire-and-forget command.

**Where it goes wrong.** `src/main.rs:99-101`:

```rust
RemoteHttpPlugin::default()
    .with_address(address)
    .with_port(port),
```

The stock `RemoteHttpPlugin` is mounted with no origin allowlist and no
`Content-Type` requirement, so it accepts a cross-origin simple request. This
confirms review finding 6 (`docs/review-2026-09-20.md`), which flagged the CORS
question as untested. It is not a preflight-bypass subtlety: the browser never
preflights a `text/plain` body, so an allowlist checked only on `OPTIONS` would
not help.

**Smallest input that does NOT break, to bound it.** The attack needs only a
`text/plain` (or `application/x-www-form-urlencoded` or `multipart/form-data`)
body — the three "simple" content types. A body with `Content-Type:
application/json` *from a browser* would trigger a preflight the server does not
answer with permissive headers, so the browser would block it; the gap is
precisely the simple-request content types.

**Class of inputs the fix must cover.** Any cross-origin request that reaches
command dispatch. The fix is a transport gate the review already proposed:
reject requests whose `Origin` is not an allowlisted local origin, and/or
require `Content-Type: application/json` (which forces a preflight the server
can then deny), and/or move the listener to a Unix domain socket or add a bearer
token. This is a same-user local endpoint by design, so the target is the
*browser* reachability, not local same-user callers.

**Reproduction (as recorded before the fix).** `fux-fuzz/repro/002-cross-origin-web-page-rce.sh <fux>`
(no agent; exit 0 = a cross-origin simple request executed a program).

Both repro scripts discriminate through neutered negative controls (a variant
that omits the trigger exits non-zero). Building the pre-F1-fix commit
`5936ff1^` for a second cross-check was not cheap here — the full Bevy graph
rebuilds — so discrimination rests on the negative controls rather than an old
binary.

---

**Fixed in `334752a` (refined in `8e3a341`), by removing reachability rather than filtering.** fux
no longer listens on TCP at all: it serves the same HTTP/1 BRP only on a Unix
domain socket (`--socket`, `FUX_SOCKET`, else `$XDG_RUNTIME_DIR/fux/server.sock`
or `$TMPDIR/fux/server.sock`), mode 0600 in a mode-0700 directory owned by the
user. A web page cannot open a Unix socket, so the whole cross-origin class is
gone by construction, and the permissions also close the other-local-user
exposure of the loopback port. There is still no authentication for a caller
who can open the socket. `--address`, `--port` and `FUX_ENDPOINT` were removed
without a fallback. Rejected: an `Origin` allowlist or a `Content-Type:
application/json` requirement (they out-guess what a browser can send and
leave the port open to other users) and a bearer token (the brief ruled out
credentials; permissions do the same job for a same-user tool).

Regression coverage: an integration test asserts the socket is 0600 and its
directory 0700 under `umask 000` and that the server's own pid holds no
internet socket (`lsof`, after proving `lsof` sees the pid's Unix socket).
The repro script now fires the original browser request only at TCP
listeners it finds on the server's own pid, never over the socket and never
at a port the server does not own. It exits 0 reproduced, 1 verified not
reproduced (healthy over the socket, no internet listener), 2 setup failure;
against the fixed build it exits 1.

Both repro scripts as merged in PR #44 still exit 0 against a fresh build of
`839516a`, the commit before the fixes, so the discrimination they were
written with stands. The migrated scripts start the server with `--socket`,
which a pre-fix build rejects, so they report exit 2 there, not a finding.

---

## What did not break (coverage, not findings)

Every case below was tried and correctly refused or absorbed; none is a finding.

| Area | Inputs tried | Correctly refused / absorbed | Breaks |
| --- | ---: | ---: | ---: |
| 1 transport (raw sockets, streaming, CORS) | 23 | 22 | 1 (002) |
| 2 stock methods, wrong-kind inserts, reparent, bad ids | 49 | 48 | 1 (001) |
| 3 filesystem adversary (config/layout/scene-temp/full) | 20 | 20 | 0 |
| 4 resource exhaustion (ulimit, children, 60s plateau) | 12 | 12 | 0 |
| 5 outer terminal against the frontend + CLI edges | 22 | 22 | 0 |
| 6 lifecycle orderings | 5 | 5 | 0 |
| 7 injection through the chrome | 12 | 12 | 0 |

Highlights of the non-findings, because they bound the two that did break:

- **Transport.** A body that is not JSON, JSON that is not an object, a
  1000-request batch, an id that is an object, a 1 MB method name, params
  nested 10 000 deep, a `Content-Length` larger and smaller than the body, an
  endless chunked body, 100 silent connections, a 12 s slowloris, and 10-deep
  pipelining all returned typed errors or timed out on the client with the
  server still answering the next request. 100 unread `fux.frame+watch`
  connections plus 200 paints, one unread watch under 1000 paints, and 20
  mid-frame socket closes left RSS, fds and threads flat and every other viewer
  painting.
- **Reflection.** `Settings` mutated over BRP with an empty prefix, a
  bound-key prefix, `history_lines` of 0 and `usize::MAX`, an empty/dir/absent
  `shell`, and `layout` pointing outside the config dir: all absorbed or
  rejected, no disagreement that panics. `Overlay`/`Launch`/`ProcessState`
  nested-path mutations, wrong-kind inserts other than the 001 pair, reparents,
  and entity ids of `-1`, `1.5`, `u64::MAX`, `u64::MAX+1` and a string were
  refused or repaired.
- **Filesystem.** Config as a directory, a writerless FIFO, a symlink loop, a
  symlink to `/dev/zero`, unreadable, 701 rewrites in 5 s, and deleted then
  recreated: the watcher kept the last usable config and the server stayed up.
  `load_layout` at a FIFO, a directory, `/etc/passwd`, a missing file and a 2 GB
  sparse file, and pre-created scene-task temp files as a directory and a
  read-only file: all survived. A writerless-FIFO load held the server at ~1%
  CPU (no busy loop). The `ENOSPC` write-failure path was exercised through a
  read-only temp file and the 2 GB sparse file rather than a filled `hdiutil`
  RAM disk, which was not run unattended to avoid leaving the machine
  constrained; the write-error handling reached is the same.
- **Resource limits.** Under `ulimit -n 28`, PTY exhaustion surfaced as a
  `failed` status ("dup of fd … failed") with the server alive and painting;
  closing panes restored capacity. 8 `yes` panes + one `/dev/urandom` pane with
  two frontends resizing every 100 ms for 60 s held RSS at 52.9→53.7 MB, fds and
  threads flat. A 16 MB no-newline line, unterminated OSC/DCS, a zero-row scroll
  region and a 2000-deep DSR flood were all bounded.
- **Outer terminal.** A bracketed-paste start with no end past `LIMIT`, a paste
  end with no start, unsolicited CPR/DA, a mouse report beyond `u16`, a held-open
  CSI, every C0 and C1 byte, invalid UTF-8, SIGWINCH to 0x0 and 1x1, and a
  1000-strong SIGWINCH storm neither crashed the frontend nor the server. On
  server death (SIGKILL and SIGTERM) the frontend exited within ~8 ms and wrote
  the alt-screen, mouse and paste resets and cursor-on. CLI edges (dead port,
  `TERM` unset, stdout to a pipe, a missing workspace) each printed one line and
  exited non-zero.
- **Lifecycle.** A 10 000-command batch, a double shutdown, shutdown during a
  pending save, despawn of a process entity whose waiter was blocked (no orphan
  child), and closing a pane whose child ignores SIGHUP/SIGTERM (hard-killed in
  ~2.1 s) all left the server healthy.
- **Chrome.** ESC, CSI, OSC 52, DCS, C1, bare CR, ZWJ and RLO in tab names,
  workspace names, pane names, a process error string and a notice all reached
  the frontend PTY stripped: `chrome::fit` removes every `char::is_control()`
  before painting (`src/chrome.rs:31`). A 1 MB name painted on a 2x2 viewer was
  bounded. No unencoded control byte reached the terminal.

## Harness mistakes (not findings)

- An early "close-restores-capacity" BREAK under `ulimit -n 64` was a harness
  error: fux caps panes by the documented 2x2 minimum (~5 panes on an 80x24
  viewer), so fd exhaustion never occurred and a later split was refused as "too
  small", not failed. Confirmed by forcing real exhaustion at `ulimit -n 28`,
  where the failure is a clean `failed` status.
- An early "SIGKILL-server-frontend-exits" BREAK was a measurement artifact of a
  sequential test; in isolation the frontend exits within milliseconds and
  restores the terminal (see Area 5 above).

## Ranked fixes for the hardening PR

> Superseded: both were made, as described under "Fixed" in each finding. For
> 002 the chosen fix is the Unix-socket listener; the `Content-Type`/`Origin`
> gate and the bearer token listed below were not adopted.

1. **Class 1 — `repair` must select only genuine viewers.** Exclude entities
   that also carry a layout-node component from `navigation::repair`'s
   `With<Viewer>` query, or bound repair's recursion, so no `Viewer` component
   on a `Tab`/`Workspace`/`PaneView`/`Split` can abort the server. (Finding 001.)
2. **Class 8 — gate the transport against the browser.** Require
   `Content-Type: application/json` and/or an `Origin` allowlist before
   dispatch, or offer a Unix-socket / bearer-token listener, so a cross-origin
   simple request from a web page cannot execute commands on the loopback
   server. (Finding 002; review finding 6.)

## Found while fixing

**`fux.frame` with an impossible entity id aborted the server (class 1). Fixed
in `4158e4d`.** `fux.frame`, `fux.frame+watch` and the watch-detach bridge
built the viewer's `Entity` with `Entity::from_bits`, which panics on bits no
entity can have (any id whose low 32 bits are zero, 0 among them). One request
`{"method":"fux.frame","params":{"viewer":0}}` ended the server. Area 2 above
tried `-1`, `1.5`, `u64::MAX` and `u64::MAX + 1`, which either fail to parse or
map to valid bits, but not 0. Both sites now use `try_from_bits`: the methods
answer with an invalid-params error naming the id, and the bridge registers no
detach for it. Covered by `frame_requests_for_impossible_entity_ids_are_refused`
in `tests/remote_lifecycle.rs`, which fails against the previous source.

---

# Where fux breaks under hostile input (hunt 6)

> **Status: all six findings are fixed**, in fux and against unmodified bevy.
> 003 in `b698ef0`, 004 and 005 in `9ad3c84`, 006 in `84202fb`, 007 in `df88f51`,
> 008 in `47bcc88`; the harness and documentation in `920efcf` and `f608724`, on
> top of the move to bevy 0.20.0-rc.1 in `61bc319`. See "Fixed" under each
> finding. The analysis below is the state before those commits and is kept as
> it was found.

This run attacked what PR #45 made fux's own: the HTTP serving loop over a Unix
socket, the socket's location and lifecycle, and the client that dials it. It
also did the identifier audit the 2026-09-20 review asked for (finding 5) and
hunt 5 sampled without covering, which is how hunt 5's `viewer: 0` abort was
missed. It finds; it does not fix. `src/` is untouched.

**Six distinct breaks, in three classes.** Class 1 (three root causes), class 2
(one), class 5 (two). Classes 3, 4, 6 and 7 had no finding of their own; the
class 6 and class 8 consequences of finding 003 are recorded inside it rather
than as separate root causes. The counts per area are in the table at the end.

Severity classes are hunt 5's, unchanged (`docs/fux-fuzz-hunt-prompt-5.md`).

**A note on entity ids, because every class-1 finding here depends on it.**
`Entity::to_bits` is an opaque encoding, not an index: `EntityIndex` is a
`NonMaxU32` whose `to_bits` is a `transmute`, so the low 32 bits of an entity id
are the *bitwise complement* of its index. Bits `0xFFFF_FFFF` therefore name
entity index 0, `0xFFFF_FFFE` index 1, and so on. Bevy 0.19 stores resources as
entities and allocates them first, so those first indices are resource entities.
A caller does not have to guess them: they are a short fixed sequence counting
down from `0xFFFF_FFFF`, and `world.query` never lists them.

---

## 003 — A watch request despawns any entity it names (class 1, and class 8, and class 6)

**The break.** `fux.frame+watch` names the viewer to stream. fux registers the
"this watcher went away, detach its viewer" bookkeeping straight from the
request's params, and `server::disconnected` then despawns that entity when the
response channel closes. Nothing checks that the entity is a viewer, so the id
in a watch request is a despawn of the caller's choosing:

| The id names | What happens |
| --- | --- |
| a tab | the tab and its panes are despawned |
| a process entity | the child process is killed (pid confirmed gone) |
| a resource entity (bits `0xFFFF_FFFF`, `0xFFFF_FFFE`, `0xFFFF_FFFD`) | the server aborts |

The fatal case, verbatim:

```
WARN bevy_ecs::resource: Resource entities are not supposed to be despawned.
thread 'main' panicked at bevy_ecs-0.19.1/src/error/handler.rs:130:1:
Encountered an error in command `...remove_by_id...`: Entity despawned:
The entity with ID 1v0 is invalid; its index now has generation 1.
Encountered a panic in system `bevy_app::main_schedule::Main::run_main`!
```

**The request does not have to be accepted.** A batch body containing one watch
is refused with the stock "Streaming can not be used in batch requests", and the
detach still fires, because the registration happens before dispatch. That is
one ordinary POST, refused, that despawns a tab or ends the server. A refused
input leaving state changed is class 8; a tab or a child process disappearing
with no close is class 6.

**Where it goes wrong.** `src/main.rs:191-208`, in the runner's bridge:

```rust
if message.method == "fux.frame+watch"
    && let Some(id) = message.params.as_ref()
        .and_then(|p| p.get("viewer")).and_then(|v| v.as_u64())
        .and_then(Entity::try_from_bits)
{
    // ... on response.closed(): closed.send(id)
}
```

`try_from_bits` only proves the bits could be an entity; it says nothing about
what that entity is. The other end, `src/server.rs:450-454`:

```rust
fn disconnected(mut commands: Commands, closed: Res<Disconnected>) {
    while let Ok(entity) = closed.0.try_recv() {
        commands.entity(entity).try_despawn();
    }
}
```

`try_despawn` is safe only against a *missing* entity. Against a live one of the
wrong kind it does exactly what it is told.

**Smallest input that does NOT break, to bound it.** `fux.frame` without
`+watch` on the same id does nothing: the bridge matches the method name, so no
detach is registered. `world.get_components+watch`, a stock watching method, is
also inert here for the same reason. A watch naming an id that was never
allocated is absorbed, because `try_despawn` finds nothing. Both halves are
required: the method must be `fux.frame+watch`, and the id must name a live
entity.

**Class of inputs the fix must cover.** Any entity id a client can put in a
watch request. The detach must apply to the entity only if it is a viewer, and
the check belongs where the despawn happens, not only where it is registered,
because the world can change in between. The same question applies to every
other place a client-supplied id is turned into an action on an entity rather
than a lookup.

**Fixed in `b698ef0`.** `server::disconnected` despawns the entity only if it
is a viewer, checked where the despawn happens rather than only where the watch
was registered, because the world can change in between: an id can be despawned
and its index reused by an entity of another kind before the connection closes.

The registration had a second problem, fixed at its source: a batch containing
a watch was dispatched and then refused, which opened a response channel that
was dropped at once, which reads as a watcher going away. So one refused POST
detached a real viewer -- a refused request changing the world, the class 8
half. The batch path now refuses a streaming request before dispatching it, and
the reply is byte-for-byte what it was. Closing a watch on a real viewer still
detaches it, which is documented, and is pinned by its own test.

**Reproduction.** `fux-fuzz/repro/003-watch-close-despawns-any-entity.sh <fux>`
(exit 0 reproduced, 1 verified not, 2 setup). Exit 1 against this build;
`NEGATIVE_CONTROL=1` also exits 1. The `identity` scenario now passes and runs
in the default smoke, and its trace moved to
`fux-fuzz/traces/identifier-surfaces.json`:

```
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario identity
```

---

## 004 — Despawning a resource entity through stock BRP aborts the server (class 1)

**The break.** `world.despawn_entity` with bits `0xFFFF_FFFF` despawns entity
index 0, which is a Bevy resource entity. The ECS is left inconsistent and the
next command flush panics, ending the server and every session with it. The same
holds for `0xFFFF_FFFE` and `0xFFFF_FFFD`. One accepted request does it.

**Where it goes wrong.** Not in fux's own code: `bevy_remote`'s
`process_remote_requests` despawns what it is asked to, and `bevy_ecs` panics
afterwards. fux's part is the exposure decision recorded in `README.md` —
"resource/schedule/event/schema methods are not filtered" — which the README
frames as trusted low-level access that "can bypass normal transitions".
Aborting the process is a different thing from bypassing a transition, and no
ordinary caller can tell these ids apart from a viewer id, because `to_bits`
hides the index.

**Smallest input that does NOT break, to bound it.** `world.despawn_entity` on
any ordinary entity, live or already despawned, is absorbed. Only the handful of
resource-entity ids at the top of the index space abort. `world.get_components`
and `world.list_components` on the same ids answer normally.

**Class of inputs the fix must cover.** Every stock method that acts on an
entity the caller names, against the ids of entities that are ECS bookkeeping
rather than fux's model. Either those entities are out of reach of the exposed
API, or the panic they cause is contained so one request cannot end the process.

**Fixed in `9ad3c84`, in two layers.** fux registers its own
`world.despawn_entity` after the stock methods, so it replaces the stock one by
name; it refuses an entity carrying `IsResource` with a typed error, leaves the
world untouched, and otherwise hands the request to the stock handler.
Separately, `ServerPlugin` sets Bevy's `FallbackErrorHandler` to log rather
than panic, so a command that fails reports it instead of ending the process.
Each layer closes this finding on its own: with the guard removed the handler
still contains it, and with the handler removed the guard still refuses it.

Rejected: filtering the method out of the registry, because the README's
promise is that the stock registry is served, and the problem was never that
the method exists but that one request could end the process. Also rejected:
carrying the fix in a fork of bevy, which would pin fux to a git branch of a
pre-release engine for a check fux can make itself. The fix is right upstream
too, and a branch with it exists in gold-silver-copper/bevy, but fux does not
depend on it.

**Reproduction.** `fux-fuzz/repro/004-despawning-a-resource-entity-aborts.sh <fux>`.
`NEGATIVE_CONTROL=1` despawns an ordinary entity and exits 1.

---

## 005 — `world.mutate_components` on a missing entity aborts the server (class 1)

**The break.** `world.mutate_components` naming an entity that does not exist
panics instead of returning an error:

```
thread 'main' panicked at bevy_remote-0.19.1/src/builtin_methods.rs:1194:28:
Entity not yet spawned: The entity with ID 4294954950v0 is not spawned
Encountered a panic in system `bevy_remote::process_remote_requests`!
```

Both a never-allocated id and a despawned one reach it. The despawned one is the
ordinary case rather than an exotic one: a caller reads an id, the entity is
closed underneath it, the caller writes back, and the server dies.

**Where it goes wrong.** `bevy_remote-0.19.1/src/builtin_methods.rs:1194`
resolves the component registration, then calls `world.entity_mut(entity)`
without checking the entity first. The defect is upstream; fux reaches it
because it serves the stock registry unfiltered. Its siblings do check:
`world.get_components`, `world.insert_components` and `world.remove_components`
all refuse the same id with a typed error, so this method is the odd one out.

**Smallest input that does NOT break, to bound it.** The same request against a
live entity is fine, and a bad *component* name or *path* on a live entity is a
typed error. Only a missing entity reaches the panic.

**Class of inputs the fix must cover.** Any stock BRP method that resolves a
caller-named entity with a panicking accessor. Fixing it upstream and taking the
patch version is the cleanest route; until then the panic must not be able to
end the process.

**Fixed in `9ad3c84`.** fux registers its own `world.mutate_components`,
which answers `entity_not_found` for an entity that is not alive -- as the
method's siblings already do -- and otherwise hands the request to the stock
handler. This one needs the guard: the panic is a direct one, so the fallback
error handler never sees it, and with the guard removed the repro script
reproduces again even with the handler in place.

**Reproduction.** `fux-fuzz/repro/005-mutate-components-missing-entity-aborts.sh <fux>`.
`NEGATIVE_CONTROL=1` mutates a live entity and exits 1.

---

## 006 — Descriptor pressure wedges the accept loop (class 2)

**The break.** When `accept` fails with `EMFILE` or `ENFILE`, `transport::serve`
classifies it as transient, logs a warning, sleeps 50 ms and loops. It never
accepts-and-closes to drain the backlog and never sheds a connection. While the
pressure lasts, **no new client can connect at all**: `fux attach`, `fux rpc`
and `fux stop` each need a new connection, and each fails.

Measured with the server under `ulimit -n 64` and one same-user process holding
199 connections open: 39 connection attempts over 15 s, none succeeded, and 184
`BRP socket accept: Too many open files` warnings were written to stderr in that
window — roughly twenty a second, unbounded, on a server whose stderr is
normally redirected to a file. The condition does not clear on its own; it
cleared within 0.0 s of the holder closing its connections.

**Where it goes wrong.** `src/transport.rs:390-440`:

```rust
Err(error) if transient(&error) => {
    bevy_log::warn!("BRP socket accept: {error}");
    Timer::after(Duration::from_millis(50)).await;
}
```

`transient` lists `EMFILE` and `ENFILE` with `ECONNABORTED`, `EINTR` and
`EAGAIN`, but those are not alike: an aborted peer is momentary, while a
descriptor shortage persists until something releases descriptors, and retrying
the same `accept` cannot release any.

**Smallest input that does NOT break, to bound it.** The same connections opened
and closed immediately never wedge anything: the negative control does exactly
that and the server stays reachable throughout. A burst of connects that
overflows the 128-deep listen backlog gets `ECONNREFUSED` per connection and
also recovers by itself. The wedge needs descriptors to be *held*.

The reachability depends on the server's descriptor limit, which this script
supplies with `ulimit -n 64` so the case is quick and bounded. It is not
artificial: macOS `launchctl limit maxfiles` is 256 by default, so a server
started from a launchd context wedges at roughly two hundred connections, and
`ENFILE` is machine-wide and needs no limit at all.

**Class of inputs the fix must cover.** Any accept failure that persists rather
than passes. A descriptor shortage needs a different response from a momentary
error: drain the backlog so clients get a prompt refusal instead of silence,
rate-limit the warning, and surface the condition rather than logging it twenty
times a second.

**Fixed in `84202fb`.** `EMFILE` and `ENFILE` are no longer classed with
`ECONNABORTED`, `EINTR` and `EAGAIN`. A momentary error is logged at debug and
retried. A shortage is reported once at error level and then at most every five
seconds, and the loop spends a descriptor reserved at startup to accept and
immediately close one waiting connection, so the backlog drains, a client fails
promptly instead of waiting on a listener that cannot answer, and the loop makes
progress rather than spinning. Recovery is reported when accept succeeds again.

What it does not do: a server with no descriptors cannot serve a new client, and
nothing here changes that. The README says so. Measured under `ulimit -n 64`
with one process holding 221 connections: before, 168 log lines and a client
waiting 6.3 s; after, 3 log lines, the slowest client failure 3.0 s, the
condition reported, and recovery in 0.0 s once the connections were released.
The repro script now measures that behaviour rather than mere reachability, and
still reports the old loop as reproduced.

**Reproduction.** `fux-fuzz/repro/006-descriptor-pressure-wedges-the-accept-loop.sh <fux>`.
`NEGATIVE_CONTROL=1` closes each connection immediately and exits 1. The
`ulimit` change applies only to the subshell that execs the server.

---

## 007 — The request body has no size limit (class 5)

**The break.** `transport::batch` reads the whole request body into memory
before looking at it, with no limit in hyper's builder or in fux. One connection
grew the server by **580 MB for a 256 MB body** — about 2.2x, because the bytes
are collected, parsed into a `serde_json::Value`, and on the error path copied
again into the JSON-RPC message that echoes the offending text.

Time is strongly superlinear: 16 MB is answered in about 2 s, 32 MB in 7 s,
128 MB in 237 s, 192 MB in 374 s. Other clients keep being served throughout and
attached viewers keep painting, so this is growth and latency rather than a
stall; the server does not refuse, does not stream and does not cap.

**The same absence on the way out.** A batch is answered in full before
anything is written, so a small request buys a large response: a 478 KiB body
holding 10 000 `rpc.discover` requests was answered with **12.49 MB** in 2.7 s,
and took the server from 46 MB to 278 MB. That is 26x amplification from a
request that fits in a single write, and it needs no large body at all, so a
body limit alone would not bound it.

**Where it goes wrong.** `src/transport.rs:525`:

```rust
let body = match request.into_body().collect().await {
```

The stock `bevy_remote` loop this was modelled on has the same shape, so the
absence of a limit came across with it; the difference is that fux now owns the
line and can bound it.

**Smallest input that does NOT break, to bound it.** An ordinary request leaves
RSS flat (the negative control sends 1 MB and measures under 64 MB of growth).
Bodies up to a few megabytes are answered immediately. There is no threshold in
the code: the cost is proportional to what the caller sends, and not to how fast
they send it — 64 MB dribbled in 256 KiB steps over 27 s reached 193 MB of RSS
just the same, with other clients served throughout.

**Class of inputs the fix must cover.** Any request whose size the caller
chooses, and any response whose size follows from it. A byte limit on the body,
refused with a typed error, bounds the body and the error-echo amplification
together; the batch case additionally needs a cap on the number of requests in
one batch, or a response that is written as it is produced. The limits must stay
above the largest legitimate request, which is a `load_layout` scene or a 64 KiB
paste, not a megabyte.

**Fixed in `df88f51`, with three bounds, because they catch three different
things.** `MAX_BODY` (4 MiB) applied with `http_body_util::Limited` and refused
with a typed error naming it; `MAX_BATCH` (1024 requests); and
`MAX_BATCH_RESPONSE` (8 MiB), after which the remaining requests in that batch
are answered with an error instead of being run, since a thousand
`registry.schema` calls would otherwise answer with 120 MB. Responses are
serialized as they are produced, so the reply can be counted without serializing
anything twice.

The numbers are measured against the largest legitimate request rather than
picked: a paste is bounded by `paste::LIMIT` at 64 KiB, about 400 KiB once every
byte needs a six-character JSON escape; a one-megabyte name through `rename` or
a raw `Name` insert is about 1 MiB; a `load_layout` names a path rather than
carrying the scene, and the largest scene here is 62 KiB on disk; fux's own
clients send no batches, and the harness's largest is 1000 requests.

Measured after: a 256 MB body grows the server by 0 MB and is refused by its
size, where before it grew it by 580 MB.

**Reproduction.** `fux-fuzz/repro/007-request-body-is-unbounded.sh <fux>`, which
stops at 4 GB of RSS so it cannot pressure the machine. `NEGATIVE_CONTROL=1`
sends a 1 MB body and exits 1.

---

## 008 — The frontend buffers an unterminated SSE line without bound (class 5)

**The break.** `fux attach` reads the `fux.frame+watch` stream with
`BufReader::read_line` into a `String`. Server-sent events are newline framed,
so a peer that never sends a newline makes the frontend buffer the whole stream.
Resident memory climbed at roughly 95 MB/s and reached **5.3 GB in 60 s** before
the peer stopped; the repro script reaches 1.5 GB in about 15 s and stops there.
The frontend is not painting during this: it is accumulating one line.

**Where it goes wrong.** `src/viewer.rs:150-165`: the stream reader loops on
`reader.read_line(&mut line)`, and nothing caps the line, the decoded frame, or
`Frame.paint`.

**Smallest input that does NOT break, to bound it.** The same byte volume sent
as ordinary newline-terminated frames leaves the frontend flat: the negative
control sends 1.6 GB that way and measures 0 MB of growth. So the volume is not
the problem and the missing line bound is.

**Reachability, stated plainly.** The peer here is a small socket server in the
repro script, never fux. It is reached the way any peer is: `FUX_SOCKET` names a
socket, and `transport::check_client_socket` is satisfied by any socket the user
owns in a private directory, which any same-user process can create. That caller
is already trusted with the whole API, so this is not a privilege finding. It is
that the frontend has no defence against a stream that does not end, from a
server that is wedged, buggy, or replaced.

**Class of inputs the fix must cover.** Any stream the frontend reads: a line, a
frame and a paint each need a bound, and passing it must end the attachment with
a message rather than growing until the machine notices.

**Fixed in `47bcc88`.** Each event is read through `MAX_EVENT` (64 MiB), and
passing it ends the attachment with a message naming the bound and the terminal
restored. Because an event is one line of JSON, bounding the line bounds the
frame and the paint inside it.

The bound is measured: an idle 4096x4096 viewer paints about 9 KB, because a
frame carries content rather than every cell, and the worst legitimate case -- a
4096x4096 viewer showing a pane that changes colour every cell -- serializes to
11.6 MB. 64 MiB is about five times that.

Measured after: the same peer that reached 5.3 GB now takes the frontend to 39
MB before it stops on its own.

**Reproduction.** `fux-fuzz/repro/008-frontend-sse-line-is-unbounded.sh <fux>`,
bounded to 1500 MB and 25 s. `NEGATIVE_CONTROL=1` sends the same volume as
proper frames and exits 1.

---

## What did not break (coverage, not findings)

| Area | Inputs tried | Correctly refused / absorbed | Breaks |
| --- | ---: | ---: | ---: |
| 1 the owned HTTP transport: parsing, framing, pipelining | 32 | 32 | 0 |
| 1 the transport under resource pressure | 24 | 22 | 2 (006, 007) |
| 2 socket location, lock and lifecycle | 65 | 65 | 0 |
| 2 the client against a hostile peer | 18 | 17 | 1 (008) |
| 3 caller-chosen identifiers: 29 surfaces x 20 values, both builds | 1160 | 1140 | 3 (003, 004, 005) |
| 4 panics the lints do not catch, both builds | 14 | 14 | 0 |
| 5 the hunt 5 guards under pressure | 6 | 6 | 0 |
| 5 memory scaling: viewers, and attach/detach churn | 2 | 2 | 0 |

The identifier row counts every (surface, value) pair: 20 of the 1160 pairs
broke, and they fall into the three root causes above. The lifecycle row counts
the thirty simultaneous-start rounds and the ten kill-window restarts
individually, because each is a separate timing attempt.

Highlights of the non-findings, because they bound the findings above:

- **Transport parsing.** A GET and an OPTIONS with no body, HTTP/1.0, a path
  that is not `/`, a missing `Host`, `Expect: 100-continue`, `Content-Length`
  with `Transfer-Encoding` in both orders, a malformed chunk size, a
  `Content-Length` longer and shorter than the body, a negative one, invalid
  UTF-8 in the body, the target and a header value, NUL bytes in headers, bare
  LF line endings, 2 KiB of binary garbage, 10 000 headers, a 1 MiB header, a
  1 MiB target, an empty body, `null`, a number, `[]`, unparsable JSON, a
  request with no method, an object id, a 1 MiB string id, a deeply nested id,
  params nested 10 000 deep, a 1000-request batch and ten pipelined requests:
  every one answered with a typed JSON-RPC error, answered normally, or was
  refused by hyper before fux saw it, with the next request on a fresh
  connection always succeeding. This is the `transport` scenario, now in the
  default smoke.
- **Transport under pressure, beyond the two findings.** Headers dribbled one
  byte per second were held open below hyper's 30 s header timeout and closed at
  30.6 s above it, exactly as configured. A batch of 1000 watches was refused
  with the stock streaming error in milliseconds. A watch on each of 331 viewers
  (the client ran out of descriptors first) left the server healthy, and closing
  them all returned every descriptor. A watch whose reader consumed one byte per
  second while its viewer was resized 68 800 times held RSS flat at 822 MB and
  fds at 16 for 45 s: the 8-slot result channel is real backpressure, and the
  slow consumer neither grew the server nor blocked the others. `SIGSTOP` with
  20 clients mid-request, then `SIGCONT`, answered 20 of 20. 100 000
  connect/close cycles finished in 1.5 s with descriptors, threads and RSS flat
  30 s later; 19 374 of them were refused by the 128-deep backlog, which is the
  kernel's answer to a burst, not fux's.
- **Memory scaling.** Each concurrent viewer costs about 1.58 MB, linearly and
  with no cap on how many a caller may attach: 300 viewers took the server from
  47 MB to 521 MB. Detaching all of them returned the viewer count to 0 but left
  RSS at 522 MB, which is the allocator keeping the high-water mark rather than a
  leak: 2190 attach/detach cycles over 90 s, never more than two viewers at once,
  held RSS flat at 50 MB and returned to 50 MB after 20 s idle. Bounded input,
  bounded memory, so this is coverage and not a finding.
- **Socket lifecycle.** `FUX_SOCKET` as `/`, a relative path, a trailing slash,
  `..` components, 205 bytes and empty; a lockfile replaced by a directory, a
  FIFO or a symlink; the socket directory chmodded to 0755, renamed or removed
  while the server ran; the socket file removed or replaced by a regular file;
  the lockfile removed underneath a running server; a read-only parent. Every
  one was refused with a message naming the cause, and nothing that already
  existed was modified. A start while the owner was `SIGSTOP`ped was refused by
  the lock, left the socket's inode unchanged, and the stopped server resumed
  serving on `SIGCONT` — the stale-socket probe did not unlink a live socket.
  Thirty rounds of two simultaneous starts left exactly one survivor every time.
  A `SIGKILL` at ten different points across the bind/listen window was followed
  by a successful restart on the same path, 10 for 10. `$TMPDIR` changed between
  starting the server and starting a client made the client say "no fux server
  socket at ..." rather than connect to something else. A lockfile held by a
  process that is not fux was refused (the message says "another fux server",
  which is a wording nit rather than a break). A socket directory on a full
  filesystem was refused with "No space left on device", naming the path. A
  `FUX_SOCKET` holding non-UTF-8 bytes was refused with "FUX_SOCKET is not valid
  UTF-8". A `SIGKILL` delivered at four points *during* graceful cleanup left the
  socket file behind every time, as a `SIGKILL` must, and the next server
  recovered the path through the stale-socket probe every time.
- **The client against a hostile peer.** `fux rpc` against a peer that accepts
  and never answers, answers garbage, sends an endless SSE line, sends a 100 MB
  frame, closes mid-frame, or dribbles one byte per second: bounded every time,
  either by the 10 s global timeout or by an immediate parse error, and never a
  hang. `fux attach` against the same peers exits non-zero before entering raw
  mode, so the outer terminal is never touched.
- **Identifiers.** 29 surfaces — `fux.frame`, `fux.frame+watch`, the `Control`
  and `UserInput` event targets, ten command subjects, both `load_layout`
  mapping positions, five entity-typed component fields, `Children` with a
  duplicated id, `Overlay.target`, and six stock methods — against 20 value
  classes each, on debug and release. Everything except the three findings above
  was refused with a typed error or absorbed with the world still consistent.
  Notably, every reflected component field is safe: `Entity`'s `Deserialize`
  goes through `try_from_bits`, so a bad id fails to deserialize rather than
  reaching the world.
- **Panics the lints do not catch.** An overlay open with its relationships
  removed underneath and Enter pressed; a rename overlay whose target is
  despawned mid-edit; copy mode with the viewer resized to 0x0 and then to
  4096x4096 with `scrollback` at `u64::MAX`; `ProcessState` rows and cols at 0
  and 65535; `Viewer.scrollback` near `u64::MAX` followed by scrolling both
  ways; `Prefix.scroll` at `u64::MAX` followed by every arrow key;
  `Overlay.mode.List.selected` at `u64::MAX` followed by Enter. All absorbed.
  **No debug/release difference was observed anywhere in this hunt**, including
  the arithmetic cases, which is the interesting part: debug builds panic on
  overflow and release builds wrap, and neither happened.
- **The hunt 5 guards.** A viewer carrying an open overlay and a live selection
  was made a `Tab`, a `Workspace`, a `Split` and a `PaneView` in turn: the
  layout role won each time and the server stayed healthy. One entity spawned
  with every layout role and `Viewer` at once was normalized. 300 rounds of
  relationship churn did not reach repair's sixteen-pass bound, so its warning
  was not observed to be reachable from raw mutation alone.

## The identifier surfaces, and what each one did

Area 3 sent all 20 value classes at each of these 29 surfaces, on both builds.
"refused" means a typed JSON-RPC error or a documented notice; "absorbed" means
the request was applied and the world stayed consistent.

| Surface | Verdict |
| --- | --- |
| `fux.frame` params.viewer | refused: invalid-params naming the id |
| `fux.frame+watch` params.viewer | **BREAK 003** on entity indices 0, 1, 2 |
| `Control` event target `.viewer` | refused or absorbed |
| `UserInput` event target `.viewer` | refused or absorbed |
| `close {pane}`, `close {tab}`, `close {workspace}` | refused: "target no longer exists" or a wrong-kind notice |
| `focus {pane}` | refused or absorbed |
| `rename {pane}` | refused or absorbed |
| `select {scope, entity}` | refused or absorbed |
| `swap {pane}` | refused or absorbed |
| `move {to: tab}` | refused or absorbed |
| `load_layout {workspace}` | refused or absorbed |
| `load_layout` mapping, old id | refused or absorbed |
| `load_layout` mapping, new id | refused or absorbed |
| `Viewing`, `OnTab`, `Focused` inserted on a viewer | refused at deserialization, or repaired |
| `PaneView.pane` inserted on a viewer | refused at deserialization, or repaired |
| `ChildOf` inserted on a viewer | refused, or normalized |
| `Children` with a duplicated id | refused, or normalized |
| `Overlay.target.leaf` | refused or absorbed |
| `world.get_components` entity | refused: typed error |
| `world.list_components` entity | refused: typed error |
| `world.despawn_entity` entity | **BREAK 004** on entity indices 0, 1, 2 |
| `world.remove_components` entity | refused: typed error |
| `world.mutate_components` entity | **BREAK 005** on any missing entity |
| `world.reparent_entities` parent, and child | refused: typed error |

The pattern worth keeping: every surface that takes an entity id through a
reflected component field is safe, because `Entity`'s `Deserialize` goes
through `try_from_bits` and a bad id never reaches the world. The three that
broke are the three that take an id and *act* on the entity — stream to it,
despawn it, or mutate it — rather than looking it up.

## Not run this hunt, and what would differ on Linux

macOS arm64 only, as hunt 5 was. Nothing here was run on Linux, and these are
the specific places where the result should be expected to differ:

- **`sun_path` is 108 bytes on Linux, 104 on macOS.** fux takes the limit from
  `libc` rather than hardcoding it, so the path-length refusals in area 2 will
  refuse at a different length. The 205-byte value used here is over both.
- **`$XDG_RUNTIME_DIR` is normally set on Linux and normally unset on macOS.**
  Every default-path case here therefore resolved through `$TMPDIR`, which on
  macOS is already per-user under `/var/folders`. On Linux the default lands in
  `/run/user/$UID`, whose ownership and mode come from the system rather than
  from fux, so `private_directory`'s checks meet a directory it did not create.
- **`flock` semantics.** The ownership lock is advisory `flock` on a lockfile.
  Linux `flock` follows the open file description the same way, but the
  interaction with a `/run/user` tmpfs and with `noexec`/`nosuid` mounts was not
  exercised here.
- **`lsof` output differs**, and the `-U`/`-i` selectors behave differently, so
  the descriptor accounting in area 1 and the no-TCP assertion in fux's own
  integration test would need checking rather than assuming.
- **`EMFILE` reachability.** The default soft descriptor limit differs
  (`launchctl limit maxfiles` is 256 on macOS; Linux distributions commonly set
  1024 or far higher), so finding 006 needs a different number of held
  connections, not a different mechanism.
- **PTY and process-group behaviour** in the frontend cases, which hunt 5
  already flagged as macOS-specific in `src/terminal.rs`.

## Harness mistakes (not findings)

- An early "the server stops accepting at 450 connections" was a measurement
  artifact: connections were being opened faster than the accept loop drained
  the 128-deep backlog, so the kernel refused the overflow. Opening them at a
  slower rate accepts all of them. The real accept-loop finding (006) needs
  descriptors to be exhausted, not the backlog.
- Several early "no reply within the timeout" results were the harness's own
  `Content-Length` arithmetic being two bytes longer than the body it sent, so
  the server was correctly waiting for the rest of the request.
- An early "`fux attach` never exits" was the harness reading the PTY to EOF and
  then not reaping the child; and an early "the frontend never enters raw mode"
  was the harness forgetting to set a window size on the PTY it allocated, so
  `fux attach` exited with its documented ioctl message before touching the
  terminal.

## Ranked fixes for the hardening PR

> Superseded: all four were made, as described under "Fixed" in each finding.
> Two landed differently from the sketch below. The stock-method aborts (2) are
> guarded in fux rather than fixed in bevy: fux replaces the two methods by name
> with checks that then call the stock handlers, and additionally logs a failed
> command rather than panicking on it, which contains that class whether or not
> a given method is guarded. The
> descriptor-pressure work (3) added the reserved descriptor the sketch treated
> as optional, and the repro script's criteria were rewritten around what the fix
> can actually guarantee, because a server with no descriptors cannot serve a
> client however it is written.

1. **Class 1 — a client-named id must not select an entity to destroy.**
   Make the watch detach apply only to an entity that is a viewer, checked where
   the despawn happens. This closes the whole of finding 003, including its
   class 8 and class 6 halves. (Finding 003.)
2. **Class 1 — one request must not be able to end the process.** Two doors are
   open: despawning ECS bookkeeping entities through stock methods (004), and a
   stock method that panics on a missing entity (005). Both are reached with a
   single accepted request, and neither is distinguishable from ordinary use by
   the caller. Fix 005 upstream and take the patch; decide deliberately whether
   resource entities are in reach of the exposed API at all. (Findings 004, 005.)
3. **Class 2 — an accept failure that persists needs a different response from
   one that passes.** Drain the backlog under descriptor pressure so clients are
   refused promptly instead of silently, and rate-limit the warning so stderr
   does not grow without bound. (Finding 006.)
4. **Class 5 — bound what a caller can make either side hold.** A byte limit on
   the request body, refused with a typed error (007), and a bound on the
   frontend's SSE line, frame and paint, which ends the attachment with a
   message rather than growing (008). (Findings 007, 008.)

---

# Where fux breaks under hostile input (hunt 7)

> **Status: fixed in the hunt 8 branch, except 011.** Each finding's "Fixed"
> line names what closed it; 009, 010, 012 and 013 are closed with tests that
> failed first, and 011 is the documented exception (the ALSA chain is
> `bevy_remote`'s, not fux's to cut). The analysis below is the state when the
> findings were made.

This hunt attacked what changed after hunt 6: the guards that replaced two
stock BRP methods, the UI hit test rebuilt on `bevy_picking` when
`bevy_ui::Interaction` became unusable in Bevy 0.20, the four new size limits,
and Linux, which no hunt had run on and which fux 0.12.0 is nonetheless
published for.

Severity classes are hunt 5's, unchanged. Two additions for this run: a
behaviour difference between macOS and Linux that the README does not state,
and a limit that refuses a legitimate workload.

Linux runs are in a container built by `fux-fuzz/linux/Dockerfile`, driven by
`fux-fuzz/linux/run.sh`: Debian 12, glibc 2.36, Rust 1.98.1, kernel 7.0.14
(OrbStack), `aarch64` and `x86_64`, as an ordinary user (uid 1000), because
root ignores the socket permissions that are fux's access control.

## 009 — A `Viewer` on a resource entity ends the server, around both guards (class 1)

**The break.** Two accepted requests and a closed connection stop the server,
taking every attached session and child process with it. No guarded method is
called:

1. `world.insert_components` puts a `fux::model::Viewer` on a **resource
   entity**. Nothing refuses it.
2. `fux.frame+watch` on that id, then close the connection. `disconnected`
   despawns it, because by now it really is a viewer.

The despawn lands on a resource entity, which is exactly what hunt 6's finding
004 guard exists to prevent:

```
WARN bevy_ecs::resource: Resource entities are not supposed to be despawned.
ERROR bevy_ecs::error::handler: Encountered an error in system
  `bevy_app::main_schedule::Main::run_main`: System panicked
resource does not exist: bevy_app::main_schedule::MainScheduleOrder
```

**Why the guards do not cover it.** The guard is on the *method*:
`remote::despawn_entity` refuses an entity carrying `IsResource`. fux despawns
viewers in its own code, and those paths never ask:

- `layout`/`server::disconnected` despawns the entity a closed watch named,
  after checking `IsViewer`, which is `(With<Viewer>, NotLayout)`;
- `execute`'s `Detach` despawns the viewer the command named.

Both check that the entity is a viewer. Neither checks that it is not a
resource, and step 1 makes a resource entity pass the viewer check. The hunt 5
finding 001 normalization that strips a `Viewer` off a layout node knows about
`Workspace`, `Tab`, `Split` and `PaneView`; a resource entity is none of them.

**Which resource decides what happens.** `Entity::to_bits` complements the
index, so resource entities are a short fixed sequence from `0xFFFFFFFF`
downwards and a caller needs no guesswork. On this build:

| Bits | Index | Resource | Result |
| --- | --- | --- | --- |
| `0xFFFFFFFF` | 0 | `DefaultQueryFilters` | survives; no visible damage |
| `0xFFFFFFFE` | 1 | `Schedules` | **stops answering** |
| `0xFFFFFFFD` | 2 | `AppTypeRegistry` | **answers every request with an error**: alive, useless, still holding every session's PTYs |
| `0xFFFFFFFC` | 3 | `MainScheduleOrder` | **stops answering** |
| `0xFFFFFFFB` | 4 | `FixedMainScheduleOrder` | survives |
| `0xFFFFFFFA` | 5 | `Messages<AppExit>` | survives |

Index 2 is worth its own line: the server neither exits nor serves. A liveness
check that only asks whether the socket answers would call it healthy.

**What the error handler does and does not buy.** `ServerPlugin` logs rather
than panicking on a failed command, which is why the `Detach` route leaves the
process running where it would otherwise abort. It does not stop the world
from being wrong: the resource is gone either way, and what follows is a
server that cannot run its main schedule.

**Fixed** in the hunt 8 branch, as the class: `reject_viewer_on_layout` strips
fux components off a resource entity as off a layout node, `IsViewer` excludes
`IsResource`, and `navigation::detach_if_viewer` is the one checked path both
`disconnected` and `Detach` despawn through. Repro now exits 1 on both.

**Reproduction.**
`fux-fuzz/repro/009-viewer-on-a-resource-entity-ends-the-server.sh`, exit 0
reproduced, exit 1 verified not reproduced, exit 2 setup failure;
`NEGATIVE_CONTROL=1` puts the same `Viewer` on an ordinary spawned entity and
watches that, which must leave the server serving. Reproduced on macOS arm64
and Linux arm64.

**The class, not the instance.** The guard on the method is not where this
belongs. Every despawn of a caller-named entity needs the check, including
fux's own: `disconnected`, `Detach`, and anything later that despawns what a
request named. A fix that only adds `IsResource` to `IsViewer` closes this
script and leaves the class open.

## 010 — A signal ends a request, and the attachment with it (class 2, Linux)

**The break.** Resizing the terminal window while a key is in flight ends the
attachment:

```
fux: io: Interrupted system call (os error 4)
```

The user is returned to their shell mid-session. The server is fine and the
panes keep running, so nothing is lost but the session; the point is that
resizing a window is not a hostile act, and it is the one interaction
guaranteed to raise a signal.

**Where it comes from.** `UnixTransport::await_input` in `src/unix_http.rs`
does one `read` and treats anything that is not `WouldBlock` or `TimedOut` as
a transport error:

```rust
let amount = timed(self.stream.read(input), &timeout)?;
```

A read on a socket with `SO_RCVTIMEO` set is not restarted after a signal
handler runs: it fails with `EINTR` (signal(7), under "Interruption of system
calls and library functions by signal handlers"). fux sets a read timeout on
every request, because `agent()` takes a whole-call budget. The frontend
installs a `SIGWINCH` handler through `signal-hook` and sends every key, paste
and resize through that transport on its main thread, so the two meet.

macOS restarts the read, so nothing happens there. Neither behaviour is in the
README, and the retry that would fix it is three lines: `EINTR` is not a
failed request, it is a read to repeat.

**How wide the window is.** On Linux `aarch64`, a frontend being resized while
typing lasted 3 to 13 rounds before it died, over three runs. On Linux
`x86_64`, where emulation widens the window, **25 to 27 of fux's 56 integration
tests fail**, every one of them on `EINTR` from this transport, which fux's own
tests use as their client. macOS survived 7278 rounds of the same script and
8104 of an earlier variant.

This is not a regression from this PR: `origin/main` fails the same way on
Linux `x86_64`, with the same error on the same methods.

**Fixed** in the hunt 8 branch: `uninterrupted` in `src/unix_http.rs` retries
a read the signal interrupted; the integration tests pass on emulated x86_64
and the repro exits 1 on Linux.

**Reproduction.**
`fux-fuzz/repro/010-a-signal-ends-a-request-and-the-attachment.sh`, which
drives a real `fux attach` in a pty and resizes it. Exit 0 reproduced, 1
verified not reproduced, 2 setup failure or not Linux;
`NEGATIVE_CONTROL=1` types for the same twenty seconds without resizing and
must survive, which it does: 6347 rounds.

**It is also why the Linux smoke has three failures.** `resize` and
`adversarial` fail with "premature frontend exit" or "viewer disappeared while
resizing", and `origin/main` fails the same two scenarios on the same
platform. The harness drives real frontends and resizes them, which is exactly
the collision above.

The third was not the signal finding and not a flake. The `history` scenario,
and the `owned-terminal-tiny-child-geometry` trace that replays it, wait for a
freshly split pane -- running a program that prints more lines than the pane
is tall and ends without a trailing newline -- to paint its last line. Hunt 7
recorded it as a settling race that polling converged on; hunt 8 first called
it that too. It is finding 017 below: the emulator's resize cut the bottom row,
and one frame taken after a fixed wait shows it every time, on both platforms.

## 011 — fux does not build on Linux without ALSA headers (class: platform)

**The break.** `cargo build` fails on a clean Debian 12 with only a Rust
toolchain:

```
error: failed to run custom build command for `alsa-sys v0.4.0`
  pkg-config has not been configured to support cross-compilation...
  Could not run `PKG_CONFIG_ALLOW_SYSTEM_CFLAGS=1 pkg-config --libs --cflags alsa`
```

The chain is entirely transitive:

```
fux -> bevy_remote 0.20.0-rc.1 -> bevy_dev_tools -> bevy_audio -> rodio -> cpal -> alsa-sys
```

`bevy_remote` depends on `bevy_dev_tools` for `schedule_data`, which is what
answers `schedule.list` and `schedule.graph`, and `bevy_dev_tools` pulls
`bevy_audio` unconditionally. fux already sets `default-features = false` on
`bevy_remote` and takes only `bevy_asset`; there is no feature to turn this
off from here.

**Why it matters now.** fux 0.12.0 is published, so `cargo install fux` on
Linux fails at this build script unless `libasound2-dev` and `pkg-config` are
already installed. The released binary links `libasound.so.2`, a sound library
a terminal multiplexer has no use for. On Bevy 0.19.1 the chain did not exist:
`e794df0`'s lockfile has no `alsa-sys`, and `0286346`'s does.

**Not fux's to fix in fux, confirmed.** `bevy_dev_tools` is an unconditional
dependency of `bevy_remote`, and `bevy_audio` an unconditional dependency of
`bevy_dev_tools`, so no feature selection in fux's manifest removes the chain.
This is the run's one open finding: the README states the two packages a Linux
build needs, CI installs them, and the upstream issue text is in the PR. Repro
011 stays at exit 0.

**Reproduction.** `fux-fuzz/repro/011-a-linux-build-needs-alsa.sh`, which asks
the resolved dependency graph for a Linux target rather than building, so it
runs anywhere in a second and prints the whole chain. Exit 0 reproduced, 1
verified not reproduced, 2 setup failure; `NEGATIVE_CONTROL=1` resolves the
same graph for a macOS target, where the chain is absent. The consequence, a
build that fails without the headers, is `fux-fuzz/linux/run.sh arm64 cargo
build --locked` with the `libasound2-dev` line removed from
`fux-fuzz/linux/Dockerfile`; the Dockerfile carries that line with a comment
pointing here, so the workaround is visible rather than silent.

## 012 — Descriptor pressure makes a Linux client wait 6.5 s, not 3.0 (class 2)

**The break.** Hunt 6's finding 006 repro, run on Linux, reproduces:

```
attempts=21 answered=0 slowest=6.54s; log lines=3, condition reports=3
REPRODUCED: a client waited 6.5s on a listener that could not answer
```

The 006 fix is intact and doing its job: 3 log lines rather than 168, the
condition reported once and then at intervals, recovery reported, and every
client after the first failing in 0.01 s. Only the first client is slow, and
on Linux it is slow enough to cross the repro's 5 s threshold.

**Why Linux is worse.** The accept loop spends its reserved descriptor to
accept and close exactly one waiting connection per 50 ms tick:

```rust
if let Some(held) = spare.take() {
    drop(held);
    if let Ok((shed, _)) = listener.get_ref().accept() { drop(shed); }
    spare = reserve();
}
Timer::after(Duration::from_millis(50)).await;
```

Linux holds the backlog (128 by default) and hands connections out in order,
so a new client is behind up to 128 others, each shed one per tick: about
6.4 s, which is what was measured. macOS refuses more of them at `connect`
time, so the queue the new client joins is shorter and it waits about 3.0 s.

**What a fix looks like.** Drain the backlog on each tick rather than one
connection, bounded by the number waiting, so the wait is one tick rather than
one tick per queued connection. The reserved descriptor already makes this
possible; it is only spent once per tick.

**Fixed** in the hunt 8 branch: the loop drains the whole waiting backlog per
tick, so the slowest wait fell to about 0.02 s on Linux and the repro exits 1.

**Reproduction.**
`fux-fuzz/repro/012-descriptor-shedding-drains-one-connection-a-tick.sh`,
which samples fresh clients over a window and reports the worst wait: 6.9 s
in the run recorded here, with 1 of 26 samples over a second and the rest
resolving in about 10 ms. A client that arrives when the backlog is completely
full is refused at `connect` at once; the slow case is the one that gets into
the queue and then waits its turn, which is why it has to be sampled rather
than measured once. Exit 0 reproduced, 1 verified not reproduced, 2 setup
failure or not Linux; `NEGATIVE_CONTROL=1` opens and closes the same
connections, leaving no pressure.

Hunt 6's `006` script, unchanged, also exits 0 on Linux and 1 on macOS. The
default soft `ulimit -n` in the container is 20480; both scripts set 64 for
the server's own subshell, so the limit under test is fux's, not the
machine's.

## 013 — `terminate` leaves background jobs alive under `dash` (class 3)

**The break.** With `/bin/sh` as the pane program, a background job survives
the pane that owns it:

```
shell 27 bg 29 shell pgid 27 bg pgid 29
after terminate: shell alive False background alive True
```

`sleep 60 &` started from the pane's shell is still running after the pane is
terminated, in its own process group, with no terminal.

**Why.** `Job::finish` sends `SIGHUP` to the pane's process group, waits up to
100 ms for the shell to hang up its own jobs, then `SIGKILL`s that group. A
background job started by an interactive shell is in a *different* process
group, so neither signal reaches it; it dies only if the shell forwards the
hangup. `bash` and `zsh` do. `dash` does not, and `/bin/sh` is `dash` on
Debian and Ubuntu, where it is also the default for `sh`-based configurations.

**How it shows up.** fux's own integration test
`interactive_background_jobs_hang_up_when_pane_terminates` fails on Linux, 4
runs out of 4, and on `origin/main` equally: the test configures `/bin/sh`,
which is `dash` there and `bash` on macOS. It is the test noticing a real
difference, not a flake.

**Already known, in one place.** `fux-fuzz/README.md` says under "Bounds and
cleanup" that "Ubuntu's dash `/bin/sh` does not provide bash's background-job
SIGHUP propagation", and the harness fixture execs `/bin/bash --noprofile
--norc -i` to avoid it. So the harness knows; fux's integration test does not,
and configures `/bin/sh`. On macOS that is bash and the test passes, which is
why this has never been visible.

**What it is not.** Not a leak of fux's own making: the process is reparented
to init and reachable by the user. The README says a pane's process is
terminated with the pane, and under `dash` a job it started is not.

**Fixed** in the hunt 8 branch: ending a pane hangs up every process in the
pane's session, whichever shell started it; a `nohup` or `setsid` job survives
as it would a closed terminal. The repro exits 1 on both platforms and the
integration test passes on Linux.

**Reproduction.**
`fux-fuzz/repro/013-terminate-leaves-a-dash-background-job.sh`, which names
`/bin/dash` explicitly rather than `/bin/sh`, so it tests the shell rather
than the platform's choice of shell. macOS ships `/bin/dash` too and the
finding reproduces on both, which is what makes it a shell difference rather
than a platform one. Exit 0 reproduced, 1 verified not reproduced, 2 setup
failure; `NEGATIVE_CONTROL=1` runs the same pane under `bash`, which forwards
the hangup, so the job dies with its pane.

Writing that control turned up one more difference worth knowing: bash 5.1 and
later enable bracketed paste, where a newline inside pasted text goes into the
line buffer instead of running it, so the script presses Enter as a key. macOS
ships bash 3.2, which does not, and `dash` does not either.

## What did not break (coverage, not findings)

Clean areas matter as much as breaks: they say where the next hunt need not
look. Everything here was attacked and behaved.

### The picking migration (area 2)

`bevy_ui::Interaction` became unusable in 0.20 and the hit test was rebuilt on
the stock picking backend over one synthetic `PointerId::Mouse`. Nothing found:

| Attacked | Result |
| --- | --- |
| Click position against painted content, swept over a grid | 201 cells across two split layouts, every one focused the pane the frame painted |
| An overlay over a pane | A click under an open menu changed no focus and left the overlay open |
| Two viewers of different sizes, clicking alternately | 6 rounds, neither viewer's focus moved when the other clicked; the shared pointer leaked nothing |
| A zoomed pane | A click anywhere kept the zoomed pane focused |
| Coordinates at and past the edge: `(79,22)`, `(80,23)`, `(200,200)`, `u16::MAX`, `(0,0)` | All answered without error, no state change |
| One-cell and tiny viewers: 1x1, 2x2, 1x80 | Clicks accepted, frames painted, server fine |
| A viewer detaching mid-drag | The other viewer kept focusing and painting |
| A resize between press and release | The release outside the shrunken viewer was absorbed |
| A layout save and load between press and release | Focus and painting intact |
| 12 rounds of split, click, close, click with no settling | No death, frames still painted |

### The guards and the entity surface (area 3)

| Attacked | Result |
| --- | --- |
| 9 entity-taking methods x 17 id classes, singly and in batches | 306 requests; the server survived every one |
| `world.despawn_entity` on a resource entity, alone and in a batch | Refused, `-23501`, world untouched |
| `world.mutate_components` on a despawned id | `entity_not_found`, as its siblings answer |
| Inserting a component onto a resource entity | Accepted (this is finding 009's first step) |
| Reparenting a resource entity under a tab, and a pane view under a resource entity | Accepted, no visible damage, server fine |
| `ChildOf` naming a resource entity at spawn | Accepted, server fine |
| Mutating `Settings` through `world.mutate_resources` | Accepted, server fine |
| Removing the `Settings` resource | Server survives; frames stop painting. Raw resource removal is trusted low-level access, and this is what the README means by it |
| Check-then-act between a guard and the stock handler | No window found: the guards resolve the entity and hand over inside one exclusive world access |

### The limits (area 4)

| Limit | At the boundary | Bypass attempted | Legitimate workload |
| --- | --- | --- | --- |
| `MAX_BODY` 4 MiB | 4194303 and 4194304 accepted, 4194305 refused with the limit named | Chunked transfer: cut off at exactly the limit, connection closed | A 1 MiB name and a 400 KiB escaped paste both fit |
| `MAX_BATCH` 1024 | 1024 accepted, 1025 refused naming the count | Pipelining 50 requests on one connection: each answered separately, none pooled into one reply | 200 `world.query` in a batch: 23 KB |
| `MAX_BATCH_RESPONSE` 8 MiB | 70 `registry.schema` = 7.84 MB accepted; at 100 the reply stops at 8.40 MB | — | An agent's plausible batches are three orders of magnitude below it |
| `MAX_EVENT` 64 MiB | Not re-measured this hunt; hunt 6's measurement stands | — | — |

A single large reply is not capped by `MAX_BATCH_RESPONSE`, by design: a
4096x4096 `fux.frame` is answered whole, inside a batch or not.

### Linux, beyond the findings (area 1)

| Attacked | Result |
| --- | --- |
| Socket path length | 107 bytes accepted, 108 refused naming the limit; fux takes it from `libc` rather than assuming macOS's 104 |
| `$XDG_RUNTIME_DIR` missing, root-owned 0755, another user's 0700, world-writable 0777, world-writable parent | Each refused with the reason; none served |
| `$XDG_RUNTIME_DIR` on `/tmp` (1777 sticky), `/dev/shm`, `/run/user/1000` | Served |
| `--socket /proc/self/fd/0` | Refused: not a socket owned by you |
| A socket on a virtiofs host mount | Served |
| Neither `$XDG_RUNTIME_DIR` nor `$TMPDIR` set | Refused with a clear message, which is right but undocumented |
| Terminal restore after the EINTR exit and after the server was `SIGKILL`ed | Reset emitted, PTY back to cooked mode, both platforms |
| A `sudo` pane terminated | `sudo` relayed the hangup, everything exited, status `exited 129`, server fine |
| Repro scripts 001-008 and 009 | Same verdicts as macOS, except 006 (finding 012) |
| Unit tests, `fmt`, clippy, fux-fuzz's own tests | Green on `aarch64` and `x86_64` |

### The Linux difference the README should state

Two behaviours differ from macOS and neither is documented: a server with
neither `$XDG_RUNTIME_DIR` nor `$TMPDIR` refuses to start (finding 010's
neighbour, harmless but surprising under `su`, `cron` and containers), and the
socket path limit is 107 bytes rather than 103. The README names only the
macOS number.

## Ranked fixes for the next hardening PR

1. **009, and the class under it.** A caller-named entity is despawned by
   fux's own code in at least two places, and neither asks what it is
   despawning. The narrow fix is `IsResource` in `IsViewer`; the fix worth
   making is that every internal despawn of an entity a request named goes
   through one checked path, the way the BRP method already does. The
   normalization that strips a `Viewer` off a layout node is the natural place
   to also strip it off a resource entity.
2. **010.** Retry the read on `EINTR` in `src/unix_http.rs`. It is a read to
   repeat, not a failed request. This is three lines, it unbreaks 25 to 27
   integration tests on Linux `x86_64`, and it stops a window resize from
   ending a session. Do this before anything else if CI is going to run on
   Linux.
3. **012.** Drain the waiting backlog per tick rather than one connection, so
   a first client under descriptor pressure waits one tick instead of one tick
   per queued connection.
4. **011.** Say in the README that a Linux build needs `pkg-config` and
   `libasound2-dev`, and raise the `bevy_dev_tools` to `bevy_audio` dependency
   upstream. fux cannot cut the chain from here.
5. **013.** Decide what fux promises about a pane's background jobs, then
   either make `terminate` reach the whole session (a wider kill, with its own
   risks) or say that a job a shell does not hang up survives its pane.
6. **Documentation**, together: the two Linux differences above, the resource
   entities an agent can see (campaign 06 measured this), and the `dash`
   behaviour from 013.

## Harness mistakes (not findings)

- The first `world.query` probe classified an entity as a resource by its id
  being near the top of the 32-bit space. Every fux entity is near the top:
  `Entity::to_bits` complements the index for all of them. The metric now
  takes its evidence from what the run itself saw carrying `IsResource`.
- The first version of repro 009 sent requests without `Connection: close` and
  read until EOF, so every request timed out against a keep-alive server and
  the script reported a setup failure. The other repro scripts had it right.
- Probe scripts were first written under `target/`, which something on the
  machine cleans; they were rewritten outside it. The tools meant to last are
  in `fux-fuzz/tools/` and `fux-fuzz/linux/`.

---

# Hunt 8: every known defect fixed, then everything hunted until a pass finds nothing

> **Status: complete. Eight findings, all fixed.** 014 (found by CI's first
> run), 015 and 016 (pass 1), 017 and 018 (the first nightly smoke, once CI
> ran it), 019 (pass 3, in 018's own fix), and 020 and 021 (pass 4). Each has
> a repro that reproduced on the unfixed code and a test that failed first;
> each repro now exits 1, and `fux-fuzz/repro/expected.tsv` records it. Pass 2
> found nothing, but the nightly runs had not been read yet; the finishing run
> added passes 3 to 5, and pass 5 found nothing new, so the run stopped there.
> The smoke's other failures were the harness's, and are recorded below with
> their causes, as are the three test mitigations that were replaced.

Hunt 8 runs with CI for the first time (`.github/workflows/ci.yml`:
`ubuntu-24.04` and `macos-15`), and the fixes to hunt 7's findings 009–013 land
in the same branch. The ranked list at the end of hunt 7 is the order they
were taken in.

## 014 — Socket cleanup trusts a reused inode number (class 6), Linux on ext4

**Found by** the first run of CI, before any hunting: fux's own unit test
`transport::tests::cleanup_leaves_a_socket_that_replaced_its_own` failed on
`ubuntu-24.04` with `NotFound`, where it had passed on macOS, in the Linux
container, and on every earlier run.

**The break.** A fux server removes its socket at shutdown only if the file at
the path is still the one it bound, because another program may have replaced
it. It decided that by comparing `(device, inode)` with the pair recorded at
bind. That pair names a file only while its inode is allocated: once the
listener closes, the inode is freed, and ext4 gives a freed inode number to
the next file created. Bound, unlinked and rebound at one path:

| Filesystem | Inode number reused |
| --- | --- |
| ext4 (loop mount, and GitHub's `/tmp`) | 200 of 200 |
| APFS (macOS) | 0 of 200 |
| tmpfs, btrfs, overlayfs (the Linux container) | 0 of 200 each |

So on ext4 a socket bound at fux's path after fux's listener closed carries
fux's old pair, and fux removes it. The stale-socket path in `bind_socket` had
the same weakness between its probe and its removal.

**Reach.** Through the binary it needs a race: the replacement has to land
between the listener closing and the endpoint being dropped during shutdown,
and the lock excludes another fux, so it takes a different program. Rare. But
the unit test states the guarantee, and on the most common Linux filesystem
the guarantee did not hold.

**Reproduction.**
`fux-fuzz/repro/014-socket-cleanup-trusts-a-reused-inode-number.sh` runs that
unit test with `TMPDIR` on a filesystem it has first checked reuses inode
numbers; it exits 2 where none is available. Locally,
`FUX_LINUX_TMP=ext4 fux-fuzz/linux/run.sh` gives the container an ext4
`TMPDIR` like the runner's, which is new in this run for exactly this reason.
`NEGATIVE_CONTROL=1` runs it on `/dev/shm`, a tmpfs, where it passes.

## 015 — Removing the `Settings` resource stops painting, silently (class 6)

**The break.** `world.remove_resources` on `fux::assets::Settings` left a
server that answered `rpc.discover` and `world.query` but painted nothing. fux
reads `Settings` with `World::resource` from about a dozen systems -- the bar,
command execution, spawning a pane -- and that method panics when the resource
is absent, so every `fux.frame` failed inside the fallback error handler,
logged and swallowed, with nothing said to the attached session.

**Found by** the resource-entity sweep carried over from hunt 7's clean-areas
table, which had recorded "removing the Settings resource: server survives;
frames stop painting" without filing it. It is a finding: the README calls raw
resource mutation trusted low-level access, but a server that stops serving
without a notice or a log is a silent failure, not a documented trade-off.

**Fixed** in `remote::remove_resources`: the guard delegates to the stock
handler, then, if `Settings` is now gone, restores it to its default and logs
a warning. Removal stays a real operation for every other resource; only the
one resource fux cannot run without is restored, and the restoration is not
silent.

**Reproduction.**
`fux-fuzz/repro/015-removing-settings-stops-painting-silently.sh` removes
`Settings`, then asks for a frame and checks its chrome is still painted. Exit
0 reproduced, 1 not, 2 setup; `NEGATIVE_CONTROL=1` removes an unrelated
resource, which does not affect painting.

## 016 — A scene file read is unbounded (class 5)

**The break.** `load_layout` read its scene file with `std::fs::read_to_string`,
which reads the whole file into a `String` with no bound. The path is the
caller's choice and an absolute one is accepted, so `load_layout` on
`/dev/zero` grew the server to 1.4 GB in a few seconds on the way to
exhausting memory, and a large regular file did the same; parsing a huge one
also blocks every session on the ECS thread while it runs.

fux already bounds the request body a caller sends over the socket (`MAX_BODY`,
hunt 6 finding 007) for exactly this reason. A scene file read from disk is
another way to make the server hold whatever a caller likes.

**Found by** the scene-hostility sweep of hunt 8's pass 1.

**Fixed** in the 016 commit: `read_scene` refuses a regular file over
`MAX_SCENE` (8 MiB) by its length, and caps the read itself at that many bytes
so a file reporting no length -- a pipe, `/dev/zero` -- cannot grow the buffer
without bound. A saved layout is a hierarchy of nodes, far below the bound;
the round-trip save and load test is unaffected.

**Reproduction.** `fux-fuzz/repro/016-scene-file-read-is-unbounded.sh` loads
`/dev/zero` and watches the server's RSS pass 1 GB. Exit 0 reproduced, 1 not,
2 setup; `NEGATIVE_CONTROL=1` loads a small missing file, refused at once.

## 017 — A resized pane loses its bottom line (class 6)

**The break.** A pane whose output overflows its height and whose last line has
no trailing newline lost that line when the pane was resized smaller. Every new
pane is: its PTY starts at one size and the first frame's `size_terminals`
resizes it to the pane's rectangle. The frame then showed the row above, so the
newest output -- a prompt, a program's final line -- was missing, and no later
frame brought it back, because the row was gone from the emulator.

**Found by** the nightly smoke and traces: the `history` scenario and the
`owned-terminal-tiny-child-geometry` trace wait for a split pane to paint its
last line, and timed out on Linux. Hunts 7 and 8 first recorded it as a
settling race, because frames requested in a polling loop seemed to converge.
One `fux.frame` taken after a fixed wait shows it deterministically: 3 of 3 on
macOS against `origin/main`, and the same on Linux.

**Root cause.** `fux_vt::grid::Grid::resized` kept the first `history + rows`
rows, so a shrink dropped rows off the *bottom* of the live area -- the cursor
line -- instead of scrolling the top into history. `Screen::window`,
`terminal::rows::snapshot` and the frame path were correct: before the resize
the window's last row is the newline-less line, after it the row above.

**Fixed** in `fux-vt`: `resized` reflows around the cursor. A shrink drops rows
below the cursor first and only then scrolls rows above it into history, so a
screen whose content is at the top keeps it and a full screen keeps its bottom
line; a grow pulls rows back from history, as xterm does, so shrinking and
growing again restores the screen. Both cursors move with their rows. The
storage stride covers exactly the rows that become history.

Two wrong versions came first, and each is now pinned by a test. Keeping the
bottom rows regardless of the cursor pushed every new pane's first line into
history -- the shell's banner vanished and 38 smoke cases failed under a CPU
limit -- and taking the stride over every old row kept a narrowed pane's
widest width forever, which doubled the 200-pane `scale` scenario's time.

`fux-vt`'s public API is unchanged, so it stays 0.1.1; the change is to its
resize behaviour. One `fux-vt` test, `resize_rejects_bad_capacity_without_mutating_state`,
had asserted the old behaviour -- after `resize(2, 3)` it expected the upper
two rows, with the cursor's row dropped -- and now expects the cursor's row and
the one above it. koh's 220 library tests pass against the fixed `fux-vt`.

**Tests.** `fux-vt`: `shrink_keeps_the_newline_less_bottom_line`,
`shrink_drops_blank_rows_below_the_cursor_first`,
`grow_restores_rows_a_shrink_scrolled_away` and the grid unit test
`narrowing_live_rows_uses_the_new_width_as_stride`. fux:
`terminal::rows::tests::a_resized_pane_paints_its_newline_less_last_line`
(the `Terminal` and `rows` level) and
`design::a_split_pane_paints_its_newline_less_last_line` (one frame over the
socket). Each failed before its fix.

**Reproduction.** `fux-fuzz/repro/017-a-split-pane-hides-its-last-line.sh`
splits with a program that prints forty lines and then `ENDMARK` without a
newline, waits two seconds, and takes one `fux.frame`. Exit 0 reproduced (no
`ENDMARK`), 1 not, 2 setup; `NEGATIVE_CONTROL=1` ends the output with a
newline and exits 1.

## 018 — A blocking scene read stalls the whole server (class 2)

**The break.** `load_layout` reads its scene file with a blocking
`std::fs::read_to_string` (`read_scene`) inside a task on Bevy's `IoTaskPool`.
That pool is one thread on a two-core machine and at most four on any, and it
also runs the BRP serving loop and every pane's PTY I/O. A load whose file is
slow to produce bytes -- a named pipe, or a large scene -- blocks a pool
thread; enough concurrent such loads block every pool thread; and then the
server answers nothing until the reads finish.

**Found by** the nightly smoke, on a two-core runner: the `race`, `churn` and
`scene_refs` scenarios, which use named pipes to hold a load in flight, timed
out with `timeout: global` on both `ubuntu-24.04` and `macos-15`. Reproduced
under `docker run --cpus 2`, on `origin/main` too: it is pre-existing, from
before the finding-016 bound, which kept the read blocking.

**Fixed** in the 018 commit: `scene_io`'s task runs the blocking read or write
on a dedicated `std::thread` and awaits the result over a channel, so it holds
no pool thread while the file is slow. The serving loop and PTY I/O keep
running. The `race`, `churn` and `scene_refs` scenarios pass under `--cpus 2`.
Those threads needed a bound of their own: finding 019.

**Reproduction.**
`fux-fuzz/repro/018-a-blocking-scene-read-stalls-the-server.sh` opens eight
concurrent `load_layout` requests, each naming a FIFO nothing writes -- more
than the pool's ceiling -- then asks `rpc.discover`. Exit 0 reproduced, 1 not,
2 setup; `NEGATIVE_CONTROL=1` sends harmless requests instead.

## 019 — Scene file threads are unbounded, and the OS limit panics (class 1)

**The break.** Finding 018's fix gave each scene load or save its own thread.
Nothing bounded them, and a read of a named pipe that nobody writes never ends:
each such `load_layout` kept one more thread for the life of the server, even
after a later load on the same viewer replaced the pending task. At the OS's
thread limit -- `RLIMIT_NPROC`, a pids cgroup, systemd's `TasksMax` --
`std::thread::spawn` panicked inside the pool task, and `scene_completions`
then panicked on the main thread every update, polling the dead task ("Task
polled after completion").

**Found by** hunt 8's pass over its own fixes. In a Linux container with
`--pids-limit 512`, 1000 such loads left the server at 508 threads, and its
stderr showed both panics. On macOS (16384 threads a process) 5000 loads left
5019 threads, and the server kept answering. `origin/main` is not affected: it
had no scene threads, and 018 hung it instead.

**Fixed** in the 019 commit: at most `MAX_SCENE_IO` (16) scene file operations
run at once. A request past the bound gets the notice "16 scene files are
already being read or written; try again later" and starts nothing. The thread
starts on the ECS thread through `thread::Builder`, so the OS refusing it is a
notice too, not a panic. Each thread gives its place back when it ends. Pipes
that are never written can still hold all sixteen places until the server
exits: that is the requester's own choice, and the server keeps answering.

**Test.** `layout::tests::scene_file_threads_are_bounded` starts 17 loads of
unwritten pipes and requires 16 held, the 17th refused with the notice and
nothing pending, and every place back once the pipes are released. It failed
first: all 17 were held.

**Reproduction.** `fux-fuzz/repro/019-scene-file-threads-are-unbounded.sh`
fires 64 loads naming unwritten FIFOs and counts the server's threads. Exit 0
reproduced (a thread for every load), 1 not, 2 setup; `NEGATIVE_CONTROL=1`
names missing files, whose loads fail at once. Measured: 64 threads before the
fix, 16 after; 0 for the control, both builds.

## 020 — A stopped pane process reads as exited on macOS (class 6)

**The break.** fux waits for a pane's leader with
`waitid(P_PID, pid, WEXITED | WNOWAIT)`. macOS's `waitid` also reports a child
that has only stopped, although only `WEXITED` was asked for (`si_code`
`CLD_STOPPED`, status 17), and `WNOWAIT` leaves that report in place. fux read
it as an exit, 128 + SIGSTOP = 145: the pane's `ProcessState` said `exited`,
the runtime was torn down, and the process group was killed although the
process was only stopped. `kill -STOP`, or Ctrl-Z in a pane that runs a
program directly with no job-control shell, was enough. Linux's `waitid`
reports nothing for a stop here (checked with the same C program in the Linux
container), so Linux was never affected. Pre-existing: `wait_unreaped` is
unchanged since fux's first commit.

**Found by** hunt 8's finishing run, while pausing a pane's program to test how
fux queues input for a program that is not reading.

**Fixed** in the 020 commit: a report whose `si_code` is not `CLD_EXITED`,
`CLD_KILLED` or `CLD_DUMPED` means the leader is alive. The non-blocking probe
returns "live"; the blocking waiter waits on, sleeping 20 ms between reports so
a stopped leader cannot make it spin. A stopped pane now stays a stopped,
running pane on both platforms, as it already did on Linux.

**Test.** `terminal::tests::a_stopped_leader_is_not_an_exited_one` stops a
child, then requires the probe to say live, the blocking waiter to still be
waiting 300 ms later, the probe to say live after SIGCONT, and the waiter to
report 128 + SIGKILL once the child is killed. It failed first on macOS:
`Ok(145)` for the stopped child.

**Reproduction.** `fux-fuzz/repro/020-a-stopped-pane-process-reads-as-exited.sh`
stops a pane's `cat` and reads its `ProcessState` a second later. Exit 0
reproduced (not running), 1 not, 2 setup; `NEGATIVE_CONTROL=1` does not stop
it. macOS: 0 before the fix, 1 after; Linux: 1 either way.

## 021 — Input past sixteen pieces is lost while a pane's writer lags (class 6)

**The break.** Input for a pane -- typed keys, pastes, mouse reports and
terminal replies -- waited for the PTY writer in a queue of sixteen pieces,
whatever their size. When the PTY's own buffer was full, because the program
was busy, stopped or slower than the input, every piece past the sixteenth was
refused, with the notice "sending into a full channel", and lost. A typed key
is one piece, so seventeen keys were enough. Sixteen pieces of up to 64 KiB
could wait, but not a seventeenth key. Pre-existing.

**Found by** the finishing run's pass over its own evidence: pausing a pane's
`cat` and sending it 3000 single keys over the socket delivered 1040 --
macOS's PTY buffer plus sixteen -- and the rest were lost.

**Fixed** in the 021 commit: the queue is bounded by bytes, not pieces.
Each piece is charged its length plus 64 bytes, up to 16 times the largest
accepted input (the old ceiling). The writer gives back each piece's charge as
it passes it to the PTY. Terminal replies are charged the same way. A single
input is still bounded to the largest paste, and a program that reads nothing
can still fill the queue, when fux says so: "the pane's program is not
reading its input".

**Test.** `terminal::tests::keys_wait_for_a_slow_reader_instead_of_being_lost`
stops a live pane's `cat`, sends 3000 single keys, resumes it and requires
none refused and all 3000 received in order. It failed first: 2870 refused.
`replies_remain_byte_exact_nonblocking_and_bounded` now bounds replies by cost
and releases each as the writer would.

**Reproduction.** `fux-fuzz/repro/021-input-past-sixteen-pieces-is-lost.sh`
stops a pane's `cat`, sends eighty 2000-byte pastes, resumes it, and compares
what arrived. Exit 0 reproduced, 1 not, 2 setup; `NEGATIVE_CONTROL=1` leaves
`cat` running. Before the fix 36,000 (macOS) and 48,000 (Linux) of 160,000
bytes arrived; after it all of them, on both.

## The nightly smoke, and what each failure was

The first nightly dispatch (run 35914442080, at `655d8db`) was red on both
runners; every trace and fuzz target passed. Local runs that imitate a runner
(`docker run --cpus`, a shared and loaded Mac) found three more. Each failure,
by cause:

| Case | Scenario | Cause | Resolution |
| --- | --- | --- | --- |
| 21 | `history` | Finding 017: the resize dropped the pane's last line | Fixed in `fux-vt` |
| 29, 39, 40 | `race`, `churn`, `scene_refs` | Finding 018: a blocking scene read stalled the I/O pool | Fixed; its bound is 019 |
| 47 | `adversarial` | The harness. On macos-15 the stream was still arriving 8.8 s in: 160 sleeps of 20 ms, each overshooting, forked or not. Paced faster, its end marker landed while the viewer was 2x2 and wrapped as EN/DE/D on ubuntu-24.04, which a non-reflowing emulator keeps. fux read the stream as fast as it was written; through a fux pane with the scenario's resizes it took 3.9 s against 3.8 s for the pacing alone, the same on `origin/main`. | Paced by deadline; marker gated on the pane being 23x80. Seen at 3.87-4.03 s. |
| 36 | `api_misuse` (local) | The harness. The Viewer query after a 65535x65535 resize waited behind the attached frontend's first 4096x4096 paint: 2.7-9.5 s in a debug build on a loaded Mac, 0.2-0.3 s in release. Profiles of the branch and `origin/main` match. Failures: 1 of 10 on macOS for each; on Linux 6 of 31 on the branch, 1 of 20 on `655d8db`. | 15 s allowance while that viewer exists, as `walk` and `limits` already had. 0 of 25 since. |
| 18 | `history` (local, macOS) | The harness. The scenario typed a 774-byte command through the frontend, one request per key; with other builds loading the Mac, its echo grew at about 10 ms a key and was at LINE-048 of 60 when the 5 s stage ended. Nothing was lost: no refusal notice, steady progress, 15 of 15 in isolation. | The same lines printed from a shell loop, 136 typed bytes |
| 48 | `concurrent` (local) | The smoke's global 600 s budget ran out behind `scale`, which took 464 s on a Mac at load 40-50 | The presentation writes only the viewed tab; see below |

**`scale`'s cost was mostly fux's, and is now mostly fixed.** Its thousand
moves to new tabs cost O(tabs) each: every control re-synced the viewer's
presentation by writing the whole workspace scene -- every tab -- into the
inert presentation world and laying it out, only to hide all but one tab. A
move took 424 ms at 1000 tabs on ubuntu-24.04, so `scale` was 365 s of that
runner's 497 s smoke (192-216 s on macos-15), and a loaded machine could not
finish the smoke in 600 s. Side by side under the same load, this branch and
`655d8db` took 441 and 444 s, then 409 and 409 s: it predates the branch.
Every request still returned inside its bound, so it was a cost, not a class 2
finding. The presentation now holds only the viewed tab: 800 moves in 44 s
instead of 118 s on the same host, the 800th in 97 ms instead of 241 ms. What
still grows with the tab count is the workspace's own layout scene, rebuilt
when the workspace changes.

**Three test mitigations were replaced by their causes.**

- `--test-threads=2` on CI's test steps is gone. Each flake it hid had a
  timing assumption, now fixed: the PTY size a frame set was published a frame
  late (the frame now publishes it); two design tests printed their READY
  marker before the mouse modes it was meant to confirm; and a frontend PTY's
  slave could be inherited by a concurrent fork (below). The workspace passes
  at full width: 5 of 5 in the Linux container, and on both CI runners.
- `hidden_tabs_stop_constraining_pty_size...` has its strict first assertion
  back: `size_terminals` publishes `ProcessState` in the step that resizes, as
  `terminate` does. `frame::tests::a_frame_publishes_the_pty_size_it_sets`
  failed first ((24, 80) published for an (11, 40) PTY).
- The frontend restore test ends with the strict `ends_with(b"\e[?1049l")`
  again. The `\r\n` it had tolerated was the harness's: portable-pty's master
  writer sends `\n` plus EOF when dropped, and when a concurrent test's fork
  had inherited the frontend PTY's slave -- `openpty` returns it without
  close-on-exec, which portable-pty sets afterwards, outside the lock -- the
  slave outlived the frontend and Linux echoed that newline in cooked mode.
  Holding one stray slave descriptor reproduces the exact bytes 10 of 10 on
  Linux, never on macOS. Every PTY and fork in the test binary now holds the
  `SPAWN` lock.

## Passes, and what each attacked

**Pass 1** attacked every area; three findings.

- **The phase 2 fixes** (009, 010, 012, 013): other routes to a resource-entity
  despawn, other signals, backlog fills, jobs leaving the session. Solid.
- **Entity-taking BRP methods** (eight of them) x sixteen id classes, single
  and batched, 256 requests: no death.
- **Configuration**: empty, non-JSON, million-byte prefix, empty keys, huge
  history, deeply nested, a thousand bindings, unknown fields. Each fell back
  to a usable configuration; the server answered every time.
- **The transport**: partial headers, no content-length, a 200 KB header, a
  bad method, HTTP/0.9, a slow drip, a negative content-length, ten thousand
  newlines, twenty unread connections. Robust.
- **Scenes**: malformed, empty, deeply nested, binary, and huge files.
  **Finding 016**: an unbounded read. Fixed.
- **Resource growth**: 120 panes with real PTYs (+20 MB), 5000 spawned
  entities (no leak), three unread watches against a flooding pane (bounded).
- **`fux-vt` in both modes** (fux's and koh's `events`/`extended_replies`):
  99,511 `cargo-fuzz` runs, no crash.
- **Resource methods and every `Input` kind at its bounds**: a partial
  `Settings`, a huge `history_lines`, `resize` to 65535 and to 0, `mouse` at
  65535, a 700 KB paste, an unpaired surrogate key. No death; a 0x0 viewer
  paints empty and recovers when resized back.
- **`Settings` removal** (carried from hunt 7's clean-areas note): **finding
  015**. Fixed.

**Pass 2** re-attacked every area and the code the fixes added -- a symlink to
`/dev/zero` past the scene bound, mutating `Settings` to break painting, a
`Focused` relationship pointing at a resource entity, and the entity, config,
transport and scene sweeps again. Nothing new. The first run stopped here.

**Pass 3** attacked what the finishing run changed; one finding.

- **The 017 reflow**, in both of `fux-vt`'s modes (the fuzzer's header bits
  switch `events` and `extended_replies`): 132,455 `cargo-fuzz` runs, no crash.
  Shrinks and grows with the cursor at the top, the middle and the bottom; the
  alternate screen, which keeps no history; the saved cursor; a pane scrolled
  back 36 rows, and to the top of its history, while a grow pulled history
  into the live area -- the view kept its bottom row or clamped to the oldest
  rows, and returned to live output. koh's full test suite against the change.
- **The 018 scene thread**: a thousand loads of unwritten pipes. **Finding
  019**. Fixed.
- **The frame-published size**: reflected dimension edits on a pane in a
  hidden tab still resize its PTY and stick, as on `origin/main`; visible
  again, one frame sizes and publishes it.
- **The harness changes**: each was forced into the failure it guards against
  (the READY/mode split, a stray PTY slave, the marker at 2x2) and shown to
  pass with it.

**Pass 4** re-attacked 019's fix and everything above. 24 viewers issuing
mixed loads and saves of unwritten pipes: 16 held, 8 refused with the notice;
8 viewers detached while their I/O was blocked; every pipe released, then
every place came back and a new load ran. The server answered throughout,
with no panic. 202,339 more `cargo-fuzz` runs on the final `fux-vt`, no crash.
The presentation that holds only the viewed tab: `walk`, `concurrent`, `raw`,
`tabless`, `repair`, `identity`, `layout` and `nav` on seeds 1-3, all passing;
and its fallbacks by raw edit -- zoom with the focus moved into a hidden tab,
the viewed tab reparented under a pane and back, the viewed tab closed by
another viewer -- each painting exactly as before the change. Chasing the
macOS smoke's case 18 then found two: **finding 020** (pausing a pane's
program made macOS report it exited) and **finding 021** (input past sixteen
queued pieces was lost). Both fixed.

**Pass 5** attacked 020's and 021's fixes. A stopped pane leader for 5 s: no
measurable server CPU (the waiter sleeps between macOS's repeated reports);
closing the pane while it is stopped leaves no process. Ctrl-Z of a job under
an interactive bash stops the job, not the pane; `fg` and Ctrl-C bring it back
and end it. A program that never reads its input, flooded: fifteen 64 KiB
pastes queued (983,040 bytes), the sixteenth refused with the new notice, 50
single keys still accepted within the bound, server RSS +2.2 MiB (the old
queue: +2.1 MiB); resumed, it received every accepted byte, and input worked
again. A program asking for the cursor position endlessly without reading its
replies: bounded, still running, the server answering, no panic. Nothing new.
The run stopped.

## Ranked, for reference

All eight hunt 8 findings are fixed in this branch. The one finding still open
across all hunts is 011 (the ALSA build chain), documented above as
`bevy_remote`'s to cut, not fux's. `scale`'s O(tabs) cost per control is
recorded above; its presentation part, most of it, is fixed.

# The BRP policy work: every request through one guard

> **Status: findings 022–031 fixed by one guard.** A property test that drives
> every BRP method in-process found them against the code as it stood after
> hunt 8. None ends the server -- Bevy 0.20 catches a panicking request system
> -- but each either answers without saying why or leaves the world in a state
> fux has no rules for. Every stock method now runs behind one guard
> (`src/remote/guard.rs`) that checks a declared per-type policy
> (`src/policy.rs`, the README table, `fux.policy`), validates values and the
> hierarchy as it would be after the request, and settles the world before it
> answers. Each repro exits 1; the property test passes 1,000,000 requests
> (four seeds of 250,000) with no failure.

030 and 031 came from the hunt passes over the guard, below.

How they were found: `remote::property::no_brp_request_breaks_the_server`
generates requests for every method from real serialized values -- live
components, defaults and examples of fux's events -- then mutates them
(boundaries, wrong types, missing and extra fields, entity IDs of every class)
and calls the method registry the server uses, with no HTTP. After each request
it requires no panic, no broken invariant (`invariants::violations`, also
served as `fux.invariants`) and a frame for every viewer. 16,000 requests over
four seeds found 022–027; 300 requests find most of them. 028 and 029 came
from asking why fux sometimes ignored a command in that run, and the property
test now checks for both.

## 022 — A panicking request answers "receiving from an empty and closed channel" (class 6)

Three stock handlers panic on requests a client can simply send:
`world.mutate_components` on a relationship component (`ChildOf`, `Focused`,
`Viewing`, `OnTab`, `PaneView` are immutable, and the handler calls
`reflect_mut` regardless); `world.reparent_entities` naming an entity that does
not exist; and `world.trigger_event` of `Control` or `UserInput` with a value
`from_reflect_with_fallback` cannot build (the event form of agent finding F1).
Bevy catches the panic, logs "System panicked" and keeps serving, so the server
lives -- but the client is told only that a channel closed, and a batch can be
applied halfway. Repro: `022-a-panicking-brp-request-answers-a-closed-channel.sh`.

## 023 — Closing a view despawns whatever entity it names (class 6, destructive)

`PaneView.pane` could be written to name any entity, and closing the last view
of a "process" despawns it. Pointing a view at a second workspace and closing
that pane in the ordinary way deleted the whole workspace, its tabs and its
panes. Repro: `023-closing-a-view-despawns-whatever-it-names.sh`.

## 024 — Raw hierarchy edits orphan layout and its processes (class 6)

Reparenting, inserting or removing `ChildOf`, `Tab`, `Split` or `Workspace`
could leave tabs outside every workspace, splits and pane views with no
container, workspaces nested under panes and tabs under splits. fux moved its
viewers off an orphaned tab, but the tab's processes kept running where no
command can show them again. Repro: `024-raw-edits-orphan-layout-and-its-processes.sh`.

## 025 — A workspace's order can be missing or shared (class 6)

`WorkspaceOrder` orders the workspace list and is what reordering swaps. A
workspace spawned over BRP had none, and two could share one. Repro:
`025-workspace-order-can-be-missing-or-shared.sh`.

## 026 — A viewed process can lose its state or get an impossible size (class 6)

`ProcessState` could be removed from a live process, which then had no status
or size to report, and its size set outside the 1..=4096 fux uses everywhere
else. Repro: `026-a-viewed-process-can-lose-its-state.sh`.

## 027 — A viewer created over BRP is never repaired (class 6)

A `Viewer` spawned or inserted over BRP got no workspace, tab or focus -- only
`fux.attach` gives those -- and no repair pass ran for it: it viewed nothing
and could not be painted. Repro: `027-a-raw-viewer-is-never-repaired.sh`.

## 028 — Despawning fux's observers silences it (class 6)

In Bevy 0.20 observers and registered systems are entities. `world.list_components`
on nearby IDs finds them (`bevy_ecs::observer::distributed_storage::Observer`)
and `world.despawn_entity` removes them. fux routes every `Control` and
`UserInput` through observers, so afterwards it kept answering and painting --
every invariant held -- but ignored every command. A rule of "only reflected
types are writable" would not have stopped it: `Observer` is reflected. The
property test now also checks that fux still obeys a `Control` and a
`UserInput` after each request. Repro: `028-despawning-fux-observers-silences-it.sh`.

## 029 — An unprojectable workspace drops every command without a word (class 6)

fux projects a workspace into each viewer's presentation before acting on a
command or key, and drops the event if that fails. One entity with no layout
role under a tab -- here a plain entity carrying interaction state -- made the
workspace unprojectable: frames painted the reason in the bar, but every
command and key for its viewers vanished, with no notice, so not even closing
the offending pane was possible. The invariant check now includes "every
workspace can be projected". Repro:
`029-an-unprojectable-workspace-drops-every-command.sh`.


## 030 — The placeholder entity, sent as a reference, passes the guard (class 6)

Found in the hunt pass against the guard itself. The guard plans a spawn with
Bevy's placeholder entity (bits `1`) standing for the entity the spawn will
create, and asked "does this entity exist?" of the plan. A client that sent
`1` as a reference -- `ChildOf(1)` on a plain entity -- was taken to mean that
new entity, so the insert was accepted and the entity was left related to one
that was never spawned, with Bevy logging failed commands. The placeholder now
names nothing as a reference, and the invariant check gained "every `ChildOf`
names an entity that exists", which the property test would have reported.
Repro: `030-the-placeholder-passed-as-a-reference.sh`.

## 031 — A tab spawned into a workspace in one request nests in another tab (class 6)

Found by the property test at 250,000 requests a seed, after the guard. The
natural way to add a tab -- one `world.spawn_entity` with `Tab`, `ChildOf` the
workspace and a `Name` -- produced a tab inside a new tab about half the time,
and a workspace that could not be projected (029's effect). The stock handler
inserts a request's components one at a time, in the order of a hash map that
is random per request, and flushes commands after each. When `ChildOf` came
first, fux's `normalize_on_child_added` saw a child with no role under a
workspace and wrapped it in a new tab "main"; then `Tab` arrived. It predates
the guard: the build before it reproduces it too. The guard now inserts
`ChildOf` after a request's other components, so everything fux reacts to sees
the entity whole. Repro: `031-a-tab-spawned-into-a-workspace-nests.sh`.

## How the guard fixes them, and what it keeps

Every write is checked before anything changes, so a request applies whole or
not at all, and a refusal names the rule. By finding:

- **022:** mutating an immutable relationship is refused ("replace it with
  world.insert_components"); every entity a request names must exist; an event
  must deserialize into a complete value. No request reaches a handler that
  panics.
- **023:** a `PaneView`'s pane must be a process.
- **024:** a tab's parent is a workspace, a split's or view's a tab or split,
  a workspace has none, nothing sits inside its own subtree, and one entity
  has one layout role. A placed tab or view cannot be unplaced.
- **025:** a spawned workspace without an order gets the next one; a duplicate
  is refused; the order cannot be removed.
- **026:** `ProcessState` cannot be removed; clients change only its rows and
  cols, within 1..=4096.
- **027:** a `Viewer` is created only by `fux.attach`; removing one is a clean
  detach, and fux strips its viewer-only state.
- **028:** a client may change only fux's own kinds of entity and entities made
  entirely of types clients may spawn -- never an observer, a system or a
  resource entity.
- **029:** only layout nodes go under layout nodes, and viewer relationships
  only on viewers. Past the guard, a viewer that cannot act is now told why
  ("cannot act here: …") instead of losing its commands silently.
- **030:** a reference to the placeholder entity is refused like any other
  entity that does not exist.
- **031:** a request's `ChildOf` is inserted after its other components.

Denied deliberately. Before the guard every one of these was accepted; each
either crashed, corrupted or wedged fux (the finding is named), or is a
default of the prompt's policy table that no documented use needs:

- writing any type the table keeps read-only: interaction state (`Prefix`,
  `Overlay`, `Mode`, `Entry`, `Run`) -- drive it with `Control` and
  `UserInput` (029's route) -- `PaneViews`, `Children`, and the Bevy UI and
  text types fux does not paint from (`Text`, `BackgroundColor`, …); any
  other registered type is read-only by default (028);
- resources other than `Settings`, and removing `Settings` (hunt 8 finding
  015 restored it afterwards; now refused before it happens); a `Settings`
  that fails the configuration file's own check;
- `world.write_message`, for every message, and triggering any event but
  `Control`, `UserInput` and `Shutdown`, or one that is not complete (022);
- naming a resource entity in any entity method, reads included (hunt 7
  finding 004/009; the resource methods reach resources by type), and
  changing an entity internal to fux or Bevy, such as an observer (028);
- naming an entity that does not exist, anywhere -- a target, a pane, a
  parent, the placeholder (022, 030);
- hierarchy edits that break the layout rules: a tab outside a workspace, a
  split or view outside a tab or split, a workspace with a parent, a cycle,
  two layout roles, a role-less child under a layout node (which fux used to
  wrap into a new tab), unplacing a placed tab or view, a viewer or process
  with a parent, removing a split that still holds panes (024, 029);
- removing `Workspace`, `Tab` or `PaneView` from their entity -- despawn it
  instead (024);
- spawning or inserting a `Viewer` (only `fux.attach` creates one), a viewer
  larger than 4096, a viewer relationship on a non-viewer or pointing outside
  its workspace or tab (027, 029);
- removing `ProcessState` or `WorkspaceOrder`, changing a process's status or
  revision, a size outside 1..=4096, a duplicate order, a `ProcessState`
  spawned without its `Launch` (025, 026);
- a `Launch` with no program, or a history no terminal can hold (the table's
  rule; a program that does not exist still starts and reports `Failed`);
- a `Name` over 4096 bytes or with a control character (it is painted into
  every viewer's bar), and a `Node` number that is not finite or beyond 1e6;
- `world.mutate_components` on an immutable component, a relationship: its
  handler panicked (022).

Kept, because fux or its clients use them, even where the prompt's table
started stricter: the two-step way of adding a pane or a tab (spawn it
unplaced, then reparent it); resizing a PTY through `ProcessState`; removing
`Viewer` to detach; removing a viewer's `Viewing`, `OnTab` or `Focused`
(fux chooses again; the `raw` scenario checks it); hiding a node with
`Visibility`; removing `Node` (nothing breaks without it); a `Viewer`
scrollback past the history (painting clamps it; the agent F1 evidence sets
one); spawning `ProcessState` with its `Launch` to choose the size; events
for a viewer that has just detached (a frontend's input races its own
detach; refusing them made the frontend exit abnormally, found by the
`race` scenario); and a `Launch` whose program cannot start.

## The policy layer, and what it rules out

Before the guard, safety over BRP was three special cases added after crashes
(005, 004, 015). Now every request passes one path, and the finding classes
below cannot recur through BRP without the guard itself being wrong:

- **Class 1 (the server ends) and the panics of 022.** No request reaches a
  stock handler with an entity that does not exist, a resource entity, an
  immutable component to mutate, or an incomplete value. Bevy 0.20 catches a
  panicking request system, so these ended the request rather than the server,
  but each was a crash in the handler.
- **Class 6 (a silent invariant break) through BRP: 023–027, 029–031.** The
  guard checks the world as it would be after the request against the same
  structural rules `fux.invariants` serves, before writing, and checks them
  again after writing: a debug build fails the request that broke one, a
  release build logs it with the request.
- **Class 8 (a refused input changes state).** Everything is checked before
  anything is written, so a refused request changes nothing -- a spawn with
  one denied component spawns nothing, a reparent batch with one bad entry
  moves nothing.

How the property test shows it: `no_brp_request_breaks_the_server` runs every
method against a real world, with values from live components and events
mutated to boundaries, wrong types, missing fields and entity IDs of every
class, and after each request requires no panic, `violations` empty and a
frame for every viewer. Against the code before the guard it failed within
300 requests (22 distinct failures in 16,000); after the guard it passes a
million, and CI runs 2,000 on every push and 200,000 every night from a new
seed.

The `raw` fuzz scenario, whose edits are deliberately hostile, now accepts the
guard's refusal as the outcome for the kinds of edit refused by design, and
only for those; every other kind must still be applied and repaired.

## Hunt passes over the guard

**Pass 1** (after the guard and the peer check were committed):
- Manual edges: a spawn with one denied component, an insert with one
  invalid component, a reparent batch with one bad entry, mutations through
  paths into relationship vectors, a watch on an entity despawned mid-watch.
  Each applied whole or not at all. Probing references found 030.
- The property test at 1,000,000 requests found 031, and that two seeds in
  every pair gave the same stream (`seed | 1`); seeds are now mixed.
- The smoke found the table's defaults denying two things in use:
  `process` spawned a `Launch` with an empty argv (the refusal is kept, and
  the scenario asserts it), and `raw` removed a viewer's relationships (made
  removable again: fux repairs them).
- Reads, watches and the schedule methods were not yet behind the guard,
  and not every type fux registers had a declared policy; both are fixed.

**Pass 2** (at `52f6e9a`), which found nothing new:
- Property test: 1,000,000 requests, four release runs of 250,000 (seeds
  201-204), no failure. It now also drives `world.observe+watch`,
  `schedule.list` and `schedule.graph`.
- fux-fuzz: the smoke (65 cases) and all 24 traces on macOS, Linux arm64 and
  Linux arm64 with ext4 `/tmp`; `identity`, `raw`, `hostile`, `api_misuse`
  and `walk` on seeds 2-4 on macOS.
- Manual edges, again and on the new surfaces: the five above;
  `world.observe+watch` of `Control`, of a component, of an unknown type,
  and on the placeholder (a resource entity is covered by
  `remote::tests::every_method_is_guarded`); `schedule.graph` with an
  empty and a 100,000-byte label; an insert of only `ChildOf`; an empty
  insert. Every answer was a result or a refusal that says why; invariants
  held; nothing panicked.
