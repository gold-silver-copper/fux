# Prompt: koh 0.11.0 — generic session host and client state, shared sessions, OSC progress, bell hook

Paste the section below into a Claude Code session opened in the koh repo. Audited against koh
0.10.0 at fa637c2 on 3 Sep 2026. The 0.10.0 prompt that preceded this one shipped in PR #13.

---

Open a PR against `main` titled **"0.11.0: generic session host and client state, shared sessions,
OSC 9;4 progress, bell hook"**. Work on a branch named `generic-host`. This release exists so that
`fux`, a multiplexer built on koh, can sync its own workspace state through koh's SSP transport
instead of a terminal screen: the server hosts an in-process state producer instead of a pty, and
the client renders that state with its own compositor. The pty path, the `koh` binary, the wire
protocol and `PROTOCOL_VERSION` must be unchanged by this PR.

Read before changing anything: `src/lib.rs` (the layering law at lines 32–53), `src/ssp/mod.rs`
(`SyncState`, line 82), `src/ssp/transport.rs` (`Transport<Local, Remote>`, line 52),
`src/ssp/testkit.rs`, `src/sim.rs`, `src/terminal/mod.rs`, `src/terminal/server.rs`,
`src/server/session.rs`, `src/server/mod.rs` (`ServerSession` line 120, `run_attached` line 246),
`src/server/cli.rs` (`serve` line 188, the accept loop from line 305), `src/client/mod.rs`
(`ClientTerminal` line 197, `ClientSession` line 346, `drive_connection` line 663),
`src/client/cli.rs` (`connect` line 182), `src/client/render.rs`, `src/client/backend/mod.rs`
(`KohBackend` line 106, `CaptureBackend` line 274), `src/predict.rs`, every file in `tests/`,
`fuzz/`, `clippy.toml`, `Cargo.toml`, `CHANGELOG.md`, `docs/ARCHITECTURE.md` and
`.github/workflows/ci.yml`.

Constraints that hold throughout: `unsafe_code = "forbid"`, `dead_code = "deny"` (it covers test
and example crates, so every helper must be used), the clippy panic-prevention denies, the
layering guard in CI (`predict.rs` imports nothing from `crate::`; `server`/`client` never `use
crate::wire`), `panic = "unwind"` in release. Every new test follows the house style: a long
behavioural sentence for the name, the requirement or design id in a comment inside the test, the
crate-level `#![allow(...)]` block at the top of every new integration target (copy it from
`tests/reattach.rs:6-15`), `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]` for
anything that drives a real session, loopback endpoints from
`transport_iroh::{bind_endpoint_local, loopback_addr}`, substring assertions on state contents,
10 s marker deadlines. Give the new design ids the prefix `KH-` (host), `KC-` (client), `KS-`
(shared sessions), `KO-` (OSC), `KB-` (bell) and add a section for them to `docs/ARCHITECTURE.md`.

## 1. A session host trait; the pty becomes one implementation

Today `server::session::Session` owns `emu: ServerTerminal` and `pty: Pty`, and `run_attached`
calls `emu.set_echo_ack`, `emu.snapshot`, `pty.write_input`, `pty.resize` + `emu.resize`,
`emu.register_input_frame`, and reads `child_alive`. Extract exactly that contract:

```rust
pub trait SessionHost: Send + 'static {
    type State: SyncState + Send + Sync + 'static;
    /// Rate-gated by the S-03 echo-ack logic exactly as `ServerTerminal::snapshot` is today.
    fn snapshot(&mut self) -> Self::State;
    fn input(&mut self, bytes: &[u8]);
    fn resize(&mut self, client: ClientId, rows: u16, cols: u16);
    fn register_input_frame(&mut self, frame: u64, now_ms: u64);
    fn set_echo_ack(&mut self, now_ms: u64) -> bool;   // true when the ack advanced
    fn application_cursor(&self) -> bool;               // for the DECCKM normaliser; `false` if N/A
    fn exited(&self) -> Option<u32>;
    fn client_detached(&mut self, client: ClientId) {}
}
```

- `ClientId` is a small `Copy` newtype the server allocates per connection. The pty host ignores
  it; a multiplexer uses it for per-client viewport policy.
- `PtyHost { emu: ServerTerminal, pty: Pty, child_alive }` implements it with today's bodies
  moved verbatim. The drain task stays in `session.rs` and is started by `PtyHost::spawn`, which
  is today's `spawn_session`. Keep `spawn_session` as a thin wrapper so `tests/*` keep compiling.
