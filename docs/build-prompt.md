# Prompt: build zor and fux together

Paste the section below into a Claude Code session opened in a directory that contains two
checkouts side by side: `fux/` (this repo, with `references/` present and gitignored) and an
empty `zor/` that has been `git init`ed. Written 3 Sep 2026 against koh 0.11.0, herdr 0.8.2 and
vt100 0.16.2. The three documents it leans on are `fux/docs/design.md`,
`fux/docs/wrapper-design.md` and `fux/docs/wrapper-prompt.md`; the last is the full
specification for zor and this prompt does not repeat it.

---

Build two programs in this directory: **zor**, the agent-state wrapper, in `zor/`, and **fux**,
the agent workspace, in `fux/`. Read `fux/docs/design.md`, `fux/docs/wrapper-design.md` and
`fux/docs/wrapper-prompt.md` in full before writing anything. They are the specification; where
this prompt is silent, they decide. Where the two disagree with each other, `wrapper-design.md`
wins for zor and `design.md` wins for fux, and you add a note under *Open questions* in the
losing document saying what you did.

The two programs share exactly one contract, the `OSC 7877` state report, and one build
dependency: fux links zor as a library for the OSC parser only. Everything else is independent,
and the plan below is written so zor and fux proceed in parallel after the contract is fixed.
Use subagents for the parallel phases if they are available to you; each phase says what may
run concurrently. Every phase ends with both crates building and every test green, in a commit
per crate with a message that names the phase.

## Ground rules for both crates

- **Edition 2024, `rust-version = "1.91"`, `license = "MIT"`**, `dead_code = "deny"`, clippy at
  `-D warnings` with `unwrap_used`, `expect_used`, `panic` and `indexing_slicing` denied outside
  tests. fux forbids `unsafe` everywhere; zor allows it in `src/platform/` only, as
  `wrapper-prompt.md` says.
- **Pinned dependencies.** fux: `koh = "=0.11.0"` with `default-features = false` and
  `backend-termina`, `ratatui-core` and `ratatui-widgets` at the versions koh's ratatui pins
  resolve to, `clap` (derive), `serde`, `serde_json`, `toml`, `tokio` (the features koh already
  enables), `anyhow`, `tracing`, `zor = { path = "../zor", default-features = false }`. zor as
  in `wrapper-prompt.md`, plus a `cli` Cargo feature (on by default) that owns `clap` and the
  binary, the way koh's does, so fux's tree gets `zor::osc` and nothing else.
- **Reference code is studied, not copied** (`design.md`, that section). You may read
  `fux/references/{koh,herdr,zellij}` for invariants, edge cases, constants and test oracles.
  You may not copy a line of Rust or TOML from herdr or zellij. koh is a dependency; use its
  published API and read its source to understand it, but do not vendor it.
- **Tests** are named as behavioural sentences, cite the design section they cover in a comment
  on the first line, and inject clocks instead of sleeping. Anything that drives a real koh
  session uses koh's loopback helpers (`transport_iroh::{bind_endpoint_local, loopback_addr}`)
  and 10 s deadlines, as koh's own `tests/reattach.rs` does.
- **One crate each, no feature flags on fux.** The phone and the desktop build the same fux
  binary. zor's only feature is `cli`.
- **Commits** are small and scoped to one crate. Never commit `fux/references/`.

## Phase 0: the contract (sequential, first)

1. In `zor/`, create the crate skeleton from `wrapper-prompt.md`'s ground rules and write
   `src/osc.rs` only: `Report { state, agent, seq, visible: Flags, exited, message }`,
   `format(&Report) -> Vec<u8>` and `parse(payload: &[u8]) -> Option<Report>` where `payload` is
   what a `vt100::Callbacks::unhandled_osc` receives, split on `;` (`["7877", "state=…", …]`
   joined back is fine; accept both the split and the raw form). Unknown keys are ignored.
   Round-trip tests, plus one that feeds the formatted bytes through a `vt100::Parser` with a
   recording callbacks impl and asserts the params. Export it from `lib.rs` behind no feature.
2. In `fux/`, replace the placeholder with a skeleton: `Cargo.toml` as above, `src/main.rs` with
   the clap surface from `design.md` *Surface* wired to `todo!()`-free stubs that print "not
   yet", and `fux id`, `fux key` and `fux connect` forwarded to koh's `run_id`, `keycmd::run`
   and `connect` immediately, since those are complete in koh. Add a test that
   `zor::osc::parse` is reachable from fux.
3. Commit both. From here zor (Phase Z) and fux (Phases F1–F5) proceed in parallel; Phase I
   joins them.

## Phase Z: zor (parallel with F1–F5)

