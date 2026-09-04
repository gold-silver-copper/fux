# Prompt: build zor and fux together

Paste the section below into a coding-agent session opened in a directory containing two sibling
Git repositories: this checkout at `fux/`, with its gitignored `references/` trees present, and an
empty, already `git init`-ed `zor/`. It was audited on 4 Sep 2026 against the reviewed koh 0.12.1
follow-up candidate (`cfb6f3e5b623b8656171278be2a5a235928142d7`; PR #17 and registry
publication are still pending), herdr 0.8.2 (`94f6d9c`), zellij main (`af38660`), and vt100 0.16.2.

The prompt deliberately resolves implementation gaps that the design documents leave open. In
particular, it records the exact koh API and prerequisite revision, makes named workspaces
implementable, and closes passthrough, scrollback-access, detach, and downstream-test seams found
in the audit.

---

Build two programs side by side: **zor**, the agent-state wrapper, in `zor/`, and **fux**, the
agent workspace, in `fux/`.

Before changing anything, read all tracked files in `fux/`, then read these reference surfaces in
full: `fux/references/koh/{README.md,CHANGELOG.md,docs/ARCHITECTURE.md}`, the public traits and
functions named below in koh's source, herdr's layout and detection sources cited by the designs,
and zellij's grid tests cited by the designs. Read any `AGENTS.md` that governs a file before
editing it. Do not read generated targets, nested Git metadata, duplicated documentation versions,
translations, binaries, or fixtures unrelated to the cited behaviour.

`fux/docs/design.md`, `fux/docs/wrapper-design.md`, and `fux/docs/wrapper-prompt.md` are the product
specification. This prompt is the execution specification and wins where it explicitly resolves a
gap or contradiction. Otherwise, `wrapper-design.md` wins for zor and `design.md` wins for fux.
Copy `fux/docs/wrapper-design.md` byte-for-byte to `zor/DESIGN.md` in Phase 0. When this prompt
chooses something an earlier document left open or described differently, append a short dated
note under that document's *Open questions*; do not silently rewrite the design.

The programs share only the `OSC 7877` report contract. fux depends on zor as a library with
default features off, which must compile only the OSC data model/parser. The binaries otherwise
remain independent.

## Working rules and completion gates

- Start by recording `git status --short`, the branch, and the three reference revisions. Preserve
  pre-existing changes. Never commit `fux/references/`, build output, captured secrets, runtime
  descriptors, socket files, or generated keys.
- Use Rust 2024, `rust-version = "1.91"`, MIT, `dead_code = "deny"`, and `unsafe_code = "forbid"`
  in fux. zor permits unsafe only in `src/platform/{linux,macos}.rs`, with
  `unsafe_op_in_unsafe_fn = "deny"` and a `// SAFETY:` explanation on every block. Deny clippy's
  `unwrap_used`, `expect_used`, `panic`, `unreachable`, `todo`, `unimplemented`,
  `indexing_slicing`, and `string_slice` outside tests.
- Keep load-bearing dependencies exact: koh `=0.12.1`, iroh `=1.0.0` where fux names iroh types,
  vt100 `=0.16.2`, portable-pty `=0.9.0`, ratatui-core `=0.1.0`, ratatui-widgets `=0.3.0`, and zor
  `=0.1.2`. Use compatible current releases for ordinary utility crates and commit both lockfiles.
  Every direct dependency must have a one-line purpose comment in `Cargo.toml`.
- fux uses koh with `default-features = false, features = ["backend-termina"]`. It has no Cargo
  features. zor has one feature, `cli`, enabled by default; its binary and every non-OSC module and
  dependency are gated by it. Declare the fux dependency as
  `zor = { version = "=0.1.2", path = "../zor", default-features = false }`, so packaging strips
  the development path after zor 0.1.2 exists on crates.io.
- Other expected fux dependencies are clap derive, serde, serde_json, toml, tokio with only the
  features actually used, tokio-util for reaper/task cancellation, nix for safe signal handling,
  anyhow, tracing, base64 for OSC 52, and proptest as a dev dependency. Do not assume koh depends
  on ratatui—it does not.
- Study reference code for invariants, edge cases, constants, and test oracles. Do not copy Rust,
  TOML, schemas, prose, or fixtures from herdr or zellij. Use koh through the public API at the
  pinned v0.12.1 revision; do not add further local patches or vendor it.