- `Session<H: SessionHost>`, `SessionHandle<H>`, `SharedSession<H>`, `SessionStore<H>`,
  `attach`, `detach`, `reap`, `teardown`, `run_reaper`, `ConnGuard` become generic. The
  `changed: Notify` stays in `SessionHandle`; a host signals through it by holding a clone of
  the `Arc<Notify>` handed to it at construction (add `fn attach_notify(&mut self, Arc<Notify>)`
  with a default no-op, called once by `SessionHandle::new`).
- `ServerSession` becomes `ServerSession<S: SyncState>` over `Transport<S, UserInput>`.
  `run_attached<H>(conn, handle: SharedSession<H>, client: ClientId)` and `run_session<H>` keep
  their loops line for line, with the `emu`/`pty` calls replaced by host calls.
- `serve(ServeConfig)` keeps its signature and behaviour and is now
  `serve_with(config, PtyHosts)` under the hood. Add:

```rust
pub trait HostProvider<H: SessionHost>: Send + Sync + 'static {
    /// Called on every admitted connection. Return the existing handle to share a host.
    fn host_for(&self, peer: EndpointId, store: &SessionStore<H>) -> impl Future<Output = anyhow::Result<Option<(SharedSession<H>, AttachKind)>>> + Send;
}
pub async fn serve_with<H, P>(config: ServeConfig, provider: P) -> anyhow::Result<()>;
```

`PtyHosts` reproduces today's per-peer `session::attach` semantics. The accept loop, admission,
allow-list, `max_connections` and `max_sessions` are untouched; only the `attach` call goes
through the provider. `tracing_subscriber::fmt().init()` must move out of `serve` into the `cli`
path (or become `try_init`), because an embedding binary initialises its own subscriber and
`init` panics on a second call.

## 2. Shared sessions

Add `SharedHost<H>`: a `HostProvider` that lazily constructs one host on the first admitted
connection and hands every later peer the same `SharedSession`. `attach` already refcounts
`attached`; make sure `detach` of one client never reaps a host another client is attached to,
and that the TTL reaper applies to a shared host only when `attached == 0`, same as today. Each
connection gets its own `ClientId`, `ServerSession` and `Transport`, so two viewers get
independent SSP streams of the same state. `host.resize(client, …)` is called with each
connection's coalesced resize (KOH-05 coalescing stays per connection); the pty host applies the
last one it receives, which is today's behaviour with one client and the documented v1 policy
with several. `client_detached` is called from `ConnGuard`'s drop so a host can forget that
client's viewport.

## 3. A generic client

Today `ClientSession` owns `Transport<UserInput, TerminalScreen>` and a `PredictionEngine`, and
`ClientTerminal::render` takes `&vt100::Screen`. Generalise over the remote state:

```rust
pub trait ClientState: SyncState + Send + 'static {
    fn window(&self) -> render::WindowState<'_>;         // title, icon, clipboard, bell_count
    fn exit_code(&self) -> Option<u32>;
    fn input_modes(&self) -> InputModes;                  // bracketed paste, mouse mode+encoding, app cursor
    fn predict_target(&self) -> Option<&dyn predict::ScreenView>;  // None disables prediction
}
pub trait ClientTerminal<S: ClientState> {
    fn render(&mut self, state: &S, overlay: &Overlay, status: Option<&str>) -> io::Result<()>;
    fn size(&self) -> io::Result<(u16, u16)>;
    fn suspend_resume(&mut self) -> io::Result<()> { Ok(()) }
}
```

- `TerminalScreen` implements `ClientState`; `TerminaTerminal<B>` implements
  `ClientTerminal<TerminalScreen>` by calling today's `render::render` with `state.screen()`.
  The `WindowState` and input-mode mirroring that `render.rs` does out of band stays where it
  is; the trait just names where the data comes from.
- `ClientSession<S>`, `drive_connection<S, T>`, the reconnect loop and the escape machine
  become generic. `connect(ConnectConfig)` keeps its signature and behaviour. Add
  `connect_with<S: ClientState, T: ClientTerminal<S>>(config, term: T, input: impl Stream of
  Vec<u8>, resize: …) -> anyhow::Result<Option<u32>>` so an embedding binary supplies its own
  terminal and input source; `connect` calls it with `TerminaTerminal` and the stdin thread.
