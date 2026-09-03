# Prompt: koh PR #14 review fixes — per-connection echo-ack, broadcast wake, shared-host guard, bell hook

Paste the section below into a Claude Code session opened in the koh repo with the `generic-host`
branch checked out. Audited against PR #14 at 2888a07 on 3 Sep 2026. It follows the 0.11.0 prompt
in `docs/koh-pr-prompt.md`; every constraint stated there still holds.

---

Push fix commits to the open PR #14 (`generic-host` → `main`, "0.11.0: generic session host and
client state, shared sessions, OSC 9;4 progress, bell hook"). Do not open a new PR, do not rebase
or squash the existing commit, do not bump the version again. The review found two bugs that break
shared sessions (KS-01), the feature this release exists for, plus one leak, one test that does not
test what it claims, and a handful of small cleanups. Fix all of them in the order below, one commit
per numbered section, each with its own tests, and run the full check list at the end.

Read first: `src/server/session.rs` (`SessionHost` lines 62–115, `drain` lines 272–301,
`AttachGuard` lines 506–580, `SharedHost` lines 450–504), `src/server/mod.rs` (`ServerSession`
lines 140–235, `run_attached` lines 250–337), `src/server/cli.rs` (`Typed::serve_conn` lines
246–308, `Hosts` line 330), `src/terminal/server.rs` (`input_history` line 173,
`register_input_frame` line 255, `set_echo_ack` line 265, `snapshot` line 324),
`src/client/cli.rs` (`BellHook` lines 71–135, the tests from line 500), `src/client/mod.rs`
(the bell call at line 842), `tests/shared_session.rs`, `tests/e2e_generic_host.rs`,
`tests/bell_hook.rs`, `docs/ARCHITECTURE.md` (the KH/KS/KC/KB sections), `CHANGELOG.md`.

Constraints unchanged from the 0.11.0 prompt: `unsafe_code = "forbid"`, `dead_code = "deny"`, the
clippy panic denies, the CI layering guard, `panic = "unwind"`, the house test style (behavioural
sentence names, requirement ids in a comment inside the test, the `#![allow(...)]` block copied
from `tests/reattach.rs:6-15` on any new integration target, multi-thread tokio for real sessions,
loopback iroh, substring assertions, 10 s marker deadlines). Wire format, `PROTOCOL_VERSION` (3),
`TERMINAL_ALPN`, `TerminalScreen`/`ScreenDiff` encoding, the `koh` binary's flags and defaults
stay unchanged. The existing pty regression suite (`reattach`, `exit_status`, `parity`, `pty`,
`e2e_loopback`, `e2e_reconnect`) must pass without semantic edits.

## 1. Echo-ack is per connection, not per host (KS-02)

The bug: SSP frame numbers are per transport, but `ServerTerminal` keeps one monotonic `echo_ack`
and one `input_history`, and every attached loop feeds it its own `remote_num()`
(`src/server/mod.rs:304`). With viewer A at frame 40 and viewer B at frame 3, B's registrations
are dropped as stale (`src/terminal/server.rs:257`) and B receives `echo_ack = 40` in every
snapshot, so B's predictor treats every prediction as already acked. Local echo on the second
viewer of a shared host is broken; with one viewer nothing is wrong, which is why no existing test
catches it.

The fix moves frame history and ack promotion into the per-connection core and makes the host a
pure state producer:

- Do not touch `SyncState`; it is the wire contract. Add one method to `SessionHost`:

  ```rust
  /// Stamp the echo-ack the connection loop computed for *its* client onto a snapshot it just
  /// took (S-03). The state is per connection from this point on; the host never sees frames.
  fn stamp_echo_ack(state: &mut Self::State, echo_ack: u64);
  ```

  and **remove** `register_input_frame`, `set_echo_ack` and `echo_ack_wait_time` from
  `SessionHost`. The prompt that produced the PR put them on the host; that was the design gap.