- Tests have behavioural sentence names and cite the relevant design heading or acceptance id in
  their first-line comment. Timing logic uses an injected clock. Real-session tests use koh's
  loopback endpoint helpers and bounded 10-second deadlines; short polling sleeps are allowed in
  I/O integration tests, not in pure timing tests. Every spawned process, task, socket, endpoint,
  and temporary directory has cleanup that also runs on failure.
- Parallel work is allowed only after Phase 0. Give each worker one repository or disjoint files,
  never the same worktree. Do not let one phase depend on uncommitted changes owned by another.
  At every phase gate, merge completed work, run both crates' format, clippy, and test checks, then
  commit each changed repository separately with the phase in the subject. Do not make empty
  commits merely to keep the repositories' commit counts aligned.
- Never leave `todo!()`, `unimplemented!()`, placeholder success, ignored failing tests, or a
  silently weakened requirement. If an external prerequisite is unavailable, complete everything
  independent of it and list the exact blocked evidence and command in the final report.

## Phase 0: prove the seams and freeze the contract

This phase is sequential. Do not begin the larger modules until its tests pass.

1. Verify that `fux/references/koh` is the reviewed koh 0.12.1 release commit. Read the exact signatures of
   `SyncState`, `SessionHost`, `ChangeSignal`, `HostProvider`, `SharedHost`, `Hosts`, `serve_with`,
   `ClientState`, `ClientTerminal`, `ClientIoTasks`, `spawn_client_io`, `connect_with`, `KohBackend`,
   `ScreenView`, `Pty` (including owned group shutdown), and `ServerTerminal` (including bounded
   scrollback access). Add a short `docs/implementation-notes.md` in each new project recording
   public-API constraints that later phases must not rediscover.
2. Create zor's crate and copy its `DESIGN.md`. Its always-built library exports only
   `zor::osc::{AgentId, Report, State, Flags, format, parse}`. `parse(&[u8])` accepts koh's joined callback
   payload (`b"7877;state=..."`) and, as a convenience, a complete OSC frame ending in ST or BEL.
   `state` and decimal `seq` are required; `agent` is required except for `none`. Unknown keys are
   ignored. Duplicate contract keys, unknown states or flags, invalid UTF-8 identifiers, bad percent
   escapes, overlong decoded messages, missing required keys, and trailing bytes after a terminator
   are rejected. Formatting always uses ST, a deterministic key order, percent-encodes `message`,
   and caps its decoded UTF-8 form at 128 bytes without splitting a code point.
3. Test OSC format/parse round trips for every state and flag combination, malformed inputs, and
   unknown keys. Feed formatted bytes through `vt100::Parser::new_with_callbacks`; join the
   callback params with `;` and prove the same report parses. Fuzz or proptest `parse` for totality.
4. Replace fux's placeholder with a binary skeleton and its final dependency policy. Reproduce the
   *Surface* from `design.md` in fux's own clap types. `id`, `key`, and the temporary terminal-screen
   form of `connect` translate into koh's config types and call `run_id`, `keycmd::run`, and
   `connect`; no koh clap type is available because `cli` is disabled. Commands not implemented in
   this phase return an explicit non-zero `NotImplemented` error naming their phase. Prove
   `zor::osc::parse` is available with a downstream compile test, then inspect
   `cargo tree -e normal --no-default-features` in zor to prove CLI-only dependencies are absent.
5. Commit both repositories. From here Phase Z and Phases F1-F5 may proceed in parallel; Phase I
   joins them.

## Phase Z: build zor

Follow sections 1-9 of `fux/docs/wrapper-prompt.md`, in order, with the following audited
clarifications. These override conflicting wording there.

- vt100 0.16.2 exposes no parser-ground-state query and cannot grow its configured scrollback
  limit. Give `screen` a separate, streaming ECMA-48 boundary tracker that persists across chunks
  and covers ESC, CSI, OSC, DCS, SOS, PM, APC, ST, BEL-terminated OSC, CAN/SUB cancellation, and
  their C1 forms. Test every split point of every sequence class. Configure a documented bounded
  scrollback limit at construction (at least the supported maximum terminal row count); resize the
  screen without pretending the limit grew.
