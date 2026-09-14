# Changelog

## 0.11.0 - 2026-09-14

Simplification pass (`docs/codebase-simplification.md` records every batch, its line count and
its behaviour differences).

- Fixed a viewer that stopped updating a pane. When a control-socket read (`list`, `capture`)
  arrived in the same step as paced output, it refreshed the pane grid and cleared the flag
  frame publication relied on, so that output was never sent until something else changed.
  Publication now compares each shown pane's grid sequence with what the viewer was sent.
  The bug predates this release.

- Attachment frames and `cells` captures carry a non-default cell style as
  `[foreground, background, attributes]` (a colour is `null`, a palette index or `[r, g, b]`;
  attributes are a bitset: bold 1, dim 2, italic 4, underline 8, inverse 16; unknown bits are
  rejected). Styled cells shrink from about 110 to about 15 bytes. A 0.10 viewer or consumer
  cannot decode 0.11 frames or `cells` captures.
- Public API: `ecs::systems`, `ecs::support`, `os`, `server::adapter`, `server::connections`
  and the `client` submodules are crate-private; `client::{attach, attach_reported,
  AttachOptions}` and `server::{run, ServeOptions}` stay public. Removed with no production
  caller: `Event::kind`, `EventKind`, `ErrorCode::Timeout`, the `ManagerAction`/`ManagerOutcome`
  mirrors of `ManagerRequest`/`ManagerReply`, `Session::workspace_names`,
  `LayoutAction::is_read_only`, `LayoutTree::cycle` (now `next_leaf`/`previous_leaf`).
- Control protocol: the six tracked operations (`fix-workspace`, `rename-pane`, `pane-input`,
  `input-reserve`, `input-submit`, `input-status`) decode without `instance` and then fail
  validation with `invalid-request`, like `events` and `split`, instead of a JSON field error.
  Every frame type is bounded by the 1 MiB limit on write. A viewer whose request queue
  overflows receives one `CloseViewer`, not two. A detaching viewer no longer counts toward
  the per-workspace viewer limit on the select/transfer path.
- CLI: every subcommand is parsed by clap with accurate `--help`; `split`/`new` accept `h`/`v`
  and a repeatable `--env`; `send-keys` takes exactly one escaped string unless `--keys`;
  `capture --cells --attrs` is rejected at parse time; invalid key notation in the config is
  reported by the TOML decoder with the key path; a manager reply carrying a failed nested
  control reply exits non-zero for every such variant.
- Viewer: a pasted Enter no longer submits the tab-rename field; a horizontal wheel no longer
  steps the swap picker or the menu; a chooser press after a stale-target dismissal adopts the
  left capture uniformly; ESC ESC inside a mode dismisses it after the Escape timeout.
- Server: a stale manager socket whose probe fails with an unexpected error is reported as an
  error rather than treated as "not running"; a request from a viewer that vanished in the
  same step gets no reply rather than a validation reply.
- Repository: gate logs and dated acceptance reports are no longer tracked
  (`docs/verification.md` indexes them in history); `tests/ecs.rs` is split by topic.
- Requires local-ipc 0.3.0.

Previously unreleased:

- Complete mouse Close workflows for panes, tabs and workspaces with clickable confirm/cancel
  rows, outside-click cancellation and captured releases. Stale or unpainted dialogs cannot
  reuse a previous menu's click targets.

- Allow keyboard focus navigation while zoomed and next/previous wraparound with a single
  pane. Navigation availability follows the focused pane rather than visible split count.

- Complete mouse interaction in tab/workspace choosers: wheel navigation, destination clicks
  and outside-click cancellation, with captured releases kept out of terminal applications.

- Add explicit transfer `--focus`/`--no-focus` and API focus controls. No-focus preserves
  destination zoom; focused transfers reveal/select the moved pane with atomic navigation
  history. Transfers carrying viewers enforce destination capacity before movement.

- Add `--ratio` to existing-tab transfers through `layout to-tab`, `transfer-pane` and their
  APIs. The existing target keeps its requested share for every insertion direction, while
  the moved pane retains its process and identity.