Follow `wrapper-prompt.md` sections 1 through 9 exactly, in order, one commit each. Section 6's
`osc` module already exists from Phase 0; extend it, do not rewrite it. When section 8 asks for
Claude Code fixtures you cannot capture in this session, do what it says: commit the draft rule
file and an empty bundle, and list the missing fixtures in the closing summary.

## Phase F1: the synced state (fux, parallel)

`fux/src/state/`. `WorkspaceState` and `WorkspaceDiff` implementing koh's `ssp::SyncState`:

- `WorkspaceState { tabs: Vec<Tab>, active_tab, focus: PaneId, panes: BTreeMap<PaneId,
  PaneView>, popups: Vec<Popup>, status: BTreeMap<String, String>, title, bell_count,
  echo_ack: u64, exit_code: Option<u32>, generation: u64 }`. `Tab { name, layout: LayoutTree,
  zoom: Option<PaneId> }`. `PaneView { rows, cols, cells: Vec<Cell>, cursor, cursor_visible,
  modes: PaneModes, title, agent: Option<AgentState>, scrolled: Option<u32> }` where `Cell` is
  koh's cell representation if it is public, else a `(char, style)` pair with the same fields
  koh's `TerminalScreen` carries.
- `LayoutTree` is the arena design from `design.md` *Reference code is studied, not copied*:
  `Vec<Node>` with a typed `NodeId`, `Node::Leaf(PaneId)` or `Node::Split { axis, ratio:
  NonZeroU16 /* per mille */, first, second }`, a root, and no `Box`. Operations: split, close
  (the sibling takes the parent's slot; focus goes to the nearest leaf in the closed pane's
  direction, then to the sibling), swap, resize along the axis, `layout(area: Rect) ->
  Vec<(PaneId, Rect)>`, `neighbour(pane, direction) -> Option<PaneId>` by geometry (largest
  overlap on the perpendicular axis, nearest on the direction axis, ties to the earlier pane).
  herdr's `layout.rs` tests are your oracle for the geometry cases; write your own tests that
  cover the same situations.
- `diff_from` emits layout and metadata as whole values when changed (they are small), and per
  pane either `Full(PaneView)` when the base lacks the pane or its size differs, or
  `Cells(Vec<Run>)` of changed-cell runs plus cursor and mode deltas. `apply` is its inverse.
  `RECV_DECODE_LIMIT` and `RECEIVE_BUDGET_UNITS` are set consciously with a comment;
  `resource_units` is the cell count summed over panes.
- Tests: proptest that `apply(diff_from)` reproduces the state for random edits; a closed split
  yields no cell traffic for surviving panes; a tab switch to already-present panes yields no
  cell traffic; every layout operation on random trees keeps the invariants (every leaf reachable
  once, ratios in range, focus valid); neighbour geometry on the herdr cases.

## Phase F2: the host (fux, after F1)