- There is exactly one ordered output owner. The PTY reader only sends chunks to the main loop. For
  each chunk, the loop writes and flushes the child's bytes to stdout first, then parses a copy,
  then emits queued zor bytes only if the boundary tracker is in ground state. A reader thread and
  an emitter must never race to stdout: a mutex alone cannot prevent a later child chunk from
  overtaking parsing of the previous one and placing an injected OSC inside a control string.
- Title `prefix` and `replace` modes do not delete or edit bytes already emitted. They pass the
  child's title OSC through, then append zor's title OSC at the next safe boundary. `never` adds no
  title bytes. Restore the last original title on clean or signal-driven exit if zor changed it.
- Cache the detection window while `Screen` is mutably available; do not design an immutable
  `ScreenView` method that secretly calls `vt100::Screen::set_scrollback(&mut self)`. Restore the
  live viewport before returning from the cache refresh.
- In 0.1.0, a child-emitted OSC 7877 is passed through and logged under `--debug`; fux may consume it
  directly. zor does not reinterpret or resequence it. OSC 21337 is likewise observation-only until
  its provenance and schema are verified. Apply reports in fux in byte/ring arrival order; `seq` is
  diagnostic and deduplicates identical consecutive reports, not a cross-emitter trust boundary.
- If `ZOR_PID` is already present, avoid a second emulator/PTY layer: run the requested command as
  a transparent child and propagate its exit. Otherwise set `ZOR_PID` for the child. Cover both
  paths. Forward termination, hangup, window-change, and job-control signals to the child process
  group; restore terminal modes and titles on every exit path. Return the child's exit code, using
  the shell convention for signal death.
- Section 6 extends the Phase 0 OSC module; it does not replace its public types or parser. The
  single implementation stays at `src/osc.rs`; `emit` calls it rather than creating a second
  `emit/osc.rs`. The `cli` feature gates clap, the binary, platform, pty, screen, rules, the
  hysteresis machine, title, and event sinks.
- The original layering sentence saying `state/` imports nothing conflicts with its prescribed
  `Verdict`, `State`, and `Flags` inputs. Keep wire-neutral `AgentId`, `State`, `Flags`, and `Report` in the
  always-built `osc` module. The CLI-gated hysteresis module may import those types and otherwise
  imports no zor module; give it a small local `Observation` value so it never imports `rules`.
  The main loop converts a rule verdict into an observation. Enforce this corrected dependency
  direction in the layering check.
- If Claude Code cannot be run to capture genuine panes, ship `rules/claude.toml.draft`, keep it out
  of the bundle, and make `zor agents` honestly report no bundled Claude rules. Synthetic fixtures
  may test the engine but may not be presented as captured evidence.

Run zor's no-default-features build after every section so fux's dependency stays minimal.

## Phase F1: synced state and layout

Implement `fux/src/state/` without I/O.

- Use typed `PaneId`, `TabId`, and arena `NodeId` values. A tab owns `name`, an arena BSP
  `LayoutTree`, its own focused pane, and optional zoomed pane; the workspace owns `active_tab`.
  Pane ids are workspace-global. A popup references a pane id plus bounded dimensions and stacking
  order. This per-tab focus removes the invalid single-focus state implied by the earlier sketch.
- Define a serializable fux-owned cell: bounded UTF-8 grapheme contents, explicit
  blank/wide-leading/wide-continuation kind, and a fux-owned style/color representation covering
  vt100's foreground, background, bold, dim, italic, underline, and inverse fields. Do not attempt
  to serialize `vt100::Cell`; do not reduce contents to `char`, because combining sequences occupy
  one vt100 cell. Define serializable cursor and pane-mode types and lossless conversions from
  vt100 0.16.2.
- `PaneView` carries rows, columns, row-major cells, cursor state, modes, title, agent state, and
  viewport offset. `WorkspaceState` also carries popups, named status segments, the selected window
  title, base64 clipboard payload, monotonic aggregate bell count, per-connection `echo_ack`, final
  workspace exit code, and generation. All strings, collection counts, dimensions, and total cells
  have explicit bounds. Rendering malformed-but-decodable state must fall back safely, never index
  or allocate from unchecked peer values.
