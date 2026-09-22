# Where fux breaks under hostile input (hunt 5)

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

**Reproduction.** `fux-fuzz/repro/001-viewer-on-tab-repair-recursion.sh <fux>`
(no agent, no seed; exit 0 = reproduced). Also in the harness as the currently
failing `hostile` scenario and the trace
`fux-fuzz/traces/open/001-viewer-on-layout-entity.json`:

```
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario hostile
```

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

**Reproduction.** `fux-fuzz/repro/002-cross-origin-web-page-rce.sh <fux>`
(no agent; exit 0 = a cross-origin simple request executed a program).

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
  CPU (no busy loop).
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

1. **Class 1 — `repair` must select only genuine viewers.** Exclude entities
   that also carry a layout-node component from `navigation::repair`'s
   `With<Viewer>` query, or bound repair's recursion, so no `Viewer` component
   on a `Tab`/`Workspace`/`PaneView`/`Split` can abort the server. (Finding 001.)
2. **Class 8 — gate the transport against the browser.** Require
   `Content-Type: application/json` and/or an `Origin` allowlist before
   dispatch, or offer a Unix-socket / bearer-token listener, so a cross-origin
   simple request from a web page cannot execute commands on the loopback
   server. (Finding 002; review finding 6.)