- Add initial split/new `--ratio` and `--focus`/`--no-focus` controls. No-focus creation preserves
  selection, zoom and queued-input ownership; focused creation selects hidden target tabs for
  workspace controls and reveals the new pane without changing other viewers' selections.

- Add per-pane right-click policy (`auto`, `fux`, `pane`), controlled by prefix `*`, the pane
  menu, `pane-input` CLI/API and split/new creation options. Policy follows live pane movement;
  Alt-right-click always opens the menu. Layout imports preserve the current policy.

- Add last-focus navigation through prefix `!`, CLI `focus last` and the control API.
  Attached viewers retain private history across tabs/workspaces; workspace control connections
  use scoped default history. Deleted targets and full destination workspaces reject safely.

- Add `fux layout TAB inspect PANE` and the read-only layout `inspect` API for coherent pane
  rectangles, directional neighbors, outer-edge flags and zoom visibility with layout identity.
  Inspection shares directional focus rules and preserves processes, layout and private focus.

- Keep EOF processes locatable for owned cleanup while explicitly reporting that they no longer
  accept input. Moved `zor run` timeout cleanup now terminates processes that close their terminal
  descriptors; verified server replacement does not turn a completed run into cleanup failure.
- Use one coherent manager identity check for native worker liveness, avoiding false retirement
  when a pane moves between route discovery and workspace observation.

- Allow active `zor run` panes to move between workspaces. Final output/status retain launch
  identity; timeout cleanup follows only the owned pane and preserves destination processes
  and any replacement of the original workspace.

- Release managed-launch creation pins only after exact pane/PID identity is durable. Attached
  managed panes can move and retain prompt delivery, reconciliation and stop/final evidence.
  Lost release requests/replies retry without spawning; explicit workspace pins remain protected.

- Let newly adopted zor tasks follow panes across workspace moves, retaining exact server/pane/PID
  and launch attribution. Route-aware live requests and final evidence preserve task ownership;
  managed launch pins remain until creation recovery can safely follow movement.

- Preserve delivered prompt input sequences during workspace moves while permanently failing
  unused reservations. Add manager receipt reads keyed by server/pane/operation, used by zor
  reconciliation after the originating workspace socket disappears.

- Add read-only `locate-pane PANE --instance INSTANCE` and manager `pane-location` lookup.
  It returns the current workspace/tab and immutable launch attribution for the same live
  pane/PID, including after the original workspace retires.

- Add shared workspace display labels through prefix `=`, the workspace menu and guarded
  `workspace rename` CLI/API. Labels appear in the bar and choosers, survive archive restore,
  and preserve workspace routes and live processes. Unchanged labels add no delta-frame bytes.

- Avoid emulator and PTY resize requests when layout changes only move a pane's position.
  Equal-sized swaps still publish the new arrangement to viewers.

- Add workspace close confirmation through prefix `q`, the workspace menu and guarded
  `fux workspace close NAME --instance INSTANCE --stream STREAM`. Full attachment frames carry
  the workspace lifetime so stale confirmations cannot target a replacement with the same name.
  Closing affects only that workspace and detaches its viewers.

- Add an explicit pane swap chooser through prefix `.` and the pane context menu. Keyboard and
  mouse selection use captured pane IDs and a layout revision, allowing nonadjacent swaps
  without intermediate moves while preserving both processes.

- Add contextual pane/tab/workspace menus with keyboard navigation, mouse selection, disabled
  reasons and captured target identities. Ordinary application right-clicks remain available;
  Alt-right-click explicitly opens the pane menu. Prefix `?`, `'` and backtick open the menus
  from the keyboard. Tab close confirmation describes all of the targeted tab's panes even
  when that tab is not selected.

- Add manual pane labels through prefix `;`, `fux rename-pane`, and the guarded `rename-pane`
  control request. Labels stay separate from application titles, follow live pane moves and
  clear back to the current application title. Renaming preserves PTYs, processes and contents.
  Listings and viewer/history updates expose the label independently.
- Include manual pane labels in complete layout exports and workspace archives. Imports restore
  labels atomically with geometry and zoom, apply pane-ID remapping, and reject duplicate,
  foreign or invalid labels. Bare tree imports preserve current labels; renames advance the
  layout revision to prevent stale imports from overwriting newer names.