- `LayoutTree` is an arena of `Node::Leaf` and `Node::Split`; a split stores its axis,
  `NonZeroU16` ratio, and first and second child ids. Use a root and free-list or compaction
  strategy. Define the fixed-point ratio scale and legal range. Implement split, close, swap,
  resize, geometry, and directional neighbour lookup.
  Closing promotes the sibling into the parent slot; focus chooses the nearest leaf in the close
  direction, then the promoted sibling. Neighbours maximize perpendicular overlap, minimize
  directional distance, then use stable layout order. Every operation validates reachability,
  uniqueness, acyclicity, child ids, focus, zoom, and ratios.
- `WorkspaceDiff` explicitly represents removed panes as well as new/full panes. For same-sized
  existing panes it carries maximal contiguous changed-cell runs and independent deltas for every
  scalar pane field. It carries tabs/layout and small workspace metadata as replacement values only
  when changed. `apply` must exactly invert `diff_from` and preserve all unchanged fields.
- Choose `RECV_DECODE_LIMIT` and `RECEIVE_BUDGET_UNITS` from documented maximums, including cell
  text and metadata bytes rather than counting only cells. Maintain a verified cached resource-unit
  total so `resource_units()` is O(1), as koh's trait requires; update it on every mutation and
  diff application. Tests recompute it deeply and compare.

Tests: arbitrary state edits satisfy koh's round-trip law; arbitrary diffs and malformed topology
never panic; removing a pane, changing only modes, changing only agent flags, and changing metadata
round trip; close and tab switch emit no cell runs for unchanged surviving panes; random layout
operations preserve invariants; and independently written geometry cases cover herdr's behavioural
oracle. Drive `Transport<UserInput, WorkspaceState>` through koh's `SimHarness` under loss. Do not
call koh's `sim::run_generic_session`: it is fixed to koh's `GridState`, not fux's state.

## Phase F2: host, panes, and input routing

Implement `fux/src/host/` around one `WorkspaceHost` implementing koh 0.12's exact
`SessionHost<State = WorkspaceState>` methods: `snapshot`, `input`, `resize`,
`stamp_echo_ack`, `application_cursor`, `alive`, `attach_notify`, `client_detached`, `kill`, and
`shutdown`.

- Make ownership explicit. Each pane runtime owns one koh `Pty` and `ServerTerminal`; its drain
  task owns the output receiver. Start drain tasks from `attach_notify`, after a `ChangeSignal`
  exists, and immediately when panes are added later. Shared mutable pane data uses a narrowly held,
  poison-recovering lock or messages; never block while holding koh's outer session lock.
- Spawn configured commands through `zor --title never -- ...` when the configured zor executable
  passes an executable probe; otherwise log once and spawn bare. Do not treat the zor library path
  as proof that its binary is installed.
- For every PTY chunk, call `ServerTerminal::process`, drain `take_host_replies()` back into that
  same PTY (required for DA/DSR/DECRQM), drain `take_unhandled_oscs()` in arrival order through
  `zor::osc::parse`, refresh the pane snapshot, update title/clipboard/bell ledgers, set dirty, and
  pulse `ChangeSignal`. Poll `try_wait`; retain the real per-pane exit status for events and derive
  the final workspace exit status only when the workspace itself is killed or has no panes by the
  documented policy.
- Use koh 0.12's public `ServerTerminal::with_scrollback_screen` callback to inspect a temporary
  scrollback position without cloning the full configured history. Clamp rows and output bytes
  before materializing capture text; the callback must restore the live viewport even if capture
  panics. Do not add a replay parser or raw-output log: one authoritative emulator per pane is
  sufficient.
- The router is streaming across input chunks. Outside bracketed paste, the configured prefix opens
  one-key command mode. Inside paste, bytes are verbatim. Unknown/incomplete escape sequences are
  forwarded losslessly. Buffer only a prefix that may still become a recognized sequence, and use
  an injected, bounded ambiguity timeout so a lone Escape or partial sequence cannot stall input.
  Handle SGR mouse globally: focus on click; use Shift or copy mode for local selection; otherwise
  translate coordinates to the target pane and re-encode for an application that requested mouse
  input. Wheel scrolls fux history only when the pane application has not claimed the event. Test
  sequences split at every byte boundary.
- Resize uses the most recent client size, with an explicit deterministic tie rule. Fit the active
  tab and popups, subtract borders/status before resizing PTYs, and handle rectangles too small for
  koh's 2x2 minimum without underflow. `client_detached` removes that client's remembered viewport.