- Move `input_history`, `echo_ack`, `echo_timeout_ms`, `register_input_frame`, `set_echo_ack`
  and `echo_ack_wait_time` out of `ServerTerminal` into a new `pub(crate) struct EchoAck` in
  `src/server/mod.rs` (bodies verbatim, including the injected-timeout constructor the existing
  unit tests use), owned by `ServerSession<S>`. The three `ServerTerminal` echo tests
  (`echo_ack_debounces`, `echo_ack_honors_injected_timeout`,
  `echo_ack_is_monotonic_and_takes_newest`) move with the code and keep their names.
  `ServerTerminal::snapshot` no longer sets `echo_ack` (leave the field at 0);
  `PtyHost::stamp_echo_ack` sets `state.echo_ack`. `TerminalScreen` and `ScreenDiff` are not
  touched: `echo_ack` was already a field on the wire, it is just written by a different caller.
- `run_attached`: the lock block at `src/server/mod.rs:266-274` becomes
  `let echo_changed = session.set_echo_ack(now);` before the lock, snapshot under the lock when
  `needs_snapshot(echo_changed)`, then `H::stamp_echo_ack(&mut snap, session.echo_ack())` before
  `install_snapshot`. The `echo_wait` comes from the core. The `register_input_frame` call at
  line 304 becomes `session.register_input_frame(input.frame, now)`. Nothing else in the loop
  changes.
- `ScriptedHost` and `EchoHost` (tests) lose their `pending_frame`/`echo_ack` fields and
  implement `stamp_echo_ack`. `GridState` keeps its `echo_ack` field. The README sketch drops the
  three removed methods and gains the one-line `stamp_echo_ack`.
- Docs: `SessionHost` doc comment, `docs/ARCHITECTURE.md` (add KS-02: "echo-ack is per
  connection; a host never sees frame numbers"), `CHANGELOG.md` 0.11.0 entry (the trait shape
  changed before release, so edit the existing entry rather than adding a new one).

Tests:

- `src/server/mod.rs`: `run_session_with` over `ScriptedHost` with **two** fake clients on one
  `SharedSession` (the pattern of the existing `ServerSession` unit tests): client A sends
  frames 1..40, client B sends frames 1..3; after the debounce, A's snapshots carry
  `echo_ack == 40` and B's carry `echo_ack == 3`, never 40. Name it
  `echo_ack_is_tracked_per_connection_so_a_second_viewer_sees_only_its_own_frames`.