## 0.10.0 - 2026-09-11

Breaking release: retention durations become the caller's policy under fux-enforced ceilings.
fux keeps the caps (128 receipts, 128 final records: they bound server memory against any
client); how long each item lives is chosen by whoever created it. No compatibility with 0.9.0
request shapes is kept.

- `input-reserve` requires `retain_ms` (u64, milliseconds). `0` is `invalid-request`; values
  above the new ceiling `MAX_INPUT_RETENTION_MS` = 600 000 (ten minutes) are clamped, and the
  receipt's `expires_ms` is the reservation time plus the applied value, so the clamp is
  visible. Ten minutes covers any single input round trip and its reconciliation many times
  over while keeping a stuck client from pinning receipts (and their submitted bytes) for hours.
  Removed `INPUT_RETENTION_MS` (60 s, applied to every receipt).
- `split` requires `final_retain_ms` (u64, milliseconds), stored on the pane and applied when
  its final record is created at close. `0` is `invalid-request`; values above the new ceiling
  `MAX_FINAL_RETENTION_MS` = 14 400 000 (four hours) are clamped. Four hours lets a supervisor
  that was down reconnect and still read exit evidence, while a record's bounded capture
  (128 KiB) times the cap of 128 stays a fixed worst case however long the durations.
  Removed `FINAL_RETENTION_MS` (60 s, applied to every record).