- `stamp_echo_ack` changes only the snapshot passed to it; frame numbers never enter shared host
  state. `snapshot` is bounded and clones only dirty pane views where practical. `kill` is
  best-effort and nonblocking; `shutdown` closes every pane and joins owned tasks.
- In this phase, foreground `fux serve` hosts one default workspace with
  `Hosts::new().with(FUX_ALPN, SharedHost::new(...))`; `FUX_ALPN` is `b"fux/1"`. It may also register
  `TERMINAL_ALPN` with `PtyHosts` for plain-koh fallback. At least one explicit remote allow id is
  required. The local daemon's automatic identity and multi-workspace endpoint policy arrive in F4.

Tests: router tables and chunk boundaries; DA/DSR replies reach a real child; a real child OSC
updates agent state; consecutive duplicate reports behave as documented; bell and clipboard
accounting survives pane close; every PTY receives safe geometry; scrollback requests clamp safely;
and a fake host round-trips over a real loopback connection using `Hosts::serve_connection` plus
`run_client`.

## Phase F3: client and compositor

Implement `fux/src/client/` after F1; it may proceed in parallel with F2.

- Implement koh's exact `ClientState` surface, including `echo_ack()`. `window()` returns fux's
  title, clipboard, and aggregate bell ledger; `input_modes()` mirrors application keypad,
  application cursor, and bracketed paste from the focused pane while enabling the outer SGR mouse
  mode fux's router needs. `predict_target()` returns the focused live pane as a
  `koh::predict::ScreenView`; return `None` in copy/scroll mode or when focus/topology is invalid.
- Implement `ClientTerminal<WorkspaceState>` with a fux-owned wrapper around
  `koh::client::backend::DefaultBackend`. koh's `BackendTerminal` implements only
  `TerminalScreen`, its `OutOfBand` ledger is private, and its `CaptureBackend` is `pub(crate)`;
  none is reusable downstream. Reproduce the small ledger in fux using public `KohBackend`
  methods and `InputModes::{formatted,diff}`: enter/leave raw and alternate-screen modes, sanitize
  and bound titles, make OSC 52 opt-in and validate base64, coalesce bells, reset modes on drop, and
  invalidate after suspend/resume. Define a fux-local capture backend for tests.
- Composite a ratatui `Buffer` at the real terminal size. Lay out the active tab inside the area
  left after the tab/status row, paint pane cells with continuations skipped, translate the koh
  prediction `Overlay` from focused-pane coordinates into that pane's content rectangle, draw
  borders and focus, then popups, then status. Treat koh's `status: Option<&str>` as a temporary
  connection banner that overrides or augments fux's status without changing synced state. Diff
  buffers and emit only changed cells through `KohBackend`, in a synchronized-output frame.
- Call the actual koh 0.12 API:
  `connect_with(config, FUX_ALPN, make_terminal, input_rx, resize_rx)`. Obtain the public raw-stdin
  and SIGWINCH channels with `koh::client::spawn_client_io`; retain its `ClientIoTasks` owner for
  the session and always await `ClientIoTasks::shutdown` afterward. Koh's higher-level CLI adapters
  remain disabled/private, but these cancellation-aware producers are part of koh 0.12.1's public API.
- Detach is client-side. Before `input_rx`, a tiny stateful preprocessor recognizes configured
  `prefix d` outside bracketed paste and emits koh's client escape `0x1e, b'.'`; the host cannot
  detach one viewer because `SessionHost::input` has no `ClientId`. On named-manager attachments,
  `prefix s` is also consumed locally: shut down the current connection, fetch the bounded manager
  list, select through `/dev/tty`, and reconnect. Other prefix commands pass to the host unchanged.
  Test configured prefixes, every chunk split, and switching between two named workspaces.
- Koh's predictor sees bytes before the host router. Confirm with a latency test that prefix/mouse
  control bytes open a tentative epoch and do not visibly corrupt the focused pane; if they do,
  disable prediction for those modes and record the limitation rather than accepting a ghost glyph.

Snapshot tests cover two panes/tabs at small and large sizes, zero/tiny areas, combining and wide
cells at the right border, prediction overlay offset, popup stacking, connection banners,
out-of-band deltas, suspend/resume, and malformed state. They use fux's capture backend.

## Phase F4: local daemon and named workspaces

