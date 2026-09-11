# Changelog

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