- New configuration key `[final] retain-ms` (1 through 14 400 000, default 60 000): the
  `final_retain_ms` of the panes fux creates itself (a workspace's initial pane on `workspace
  new`/`resolve`/manager `create`, a new tab's pane, the viewer's `split-side`/`split-stack`,
  the CLI's `fux new`/`fux split`). It lives in `Config` because those panes have no protocol
  caller to state a policy, and fux's interactive users keep the previous 60 s behavior by
  configuration; automation must choose per pane on `split`. Only the `new`/`split` CLI
  aliases read the configuration; the other aliases still run without a config file.
- `info.limits` gains `input_retention_ms` and `final_retention_ms`, the two ceilings, so a
  client can size its policy without guessing; zor pins its policy to them.
- Fixtures: `request_split.json` carries `final_retain_ms`; new `request_input_reserve.json`
  and `reply_completed_info.json` (the latter also copied to `crates/zor/tests/fixtures/
  control/`, kept byte-identical by the fixture suite). The consumer fixture records zor as
  the consumer of `info.limits` and its two ceilings.
- `final` explains a missing record. New error codes `evicted` (the 128-record cap dropped the
  record under load before its `expires_ms`) and `unknown` (this server never retained a record
  for the id, or has forgotten that it did); `expired` now means only that a record existed and
  its retention elapsed. fux remembers, per server instance, the most recent 1024 evicted ids
  and the most recent 1024 expired ids (`MAX_FORGOTTEN_FINAL_IDS`; two rings, 4 KiB each), and an
  id that falls off its ring answers `unknown`. The rule is exact: no id is reported `evicted`
  or `expired` without a record having been made for it. Capacity eviction now sweeps expired
  records first, so it only ever drops a record that was still valid. `pending` and `conflict`
  are unchanged. The `final-records` automation scenario checks `unknown` for a never-recorded
  pane.
- zor 0.5.0 ships from this workspace; see `crates/zor/CHANGELOG.md`.

## 0.9.0 - 2026-09-11

Breaking release: dead automation surface is deleted and the two workflows fux still carried
move to zor. fux keeps only what needs the PTY, the process, the retained grid or the event
log. No compatibility with 0.8.0 consumers is kept.

- Removed request `wait` (`WaitUntil::{Exit,Seq}`, `WaitFired`, the `waited` result, the
  `Waits`/`PendingWait` resources, `MAX_WAIT_MS`, `MAX_PENDING_WAITS`, `MAX_WAITS_PER_PANE`,
  the ECS `Waits` phase and the blocking-connection path); every request is now answered or
  failed within the fixed 30 s window. CLI: `fux wait` is removed. Consumers poll `seq`/
  `revision` or follow `pane.output`.
- Removed the `events` filter from `subscribe` (and `MAX_EVENT_FILTERS`): a subscription
  receives every event of its workspace. CLI: `fux subscribe` takes no arguments.
- Removed event kinds `pane.title`, `client.attached` and `client.detached` (`Event` and
  `EventKind`); a title change advances the pane's output sequence and is read from `list` or
  a capture, viewer counts from `list`.
- `info.limits` keeps only the bounds a client sizes requests by: `scrollback_lines`,
  `frame_bytes`, `capture_bytes`, `key_bytes`. Removed fields: `workspaces`, `tabs`, `panes`,
  `viewers`, `control_connections`, `event_filters`, `subscriber_queue`, `viewer_queue`,
  `retire_grace_ms`, `terminate_deadline_ms`, `output_event_interval_ms`,
  `frame_interval_ms`.
- Removed CLI subcommands `fux run` (with its `--timeout`, `--workspace`, `--cwd`, `--env`,
  `--rows`, `--columns` flags) and `fux final --instance NONCE PANE`. `zor run` (zor 0.4.0)
  is the replacement for `run`, with the same observable behavior; manager `final`, the
  retained final records and their bounds stay in fux as the primitive zor reads.
- `progress` stays on both capture forms (zor's rules read it).
- zor 0.4.0 ships from this workspace; see `crates/zor/CHANGELOG.md`.
- `FinalRecord` no longer carries `closed_ms` and `expires_ms`; they were server-side
  retention bookkeeping with no consumer, and stay internal.
- Socket plumbing now comes from local-ipc 0.2.0: `connect_local`, `write_all_until`, the
  manager frame reader and runtime-directory discovery are the shared `connect_until`,
  `write_all_until`, `FrameReader` and `runtime_directory_from`; behavior, limits and error
  texts are unchanged.
- The fux integration test crate is `automation_integration` (it tests automation over the
  control protocol, driven by zor); `tests/fixtures/control-consumers.json` records every
  protocol item's consumer and the `protocol_consumers` test fails on any item with none.


## 0.8.0 - 2026-09-11

Breaking release: the fux/zor boundary is tightened and unconsumed surface is removed. No
compatibility with 0.7.0 consumers is kept.

- `capture` gains `format:"cells"`: the visible grid as fux's wire cells (text, kind, style,
  run-length blanks) with cursor, size, title, progress, revision, grid sequence, input
  sequence, `unchanged` and `truncated`, served from the same pane borrow as the text form.
  `max_bytes` bounds the encoded lines (whole lines dropped from the bottom, `truncated:true`).
  `cells` rejects `scrollback`, `attrs` and `if_revision` behaves as for text. CLI: `fux
  capture --cells`.
- Removed: `capture format:"rows"` and `since` (`CaptureRow`, `since_applied`, the CLI
  `--rows` and `--since` flags); the `progress` field of `list` pane summaries (progress stays
  in both capture forms); `wait` conditions `pattern` and `quiet` with their limits
  (`exit` and `seq` remain; `fux run` never used them and is unchanged); the `regex-lite`
  dependency.
- New workspace crate `local-ipc` (0.1.0): the same-user local-socket discipline (private
  0700 directory, 0600 socket with inode-scoped cleanup, peer-uid check, random token,
  nonblocking connect) shared by fux and zor with no wire, path, permission, limit or
  error-text change. fux depends on it.
- `docs/design.md` states what fux promises automation consumers and what zor consumes.
- zor 0.3.0 ships from this workspace, with its PTY wrapper behind the `wrap`
  feature (`zor wrap <command>`); see `crates/zor/CHANGELOG.md`.

## 0.7.0 - 2026-09-09

Native capabilities on main, a release performance pass, and zor in the same repository.

- Generic reliability for unattended consumers (#4): server and workspace incarnation identity
  with stale-instance rejection; coherent conditional `capture` with a terminal revision separate
  from the grid sequence; tracked input reservation, submission and status with exact
  partial-delivery accounting, idempotent duplicate submission and intervening-writer detection;
  bounded event replay with cursors, gaps and reconnect semantics; retained final output and
  exit records that `fux run` consumes; cancellable stalled PTY writes on Linux and macOS; split
  UTF-8 handling in the reusable parser; empty child arguments accepted. Agent policy stays in
  zor: OSC 7877 parsing and pane agent state/events are removed from fux.
- Verification and measurement tooling is Rust (`tools/xtask`): the durable gate, real-process
  scenarios, evidence validators and the `measure`, `measure-frames`, `measure-viewer`,
  `measure-memory` and `measure-koh` benchmarks; the original Python scripts are archived with
  provenance.
- Performance (#4, #5): exact event size accounting without a temporary encoding or second
  serialization; UTF-8 continuation state from the last four bytes instead of a full rescan; a
  non-exclusive input-completion system; retained grid cells compared in place and empty cells
  classified without validation. Release micro-timings: `EventLog::push` 136 to 77 ns,
  `ServerTerminal::process` 20 to 10 ns per byte versus 0.6.0, `Grid::refresh` 18 to 19 percent
  faster on typical rows (3 to 11 percent on full-width rows), ECS keystroke step 34 to 30
  microseconds.
- Repository layout (#6): a virtual Cargo workspace with `crates/fux` and `crates/zor`. zor was
  imported with its history and its fux integration patch; the zor pin and patch workflow are
  gone. koh remains a separately pinned companion. Default CI runs the real zor integration on
  Linux; `cargo install --path crates/fux` replaces `--path .`.

## 0.6.0 - 2026-09-06

Protocol and agent-surface pass. fux is now a first-class headless target and the local protocols
carry no version numbers.

- No protocol versioning. The control and manager sockets exchange a fixed `FUX\n` preface; the
  attachment `hello` carries only the terminal size. Descriptors drop the protocol field. The
  migration module, the interactive "stop the older server" offer and every `--version` flag of
  the measurement scripts are gone. A server older than its client shows up as a decode error
  reported as "restart the session server"; nothing is stopped automatically.
- Agent primitives on the control protocol. Each pane has a monotonic output sequence that
  advances for every observable change (rows, cursor, modes, title, exit) and is reported by
  `list`, `capture` and `pane.output` events. `capture` gains `format:"rows"` ({row,text,wrapped}
  plus the cursor and sequence) and `since` (only the rows changed after a sequence); the text and
  attrs forms are byte-identical to before. A new `info` request (both sockets) reports the server
  pid, nonce, crate version, runtime directory and every limit. A new `wait` request blocks
  server-side until a pane goes quiet, matches a pattern, exits or reaches a sequence, or times
  out; it is an ECS deadline, never a thread.
- `new`/`split` accept `env` and an initial `rows`/`columns` for headless workspaces; `send-keys`
  accepts `notation:"keys"` (named keys, `C-`/`M-`, literals) beside the default byte escapes.
- Schema minimised: the pane `new` request folds into `split` (kept as CLI sugar) and `tab
  select-id` folds into `tab select {target}`; the manager socket gains `info`. The manager stays
  a small bootstrap RPC because its attach reply carries a descriptor the shared schema omits.
- Agent state: fux reads OSC 7877 reports (zor's schema) from pane output and surfaces the state
  in `list` and a `pane.agent` event, so `zor -- COMMAND` or a self-reporting agent lights up fux
  with no observer socket. It is unverified presentation only; carrying it in the attachment frame
  and the viewer bar is deferred.
- `fux run -- COMMAND` runs a command headlessly in a sized pane with an environment, streams its
  screen, waits for exit, prints the final screen and exits with the command's status.
- Shared protocol fixtures under `tests/verify/fixtures/`, round-tripped by `tests/fixtures.rs`.
  `zor observe` re-captures only when a pane's output sequence advances (idle CPU 0.26 s / 20 s).
- Added `regex-lite` (linear-time, no transitive deps) for `wait` patterns; MSRV 1.95 and the
  forbidden-`unsafe` lint are unchanged.

## 0.5.0 - 2026-09-06

Performance pass. Attachment protocol v6; control `FUXCTL2`, keys, configuration and CLI are
unchanged.

- Frames carry only what changed: the server keeps one retained grid per pane, read from the
  emulator once per step with every row stamped by the step it changed in, and each viewer
  remembers what it holds, so an update carries a pane only when it changed and then only its
  changed rows, as compact wire cells (blank runs, styles omitted when default). The viewer
  keeps its frame and applies updates; queued updates are merged rather than dropped; the
  bindings are sent once after the hello; cells are validated once, where they are produced.
- Output feeding uses a reusable buffer per pane and no longer clones the title per chunk.
- Measured on an M2 Max (release builds, `tools/measure*.py`, baseline 0.4.0 → 0.5.0, ranges
  over the day's alternating runs): bytes on the attachment socket per keystroke 289,920 → 852
  at 80×24 and 1,853,858 → 855 at 200×60; input-to-frame latency median 2.8–6.2 → 0.14–0.25 ms
  at 80×24, 21–45 → 0.32–0.47 ms at 200×60, 21.8–35.8 → 0.16–0.30 ms with eight viewers on one
  tab; server CPU per 1,000 keystrokes 0.94 → 0.08 s (6.99 → 0.09 s with eight viewers); server
  CPU for a 20,000-line burst 0.21–0.23 → 0.06–0.09 s; the real viewer's CPU per 1,000
  keystrokes 1.05–1.48 → 0.25–0.33 s (5.9 → 0.87–1.07 s at 200×60); idle CPU stays 0.00 s;
  memory per retained history row unchanged (vt100's 32 bytes per cell). The burst's wall time
  stays at the shell's own floor.
- New measurement tools: `tools/measure_frames.py` (bytes and CPU per keystroke by screen size and
  viewer count), `tools/measure_viewer.py` (the real viewer on a pty) and
  `tools/measure_memory.py` (bytes per retained history row).

## 0.4.0 - 2026-09-06

Internal architecture only: no protocol, configuration, key or CLI change (attachment v5,
control `FUXCTL2`, the same default bindings and behaviour).

- ECS systems are typed: output, layout, snapshot and viewer arrival/departure run as ordinary
  systems over `Query`/`Res`/`MessageReader` with `SystemParam` bundles (`Step`, `Scene`,
  `Arrivals`, `Effects`, `ViewerExit`); only request execution, spawn completion and the lifecycle
  cascade keep `&mut World`. Viewer queues drain through the schedule instead of tail calls.
- Workspace → tab membership is a bevy relationship (`TabOf`/`Tabs`) instead of a hand-kept
  `Vec<Entity>`.
- One set of helpers replaces repeated code: viewer scans and cascades in `ecs::support`, the
  viewer's text layout in `client::text`, the `actions!` table that generates the command enum,
  labels, groups and default bindings, `serde(default)` config merging, `thiserror` error types,
  one accept loop and one framed write for the sockets, one private-directory check.
- Removed with no caller: the historical design and prompt documents (git history keeps them),
  `CONTROL_VERSION`, the synchronous control reader, the server half of the client negotiation,
  test-only public helpers and unread capture-backend fields.
- The runtime, state and descriptor directories are now checked by the same rule as socket
  directories: a real directory, mode 0700, owned by the effective user (before, ownership was
  compared with the parent directory's owner).
- `src/` shrinks by about 640 lines (4 %); the markdown documentation by about 5,000 lines.

## 0.3.3 - 2026-09-06

- The command popup becomes a bottom-right column above the bar: one row per binding under its
  group heading, as wide as its widest line, as tall as its content, no title or footer rows. It
  scrolls one row per arrow and a screenful per page key only when the terminal is too short,
  with `▲ n more` / `▼ n more` rows marking hidden entries. The choosers, prompts and close
  confirmations use the same corner box, keeping their title and key-hint rows.
- Keys are matched without Shift: `X` triggers the `x` binding, `\` the `|` binding, `_` the `-`
  binding. Bindings that differ only by Shift are rejected, so close tab moves from `X` to `c`
  and new workspace from `S` to `a`.
- The `?` "show bindings" action is removed: the prefix itself shows the column and any unknown
  key keeps it open. A configuration that binds `help` no longer parses.
- A viewer drops binding actions it does not know instead of rejecting the frame, so a server of
  another 0.3.x release (with a different action set) still attaches; the keys shown are the
  server's until it is restarted.

## 0.3.2 - 2026-09-06

- The bar moves to the bottom row and gets its own background (`[style] bar-background`, default
  `bright-black`, with `bar` now defaulting to `white`). Popups sit above the bar. Pane content
  starts at row 0; the attachment protocol is v5 for that change of rectangle contract.

## 0.3.1 - 2026-09-06

- Replace the per-pane boxes with an always-visible top bar (workspace, tabs with the current one
  reversed, focused pane `id: title`) and shared one-cell separators between panes, bold next to
  the focused pane. Panes gain the rows and columns the frames took.
- Transient notices (copy results, errors, workspace switches) show in the bar's right zone for
  two seconds or until the next key; the bottom notice bar and the "Command failed" popup are gone.
- New `[style]` configuration table (`bar`, `tab-active`, `separator`, `separator-focused`,
  `notice`) with muted defaults.
- Mouse coordinates and history selections use the leaf rectangle directly. Because the meaning
  of frame rectangles changed, the attachment protocol is now v4; a viewer meeting a 0.3.0
  server gets the same interactive dialog as for a 0.2.x server.

## 0.3.0 - 2026-09-05

Complete rewrite as a minimal persistent multiplexer whose authoritative model is a standalone
`bevy_ecs` 0.19.1 World (workspaces, tabs, panes and viewers are entities; typed inbound messages
and effects; one explicitly ordered single-threaded schedule per event-driven step).

- Attachment protocol v3 (`input`, `mouse` with layout generation, `control`, `view`, `resize`,
  `detach`; per-viewer frames) and control protocol `FUXCTL2` (`tab`/`workspace` command
  families, `select-id`, `workspace select` for viewers). Version 2/`FUXCTL1` are not served.
- Viewer-private active tab, focus, menus, history position and selection; pane geometry
  negotiated over the smallest viewer showing a tab; hidden tabs keep their last size.
- Creation barrier: input queued behind a split, new tab or new workspace reaches the new pane
  only after the process started; failures roll back without a phantom pane.
- Natural exit of the last pane retires the workspace with its exit status after viewers saw the
  final frame; confirmed close and kill terminate process groups with a reap gate.
- Immediate keybinding popup with the workspace name; command bursts apply before repaint;
  prefix twice sends the literal prefix; unknown keys keep the popup; Esc backs out.
- Deterministic ECS suite with a randomized command-sequence test, real-process fixture scenarios,
  required real koh and real zor integrations with explicit binary paths.
- Removed: floating popup panes, full-screen pickers and the startup picker, external command
  bindings, lifecycle hooks, desktop notifications, agent dashboards and the OSC 7877 adapter,
  zor sidecar supervision (`zor-path`), status segments, hint delay settings, SIGHUP config
  reload, `tokio-util` and `loom`.
- Interactive handling of an older, incompatible session server: explain, list its recorded
  workspaces, and offer to stop it after typed confirmation or to run alongside it.
- MSRV 1.95 (required by bevy_ecs 0.19.1).

## 0.2.1 - 2026-09-04

- Make concurrent first-client startup elect exactly one daemon and workspace.
- Preserve real pane exit status across explicit workspace teardown.
- Handle SIGINT and SIGTERM throughout daemon startup and roll back owned resources.
- Expand deterministic binary verification for detach/reattach, copy mode, process ownership,
  natural retirement, remote reconnection, and startup interruption.

## 0.2.0 - 2026-09-03

- Add bounded synchronized workspace state, BSP layouts, diffs, tabs, popups, status, clipboard,
  bell and agent-state metadata.
- Add koh-backed pane hosting, streaming input routing, terminal reply handling, scrollback capture,
  and bare-pane fallback when zor is unavailable.
- Add a terminal compositor/client with prediction, detach handling, copy mode, and OSC ledgers.
- Add strict newline-delimited JSON control requests, replies, subscriptions, private filesystem
  authorization, startup-channel peer checks, and bounded slow-consumer queues.
- Add named-workspace daemon descriptors and one endpoint identity per workspace.
- Add deterministic state/router chaos tests and adversarial protocol/resource-bound tests.