Implement `fux/src/{local,manager}.rs` after F2 and F3. Resolve the koh provider-selection gap as
follows; do not attempt a first-SSP-frame selector (`HostProvider` never sees one) or an ALPN suffix
on one fixed `Hosts` value (the client ALPN is `&'static` and the registry is fixed).

- The daemon is one process with **one iroh endpoint per named workspace**, all using the static
  `FUX_ALPN`. A workspace name is a local alias for that endpoint; `fux connect <endpoint-id>` needs
  no name and attaches to exactly one workspace. Persist a distinct server key per workspace under
  `$XDG_STATE_HOME/fux/keys/`, and write a runtime descriptor containing name, pid, a per-daemon
  random instance nonce, endpoint id, and IPv4 loopback socket address. This is the dated
  Open-questions note to add to `design.md`.
- The daemon binds endpoints with koh's public `bind_endpoint*_alpns`, retains the returned
  `iroh::Endpoint` (hence fux's exact direct iroh dependency), and serves authorized completed
  handshakes through `Hosts::serve_connection`. Check the peer id against the union of the daemon's
  explicit allowlist and fux's persistent local-client id **before** that call; enforce connection
  limits with a semaphore. Retain a clone of `PtyHosts` and run its public TTL reaper when the
  terminal fallback is enabled.
- Use a global manager Unix socket at `$XDG_RUNTIME_DIR/fux/manager.sock` and per-workspace control
  sockets. Create the runtime directory as 0700 and sockets as 0600; reject unsafe workspace names,
  symlinks, stale descriptors, pid reuse, oversized frames, and non-socket collisions. Binding the
  manager socket elects the single daemon, so simultaneous first clients cannot spawn two servers.
- Bare `fux [name]` connects to the manager, starts `fux serve --daemon` only when the manager is
  absent, requests create-or-find, then calls `connect_with` using the descriptor's endpoint id,
  direct address, and `FUX_ALPN`. No-name opens the only workspace, creates `default` when there are
  none, or shows the picker when there are several. Use bounded startup/retry deadlines and report
  stale-state recovery.
- Killing one workspace closes its endpoint and panes but not the daemon. After replying to the
  request that killed the last workspace, the daemon removes runtime files and exits. Give a
  just-started daemon a bounded grace period in which to receive the initial create request.

Tests start two named workspaces in one daemon, prove state isolation and distinct endpoint ids,
attach two viewers to one, detach/kill clients and reattach with state intact, race two first
clients, recover stale descriptors, reject unsafe permissions/names, and prove the daemon exits
only after the last workspace is killed. Use private temporary XDG roots.

## Phase F5: control socket, configuration, hooks, and notifications

Implement `fux/src/control/` and `fux/src/config.rs`.

- Freeze serde request, reply, and event enums for every command/event in `design.md` before wiring
  CLI parsing. Newline-delimited JSON frames are UTF-8, bounded in bytes, and each carries an id;
  streamed events carry the subscription request's id. Unknown commands/fields get structured
  errors without closing the connection. Bound capture size, status text, argv/env, subscriber
  queues, and event rate; a slow subscriber drops `pane.output` nudges before state transitions.
- `fux ctl` and all top-level verbs—`new`, `split`, `focus`, `zoom`, `kill`, `resize`, `send-keys`,
  `capture`, `list`, `tab`, `workspace`, `set-status`, `popup`, and `subscribe`—are one thin client
  over the socket. Define escape decoding for `send-keys` once and test invalid escapes. Replies
  distinguish accepted, completed, and failed operations.
- Load `$XDG_CONFIG_HOME/fux/config.toml` with a deny-unknown-fields schema: prefix, bindings,
  default command, zor path, clipboard policy, notification policy, history/resource limits,
  remote allow ids, local-only network policy, and hooks. Validate before replacing live config;
  changes to endpoint/network policy require a workspace restart. External bindings receive
  `FUX_PANE`, `FUX_SOCKET`, and `FUX_CWD`; scrub fux/koh secrets. Hooks use bounded exponential
  restart backoff, reset after a healthy interval, and terminate with the workspace.
- Emit events after the authoritative mutation, in order. The notifier fires once on entering
  blocked and on working/blocked to idle—not on initial idle, heartbeats, duplicate OSCs, or replay.
  A `visible_blocker` may affect presentation but is not required for the alert. Use `notify-send`
  only with a display, `terminal-notifier` then `osascript` on macOS,
  and `termux-notification` on Termux. Avoid duplicate local alerts: the host owns notifications for
  locally hosted workspaces; an attaching client owns them only for remote workspaces/Termux, under
  config. Run commands detached with stdio closed and reap them.

Tests exercise every command against a running workspace; verify reply/event ordering and
backpressure; feed a real zor-formatted OSC through a PTY and observe `pane.opened`, `agent.state`,
and `pane.closed`; verify env scrubbing; restart/stop hooks with a fake clock and command; and run
notification policy against a fake executable.

## Phase I: cross-repository integration and release readiness

1. Build the actual zor binary. Point fux at it through an explicit test-only `ZOR_BIN`; never
   assume another repository's `target/debug` layout. Run a child that emits working then idle OSC
   7877 and assert control events and status snapshots. Separately run a captured/synthetic zor rule
   fixture through zor itself so the integration test covers generated, not only passed-through,
   reports.
2. Finish copy mode over F2's bounded cloned scrollback view: keyboard and Shift-drag selection,
   wrapped-line text extraction, and OSC 52 via `WorkspaceState.clipboard`. Test zellij-derived
   behaviours with independently written cases: a wide glyph that fits/does not fit the margin, combining cells,
   wrapped selection, and resize while scrolled. State explicitly that copy/scroll viewport is
   shared between viewers in v1 because koh supplies `ClientId` to resize/detach but not input; add
   the dated design note.
3. Write useful `README.md` and `CHANGELOG.md` files for both crates; do not merely paste a Surface
   section. Document security boundaries, runtime paths, remote authorization, workspace endpoint
   ids, history limits, bare-pane fallback, OSC spoofability by the pane process, detach keys,
   platform support, and every known limitation.
4. Add CI independently to both repositories. zor: fmt, clippy with/default without features,
   tests on Linux and macOS, layering check, and package. fux: fmt, clippy, tests on Linux and macOS,
   an `aarch64-linux-android` check, package, and a cross-repo job that checks out zor as `../zor`,
   builds it, and sets `ZOR_BIN` for integration tests. Pin actions by commit. Do not claim Android
   runtime coverage from a cross-check.
5. Run in each repository:

   ```sh
   cargo fmt --all --check
   cargo clippy --all-targets --all-features --locked -- -D warnings
   cargo test --all-features --locked
   cargo doc --no-deps --all-features --locked
   cargo package --locked
   ```

   Also run `cargo check --no-default-features --locked` in zor, the fux Android check, all
   cross-repository integration tests, and any repository-specific layering/security checks.
   `cargo publish --dry-run` for zor comes first. A verified fux package cannot be built after Cargo
   strips development paths until crates.io resolves both `koh =0.12.1` and `zor =0.1.2`: before
   then, attempt the command and record the exact unresolved prerequisites. `cargo package
   --no-verify --locked` also resolves registry dependencies and is not an archive-inspection
   workaround. Do not call either attempt a successful package gate. Rerun full `cargo package`
   and `cargo publish --dry-run` for fux after both releases are published.
6. Perform a fresh full-diff review in each repository for correctness, regressions, unsafe edges,
   unbounded input/allocation, task/process leaks, terminal restoration, path/socket attacks,
   flaky timing, and missing tests. Validate findings against current code, fix confirmed issues,
   rerun affected checks, and repeat for any new high-severity finding.

## Deliverables

- Both repositories build, lint, and test on this machine; every ignored test is listed with the
  environment it needs. No required check may be described as green if it was skipped, unavailable,
  pending, or failed for an unrelated reason.
- A phase-by-phase commit list for each repository and final `git status --short` output.
- A closing summary per crate: deviations and dated design notes, resolved open questions, exact
  verification commands/results, independent-review findings and dispositions, supported
  platforms, and remaining risks.
- A separate **Human evidence still required** list: genuine Claude Code captures for any draft
  rules, OSC 7877 collision checks on Terminal.app/iTerm2/kitty/alacritty/wezterm/Termux/tmux,
  provenance/schema of OSC 21337, crates.io ownership/publication, real Android runtime testing,
  and any remote-relay test not run locally.
- Do not publish, tag, push, open a PR, or mutate issues/reviews unless the operator separately asks.