- `tests/shared_session.rs`: `second_viewer_gets_its_own_echo_ack`: two real peers on
  `SharedHost<PtyHost>`; A types 30 separate one-byte inputs (30 frames), then B types one; B's
  replica must reach `echo_ack() == 1` (or B's own frame number, read from its transport) within
  10 s and must never observe an `echo_ack` greater than its own `newest_sent_num()`.
- `src/server/mod.rs` proptest, 128 cases: two `EchoAck` instances fed interleaved random frame
  sequences never influence each other (trivially true after the move, but it pins the
  invariant): each one's `echo_ack()` is always `<=` the max frame it was given.

## 2. A state change wakes every attached viewer (KS-03)

The bug: `drain` pulses `handle.changed.notify_one()` (`src/server/session.rs:299`), which
releases **one** waiter. With two loops parked on `handle.changed.notified()`
(`src/server/mod.rs:280`) the second only re-snapshots on its 1 s `wait_ms` cap. The existing
shared-session test passes only because its deadline is 10 s.

Fix with a version counter that cannot lose a wakeup:

- Replace `changed: Arc<Notify>` on `SessionHandle` with `changed: tokio::sync::watch::Sender<u64>`
  (initial 0). `SessionHost::attach_notify` takes a `ChangeSignal` newtype wrapping the sender
  with one method, `fn pulse(&self)`, that does `send_modify(|v| *v = v.wrapping_add(1))`.
  `drain` calls `handle.changed.pulse()`. Keep `notify_one` semantics documented as "coalescing":
  `watch` collapses a burst into one wake exactly as the stored permit did.
- `run_attached` subscribes once (`let mut changed = handle.changed.subscribe();` after
  `mark_changed`-style seen tracking: call `changed.borrow_and_update()` right after each
  snapshot so a pulse between snapshot and select is not lost) and selects on
  `changed.changed()`. Because `watch` keeps a "seen" version per receiver there is no window in
  which a pulse can be missed, which was the reason `notify_one` was chosen over
  `notify_waiters`; say so in the comment that replaces the one at line 277.
- `ScriptedHost.notify`, `EchoHost.notify` and `request_exit` in the tests use the new signal.
- Docs: `SessionHandle` doc, the concurrency paragraph at the top of `session.rs`, ARCHITECTURE
  KS-03, CHANGELOG (edit the 0.11.0 entry).

Tests:

- `src/server/session.rs`: `a_pulse_wakes_every_subscribed_viewer_not_just_one`: two receivers,
  one pulse, both `changed().await` resolve within 100 ms.
- `src/server/session.rs`: `a_pulse_between_snapshot_and_wait_is_not_lost`: borrow_and_update,
  pulse, then `changed().await` resolves immediately.
- `tests/shared_session.rs`: tighten `two_peers_share_one_pty_host`: measure the time from A's
  marker appearing on A to appearing on B and assert it is under 500 ms (the old bound was the
  10 s deadline; the bug made it up to 1 s).

## 3. `AttachGuard` releases the right store key for a shared host (KS-04)

The bug: on unwind the guard calls `detach(&store, peer)` (`src/server/session.rs:561`), but
`SharedHost` stores every peer under `SharedHost::key()`, so the lookup misses, `attached` is never
decremented and the host is never TTL-reaped after a panicking connection task.

Fix: the guard holds `Arc<dyn ErasedProvider>`-equivalent detach, not a store. Simplest: make
`AttachGuard<P: HostProvider<H>, H>` hold `Arc<P>` (or a boxed `Fn(EndpointId) -> BoxFuture<()>`)
and call `provider.detach(peer)` in `Drop`. `Typed::serve_conn` (`src/server/cli.rs:288`)
constructs it from `self.provider`; `Typed` should hold `Arc<P>` so that clone is cheap. Remove
`HostProvider::store` from the guard's needs (keep it for the reaper).

Tests: `src/server/session.rs`: `attach_guard_releases_a_shared_host_attach_under_the_shared_key`
(a copy of `attach_guard_releases_the_attach_when_dropped_armed` over `SharedHost<ScriptedHost>`:
after the armed drop, the single store entry has `attached == 0` and `last_detach.is_some()`). Add
`SharedHost` steps to the KS-01 proptest's op alphabet: an "unwind" op that drops an armed guard
must behave exactly like `detach`.

## 4. The bell-hook env scrub is actually tested (KB-02)

The bug: `bell_hook_spawns_detached_with_koh_env_scrubbed` (`src/client/cli.rs:520`) sets
`KOH_KEY_PASSPHRASE` inside the hook's own `sh -c`, so the scrub is never exercised; the comment
admits it. Also, on first frame after a (re)attach `BellHook` starts at `last_count: 0`
(`src/client/cli.rs:84`) while the host's `bell_count` is cumulative, so a reattach after bells
rang while detached, or a new viewer joining a shared host, spawns the hook once immediately.

Fix:

- Split `fire` into a pure builder `pub(crate) fn command(&self, count: u64, title: &str,
  parent_env: impl IntoIterator<Item = (OsString, OsString)>) -> std::process::Command` that
  starts from `env_clear()`, copies `parent_env` minus every `KOH_*` key, then sets the two
  exports and the three `/dev/null` fds; `fire` calls it with `std::env::vars_os()` and spawns.
  Share the predicate with `pty.rs`: extract the `starts_with("KOH_")` test from `scrub_koh_env`
  (`src/pty.rs:59`) into `pub(crate) fn is_koh_env_key(&OsStr) -> bool` and call it from both.
- Seed the hook from the first frame: add `BellHook::prime(&mut self, count: u64)` that sets
  `last_count` without spawning, called from `drive_connection` on the first render of each
  connection **only when the hook has never observed a count** (a reconnect must not re-prime, or
  bells during the outage would be swallowed; a first attach must, or stale bells fire). Document
  the choice on `ConnectArgs::on_bell` and in the README Termux paragraph: "bells that rang before
  you attached do not fire the hook; bells during a reconnect do".

Tests:

- Replace the broken test with `bell_hook_command_scrubs_parent_koh_vars_and_exports_its_own`:
  build via `command(..)` with a synthetic parent env containing `KOH_KEY_PASSPHRASE=secret`,
  `KOH_LOG=x`, `PATH`, `HOME`; run it with `env > file`; assert the file has `KOH_BELL_COUNT`,
  `KOH_TITLE` and `PATH`, and lacks `KOH_KEY_PASSPHRASE` and `KOH_LOG`. Write the file under a
  per-test directory created and removed by the test (no new dev-dependency; `tempfile` is not
  one today), and remove it on the failure path too.
- `bell_hook_prime_swallows_the_count_it_is_seeded_with_but_not_later_rises`: prime(5),
  observe(5) is false, observe(6) is true.
- `tests/bell_hook.rs`: add `stale_bells_before_attach_do_not_fire_but_bells_after_reconnect_do`:
  ring once before the client attaches (drive a `Transport` directly, as `tests/shared_session.rs`
  does, then close it), attach with the hook, assert no spawn in 2 s; then use the
  `e2e_reconnect.rs` pattern to drop and reconnect the link, ring during the outage, assert one
  spawn after reconnect.

## 5. Small cleanups (one commit)

- Re-export `Hosts` from `server` (`pub use cli::{serve, serve_with, Hosts, ServeConfig}` at
  `src/server/mod.rs:17`); update the README sketch, `src/lib.rs` stability list and the tests to
  `koh::server::Hosts`.
- Replace the crate-wide `future_not_send = "allow"` in `Cargo.toml:207` with
  `#[expect(clippy::future_not_send, reason = "...")]` on `connect_with`, `run_client`,
  `run_client_with`, `drive_connection` and `reconnect` only, so server futures stay checked.
- `src/server/cli.rs:280`: for a `SharedHost`, the second viewer's attach is logged as
  "reattaching to this peer's existing session". Add `AttachKind::Joined` returned by
  `SharedHost::attach` when `attached` was already `> 0`, logged as "joined the shared session
  (n viewers)". `PtyHosts` never returns it. Update the `AttachKind` match in `serve_conn` and the
  ARCHITECTURE KS-01 text.
- `src/lib.rs` stability note: state that `ssp::testkit` and `impl ClientState for GridState`
  are `#[doc(hidden)]` test infrastructure, not covered by the stability promise.

## 6. Verification

Run and paste into a PR comment titled "Review fixes":

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --lib --locked --no-default-features --features backend-termina -- -D warnings
cargo clippy --all-targets --locked --no-default-features --features backend-termina -- -D warnings
cargo clippy --all-targets --locked --no-default-features --features backend-crossterm -- -D warnings
cargo test --locked
cargo test --locked --test shared_session --test e2e_generic_host --test bell_hook -- --nocapture
cargo test --locked shared_session -- --test-threads=1   # the timing assertion in KS-03, run 5 times
cargo doc --no-deps --locked
cargo +nightly fuzz build
cargo tree --locked --no-default-features --features backend-termina -e normal | grep -c ' clap '   # expect 0
```

Then update the PR description: add a "Review fixes" section listing KS-02, KS-03, KS-04, KB-02
with one sentence each, the new test count (`cargo test -- --list | wc -l`, before was 224), and
restate that wire, `PROTOCOL_VERSION`, key format, CLI flags and defaults are unchanged. The one
public-API change relative to the first commit is the `SessionHost` trait shape (section 1); say
so explicitly, since 0.11.0 is unreleased and nothing depends on the earlier shape.

Do not publish to crates.io. Do not tag. Stop after the commits are pushed and report the PR URL
and the pasted check output.