- `predict.rs`: replace every `&vt100::Screen` parameter with `&dyn ScreenView` (or a generic
  `S: ScreenView`), where `ScreenView` is a trait defined **in `predict.rs`** (the layering guard
  forbids `use crate::` there) with `size`, `cursor_position`, `cell(row, col) -> Option<CellView>`
  (`contents: &str`, `is_wide_continuation`, `fgcolor`, `bgcolor`), and
  `application_cursor`. Implement it for `vt100::Screen` in the same file. The 21 existing
  predictor tests must pass unchanged apart from type names.

## 4. OSC 9;4 progress on the server side

`terminal/server.rs`'s `Callbacks` implements `vt100::Callbacks` for title, icon, clipboard and
bell. vt100 0.16.2 also has `unhandled_osc(&mut self, &mut Screen, params: &[&[u8]])`. Implement
it to parse ConEmu/Windows Terminal progress, `OSC 9 ; 4 ; <state> ; <percent> ST`, into
`progress: Option<Progress { state: u8, percent: u8 }>` on `Callbacks`, exposed as
`ServerTerminal::progress()` and cleared by state 0. Also keep the last 16 unhandled OSC
payloads in a bounded ring, exposed as `ServerTerminal::take_unhandled_oscs() -> Vec<Vec<u8>>`
(each capped at 256 bytes; drop, never grow). Do **not** add either to `TerminalScreen` or
`ScreenDiff`: this is host-side information for an embedding server, and putting it on the wire
would change `ScreenDiff`'s encoding and force a `PROTOCOL_VERSION` bump.

## 5. Bell hook on the client

`ConnectConfig.bell_command: Option<String>` and `--on-bell <CMD>` on `koh connect`. When the
remote bell count increases, run `sh -c CMD` detached, with stdin, stdout and stderr on
`/dev/null` (the TUI owns the terminal), `KOH_BELL_COUNT` and `KOH_TITLE` in the environment,
and the `KOH_*` scrub from `pty.rs` applied first. Rate-limit to one spawn per second, coalescing
bursts. Never wait on the child; reap it on a background task. Document in `--help` and README
under the Termux section with `--on-bell 'termux-notification -t "koh bell"'`.

## 6. Documentation

- `src/lib.rs` "Public API stability": add `SessionHost`, `HostProvider`, `serve_with`,
  `ClientState`, `ClientTerminal`, `connect_with`, `predict::ScreenView` and `ServerTerminal`'s
  new accessors to the supported surface. State that `TerminalScreen` on the wire is unchanged and
  `PROTOCOL_VERSION` stays 3.
- `README.md` "As a library": a second example, twenty lines, of a custom state: a `SyncState`
  wrapping a `String`, a `SessionHost` that appends input to it, `serve_with(config,
  SharedHost::new(...))`, and `connect_with` with a `ClientTerminal` that prints it.
- `docs/ARCHITECTURE.md`: the host and client seams, the shared-session semantics, and the new
  ids.
- `CHANGELOG.md`: a `0.11.0` entry in the existing format. Changed: `Session`, `ServerSession`,
  `ClientSession`, `ClientTerminal`, `run_attached`, `run_session`, `predict` signatures. Added:
  the traits and functions above, `--on-bell`, `progress()`. State explicitly: wire, key format,
  `PROTOCOL_VERSION`, the `koh` binary's flags and defaults unchanged.
- Bump `version` to `0.11.0`; refresh `Cargo.lock`.

## 7. Tests

The bar: every new seam has a unit test that needs no iroh, tokio or pty; every behaviour a user
can observe has an e2e test over loopback iroh; every untrusted parse has a fuzz target; every
invariant with a state space has a proptest. Existing tests are the regression suite for the pty
path and must pass without semantic edits.

### Test doubles to add (reuse them everywhere below)