`fux/src/host/`. `WorkspaceHost` implementing koh's `SessionHost` with `State =
WorkspaceState` (`snapshot`, `input`, `resize(client, rows, cols)`, `stamp_echo_ack`, `alive`,
`attach_notify`, `client_detached`, `kill`, `shutdown`), wrapped in koh's `SharedHost` so every
authorized peer sees one workspace. Inside:

- **Panes.** One koh `pty::Pty` plus `terminal::ServerTerminal` per pane, drained on a task that
  sets a dirty flag and pulses the `ChangeSignal`. Every pane is spawned as `zor --title never
  -- <command>` when `zor` is on `PATH` or configured, else bare. On each drain, `take_unhandled_oscs()`
  is drained and each payload goes through `zor::osc::parse`; a report sets the pane's
  `AgentState` in `seq` order. Title, bell and OSC 52 come from the `ServerTerminal` as today.
- **Input router.** Bytes from the client are decoded conservatively: the prefix key (default
  `Ctrl-a`) opens a one-key command mode with the bindings from `design.md` *Surface*; SGR mouse
  reports are decoded for click-to-focus, wheel-to-scroll and Shift-drag selection; everything
  else, including anything unrecognised, goes to the focused pane's pty verbatim. Application
  cursor mode is mirrored from the focused pane.
- **Geometry.** The workspace's size is the most recent client resize (last resize wins);
  each pty is resized to its pane rectangle minus borders; zoom gives one pane the whole area.
- **Serve.** `fux serve` is koh's `serve_with(config, Hosts::new().with(FUX_ALPN, provider))`
  with `FUX_ALPN = b"fux/1"`; `--allow` is passed through. Also bind `TERMINAL_ALPN` with
  `PtyHosts` so plain `koh connect` still gets a shell, which is the phone fallback.

Tests: a scripted fake pane (no pty) drives snapshot and diff through a koh loopback session
using koh's `ssp::testkit` and `sim::run_generic_session`; a zor-formatted OSC written to a
real pty's child ends up as `AgentState` in the next snapshot; the router's decoding table on a
corpus of key and mouse sequences; a resize reaches every pty.

## Phase F3: the client (fux, after F1, parallel with F2)

`fux/src/client/`. `impl ClientState for WorkspaceState` (`window`, `exit_code`, `echo_ack`,
`input_modes` from the focused pane's modes, `predict_target` as the focused `PaneView` behind
koh's `predict::ScreenView`) and a `ClientTerminal<WorkspaceState>` whose `render` composites
with `ratatui-core`: a `Buffer` the size of the real terminal, `Layout` from the active tab's
tree fitted to that size, each pane a widget over its `PaneView` (wide-glyph continuation cells
marked skip), borders with the focused pane highlighted, a one-row status line with tabs, pane
agent states (blocked panes highlighted) and named segments, popups drawn last. The `Buffer` diff
against the previous frame is written to koh's `KohBackend`. Clipboard, title and bell mirror
through `window()` as koh does for `TerminalScreen`. `fux connect <id>` is
`connect_with(config, FUX_ALPN, terminal)`.

Tests: `CaptureBackend` snapshots of the compositor for a two-pane, two-tab workspace at two
sizes; a wide glyph at the right border; a popup over a pane; prediction confirmed on the focused
pane through koh's testkit.

## Phase F4: local attach and workspaces (fux, after F2 and F3)

`fux/src/local.rs`, `fux/src/manager.rs`. Bare `fux [name]` finds the per-user server via a pid
and endpoint file in `$XDG_RUNTIME_DIR/fux/`, starts it detached if absent (the same binary,
`fux serve --daemon`), then attaches over iroh loopback with `connect_with`. The server hosts
many named workspaces, one `WorkspaceHost` each, selected by a first frame or by the ALPN suffix
if koh's ALPN list makes that simpler; document which. `fux` with no name opens the only
workspace or a picker popup. Detach with prefix `d` leaves everything running.

Tests: start a server, attach twice from loopback, edit from one, see it on the other; kill the
client, reattach, state intact; the daemon exits when its last workspace is killed.

## Phase F5: control socket, config, hooks (fux, after F4)

`fux/src/control/`, `fux/src/config.rs`. The socket, commands and events exactly as in
`design.md` *The socket*; `fux ctl <command>` and the top-level verbs (`new`, `split`, `focus`,
`zoom`, `kill`, `send-keys`, `capture`, `list`, `tab`, `popup`, `subscribe`) are one thin client
over it. Config is TOML at `$XDG_CONFIG_HOME/fux/config.toml`: prefix key, bindings to built-ins
or external commands run with `FUX_PANE`, `FUX_SOCKET`, `FUX_CWD`, the default pane command, the
zor path, and `hooks` restarted on exit. The notifier fires on `agent.state` transitions into
blocked and idle: `notify-send` when a display is present, `terminal-notifier` then `osascript`
on macOS, `termux-notification` on Termux, on the host for the desktop case and in the client
for the phone case, exactly as `design.md` *Notifications* says.

Tests: every command against a running workspace over the socket; `subscribe` delivers
`pane.opened`, `agent.state` (from a zor OSC written into the pane) and `pane.closed` in order;
a binding to an external command receives the three variables; the notifier is exercised with a
fake command path.

## Phase I: integration (after Z and F5)

1. `fux` spawns the real `zor` binary from `zor/target/debug` in a test, runs `printf` of a
   working then idle OSC 7877 inside it, and asserts the transitions arrive on `subscribe` and in
   the status line snapshot.
2. Copy mode and scrollback: prefix `[` enters a per-pane viewport with keyboard and Shift-drag
   selection; copy goes out over koh's OSC 52 path. zellij's grid edge cases (wide glyph at the
   margin, wrapped-line selection, resize with scrollback) each get a test.
3. `README.md` for both crates from their *Surface* sections, `CHANGELOG.md` entries for zor
   0.1.0 and fux 0.1.0, CI for both (fmt, clippy, test on Linux and macOS, the layering grep for
   zor, an `aarch64-linux-android` `cargo check` for fux), `cargo publish --dry-run` green for
   both with zor's path dependency switched to a version once zor is published.

## Deliverables

- Both crates green on `cargo test` on this machine, with every `#[ignore]` listed and justified.
- A closing summary per crate: each deviation from its design document and why, every open
  question you resolved, and what needs a human (Claude Code captures for the rule set, the OSC
  7877 collision check across terminals, the 21337 provenance, the crate name publish).
- No change to `fux/docs/design.md` or `wrapper-design.md` except notes under *Open questions*.