- `src/ssp/testkit.rs`: `pub struct GridState { cells: BTreeMap<u32, Vec<u8>> }`, a `SyncState`
  whose diff is the changed entries, sized so diffs can exceed one datagram (this mirrors a
  multiplexer's per-pane grids). Keep `LogState` for the existing tests.
- `src/server/session.rs` tests: `ScriptedHost` implementing `SessionHost<State = GridState>`:
  records every call in a `Vec<HostCall>`, appends input bytes into a cell, exposes
  `set_exited(code)`, and fires `changed` on demand.
- `src/client/mod.rs` tests: `GridTerminal` implementing `ClientTerminal<GridState>` that stores
  the latest state, and `impl ClientState for GridState` with `predict_target: None`.
- `tests/` targets that need them define their own `MockTerminal` exactly as
  `tests/e2e_loopback.rs:32-53` does; do not add a `tests/common` module.

### 7.1 Host trait and pty host (KH-)

- `src/server/session.rs`: `PtyHost` snapshot/input/resize/exited match the pre-PR behaviour:
  spawn `["sh", "-c", "printf HELLO; exit 3"]`, drain until `snapshot().screen().contents()`
  contains `HELLO`, then `exited() == Some(3)`.
- `src/server/mod.rs`: the existing `ServerSession` unit tests (9) pass over `GridState`
  instead of `TerminalScreen` where they do not depend on screen contents, proving the core is
  state-agnostic. Add one test that `run_session` over a `ScriptedHost` calls
  `register_input_frame` with the frame number the client sent and `resize` with the clamped
  geometry.
- `src/server/session.rs`: `attach`/`detach`/`reap` over `ScriptedHost` reproduce the six
  existing `#[tokio::test]`s' outcomes; `run_reaper` never reaps a host with `attached > 0`.
- `tests/integration.rs`: `integration_converges_generic_state_lossy`: a `SimHarness<UserInput,
  GridState>` at loss 0.3 for seeds 1..6 with a scripted producer mutating random cells each
  step, converging to `b_view_of_a() == a`. Extend `src/sim.rs` with a `run_generic_session`
  so `examples/chaos.rs` can drive it too.
- `tests/e2e_generic_host.rs`: server built with `serve_with`-equivalent accept loop and a
  `SharedHost<EchoHost>` where `EchoHost` appends input into a `GridState` cell; client over
  `connect_with` with a `MockTerminal<GridState>`; type a marker, assert it appears in the
  replica within 10 s; `set_exited(7)` on the host, assert `connect_with` returns `Some(7)`.
- `tests/exit_status.rs`, `tests/reattach.rs`, `tests/e2e_reconnect.rs`, `tests/parity.rs`,
  `tests/pty.rs`: unchanged apart from type names; they are the pty-host regression suite.

### 7.2 Shared sessions (KS-)

- `tests/shared_session.rs`, two tests:
  - `two_peers_share_one_pty_host`: two client endpoints, `SharedHost<PtyHost>` with
    `["sh"]`; peer A types `echo SHARED_MARKER_1\r`; peer B's replica shows it within 10 s
    without typing; B disconnects; A types a second marker and still sees it; A disconnects;
    after `session_ttl` the reaper tears the host down (use a 1 s TTL).
  - `resize_from_either_client_reaches_host`: with `ScriptedHost`, resize from A then from B;
    the host's call log shows both with distinct `ClientId`s and the last one wins in the pty
    host's `emu.size()`.
- `src/server/session.rs` proptest, 128 cases: arbitrary sequences of `attach(peer_i)`,
  `detach(peer_i)`, `reap`, `run_reaper` tick over a `SharedHost<ScriptedHost>` never drive
  `attached` negative, never reap while `attached > 0`, and always reap once `attached == 0`
  and the TTL has elapsed. Put the id `KS-01` in the doc comment as `KSSP-01` is in
  `src/ssp/transport.rs:1061`.
- `tests/admission.rs`: add a case that `max_connections` still counts per connection, not per
  host, with a shared host.

### 7.3 Generic client (KC-)

- `src/client/mod.rs`: the existing 12 unit tests pass; add `client_session_over_grid_state_
  applies_diffs_and_reports_exit` using `Transport<GridState, UserInput>` on the fake server
  side, the pattern of the tests at lines 1104–1179.
- `src/client/render.rs` and `src/client/backend/mod.rs`: the 18 `CaptureBackend` tests pass;
  add one proving `TerminaTerminal::render` over `TerminalScreen` through the `ClientTerminal<
  TerminalScreen>` impl emits byte-identical output to calling `render::render` directly.
- `src/predict.rs`: the 21 tests pass over the `vt100::Screen` impl of `ScreenView`. Add
  `predictor_runs_over_a_non_vt100_screen_view`: a 5×10 `Vec<Vec<char>>` fake implementing
  `ScreenView`; typing three bytes yields three overlay cells at the right columns. Extend the
  existing proptest at line 1012 to run its cases over both impls.
- `tests/e2e_loopback.rs`: add `connect_with_over_terminal_screen_matches_connect`: the same
  marker test through `connect_with(…, MockTerminal)` so the public generic entry point is
  exercised end to end.

### 7.4 OSC 9;4 (KO-)

- `src/terminal/server.rs` unit tests: `\x1b]9;4;1;50\x1b\\` yields `Some(Progress { state: 1,
  percent: 50 })`; `9;4;0` clears it; `9;4;1;150`, `9;4;x`, `9;4`, an empty param list and a
  4 KiB payload yield `None` and do not panic; the ring keeps the last 16 and truncates each to
  256 bytes; `take_unhandled_oscs` drains.
- `tests/pty.rs`: a real child `printf '\033]9;4;1;42\033\\'` observed through `PtyHost` gives
  `progress() == Some(Progress { state: 1, percent: 42 })`.
- `fuzz/fuzz_targets/server_process.rs`: arbitrary bytes into `ServerTerminal::process` followed
  by `snapshot()`, `progress()` and `take_unhandled_oscs()`, sized 24×80 with scrollback 100,
  using the same `catch_unwind` containment the drain path uses. Add it to `fuzz/Cargo.toml`
  and to the CI fuzz smoke with the same `-max_total_time=45 -rss_limit_mb=4096`.

### 7.5 Bell hook (KB-)

- Pure logic first: `BellHook::observe(count, now_ms) -> Option<Spawn>` with the rate limit and
  coalescing as a unit test table: counts `[0,1,1,2,3]` at times `[0,0,10,20,1500]` spawn at
  index 1 and 4 only.
- `src/client/mod.rs`: the spawn uses `/dev/null` for all three fds and the scrubbed env; assert
  by running `sh -c 'env > $KOH_TEST_OUT'` with `KOH_KEY_PASSPHRASE` set in the parent and
  checking the file lacks it and has `KOH_BELL_COUNT`.
- `tests/bell_hook.rs`: pty host running `["sh"]`; client over `connect_with` with
  `bell_command = Some("touch <tempdir>/rang")`; type `printf '\a'\r`; poll for the file for
  10 s. Then type it five times in 200 ms and assert the file's mtime changed at most twice
  within 2 s.

### 7.6 CI

- The `gate` job's `cargo test --locked` picks up every new `tests/*.rs` automatically; confirm
  in the job log that `e2e_generic_host`, `shared_session` and `bell_hook` ran on both OSes.
- Extend the layering guard: `predict.rs` still imports nothing from `crate::` (the `ScreenView`
  trait lives there), and add a third check that `src/ssp` never imports from `crate::terminal`
  or `crate::server` (the generic seam must not leak back).
- `backends` job: the three clippy runs and the no-clap check pass with the generic code; add
  `cargo clippy --all-targets --locked --no-default-features --features backend-termina -- -D
  warnings` so the library-plus-tests tree is checked without `cli`.
- `fuzz` job: build and smoke the third target.
- Run and paste into the PR description:

  ```sh
  cargo fmt --all --check
  cargo clippy --all-targets --locked -- -D warnings
  cargo clippy --lib --locked --no-default-features --features backend-termina -- -D warnings
  cargo clippy --all-targets --locked --no-default-features --features backend-crossterm -- -D warnings
  cargo test --locked
  cargo test --locked --test pty --test shared_session --test e2e_generic_host --test bell_hook -- --nocapture
  cargo doc --no-deps --locked
  cargo +nightly fuzz build
  cargo build --release --locked
  cargo tree --locked --no-default-features --features backend-termina -e normal | grep -c clap   # expect 0
  ```

## 8. PR description

Two paragraphs on the why: fux syncs a workspace, not a screen, through koh's transport, so the
server must host any `SyncState` producer and the client must render any `ClientState`; shared
sessions let a laptop and a phone view one workspace. Then a checklist of the eight sections
with what was done, the list of new design ids, and the test count before and after (`cargo test
-- --list | wc -l`). State explicitly: wire, key format, `PROTOCOL_VERSION`, CLI flags and
defaults unchanged; `serve` and `connect` are unchanged wrappers over `serve_with` and
`connect_with`; the pty host is today's code moved, not rewritten.

Do not publish to crates.io. Do not tag. Stop after the PR is open and report its URL.
