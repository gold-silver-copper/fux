# Interaction restoration verification

Base: merged PR #20 (`28895af`), branch `minimal-bevy-fux`, not original `main`.
Implementation branch: `restore/interactions`. Original main's commands, client
interaction/copy behavior and README were used as behavioral references; its
controller, protocol and layout engine were not restored.

## Requirement audit

| Area | Implementation and direct verification |
| --- | --- |
| Workspaces and tabs | Reflected `Workspace → Tab → Split/PaneView`; ordered native children and `WorkspaceOrder`, shared deterministic workspace ordering. `navigation/tests.rs`, `assets/tests.rs`, and `tests/design/interactions.rs` verify creation, rename, ordering, chooser/click selection, independent viewer memory and scene replacement. |
| Bar, overflow and tiny views | `chrome/tests.rs` checks Unicode cell widths, active-tab visibility, pick bounds and every short help height. Design tests cover zero/one/two-cell dimensions, bottom-bar styles, clipped wide characters, native separators and cursor positions. |
| Native scenes | Native tab scene round trip tests check names, order and remapped process references. Legacy migration preserves children and moves the complete original layout Node exactly once. Invalid nested tabs/runtime viewers are rejected. Processes and viewer runtime components are not serialized. |
| Focus | Previous/next use native tab navigation; directional focus uses native computed rectangles and deterministic ties, without edge wrap. Integration tests exercise previous/last/directional focus, hidden-node exclusion with actual child input files, restored memories, stale entities, scene replacement, and zoom reset on tab switches/moves. |
| Safe closes | Captured entity/scope confirmations identify the subject, cancel on stale targets, and isolate input. Unit tests cover every close scope, cancellation, shared references, final-reference destruction, empty last-tab replacement and last-workspace viewer repair. Integration tests change focus after opening confirmation, reject wrong-kind targets, inspect child bytes and distinguish explicit API closes. |
| Help and menus | Shared `actions.rs` labels/groups/availability drive help, menus and dispatch. Tests inspect dimmed cells, custom binding hot reload, grouping, literal prefix, unknown keys, explicit scrolling, Unicode truncation and tiny chooser reachability. Captured unfocused menu subjects and stale destinations are exercised. |
| Rearrangement | Tests exercise native child reordering, directional moves/swaps, nested swaps and moves to existing/new tabs/workspaces. Process entity IDs/PIDs and retained terminal text are asserted, not just pane counts. Native redundant split collapse and surviving shared references retain ownership semantics. |
| History and selection | One bounded vt100 cell snapshot per copying viewer, not another history. Unit tests check wide continuations, combining characters, wrapped/hard lines, clipped backing-only cells, offsets and limits. Integration tests exercise keyboard/drag copying, simultaneous independent viewer selections, output/resize/eviction invalidation and visible notices. |
| Application input and clipboard | Byte-exact tests distinguish application mouse forwarding from Shift drag/copy ownership. Clipboard is explicitly disabled or write-only, capped at 1 MiB encoded per effect and 16 queued effects; policy/limit tests and repeated-copy delivery tests cover these boundaries. |
| Incomplete paste | Bounded envelope parser tests split every marker boundary and drain oversized pastes. Captured prompt serial/pane ownership rejects cancellation/reopening/focus redirection. The actual attached frontend test starts a fragmented paste, cancels its owner while incomplete, completes the paste, and verifies it never reaches the child. |
| Real frontend and lifecycle | Two real outer-PTY attached frontend tests check rendered ANSI, prefix/menu/confirmation/copy behavior, resize, detach and terminal restoration. Existing process cleanup, stock ECS mutation, scene remapping, PTY-size negotiation, blocked paint/hot-output and watch delivery regressions pass. |
| Architecture | Native hierarchy/layout/focus/picking/scenes/assets and stock BRP remain authoritative. Navigation, overlays, paste ownership and selection are viewer-local unreflected components. Processes remain separate entities. Input uses readiness-driven Unix poll with a stop socket/SIGWINCH, not recurring idle polling; server wake/coalescing remains event-driven. No new Rust dependency was added; existing nix enables its poll feature. |

## Executed gates

On macOS arm64, from the interaction worktree:

```sh
cargo fmt --check
RUSTC_WRAPPER= CARGO_TARGET_DIR=../fux-minimal-bevy/target cargo clippy --locked --all-targets -- -D warnings
RUSTC_WRAPPER= CARGO_TARGET_DIR=../fux-minimal-bevy/target FUX_DESIGN_CAPTURE="$PWD/verification/interactions" cargo test --locked
# Then ten additional complete cargo test --locked --quiet runs.
RUSTC_WRAPPER= CARGO_TARGET_DIR=../fux-minimal-bevy/target cargo build --release --locked
git diff --check
```

**29 unit and 29 integration tests pass**, including all ten additional complete
runs. Logs: [tests](interactions/tests.log), [repeated tests](interactions/repeated-tests.log),
[Clippy](interactions/clippy.log), [release build](interactions/build.log).
The wrapper override bypasses an unrelated local sccache stall.

## Render and input evidence

[Contact sheet](interactions/renders.png) was manually inspected for tab emphasis,
workspace chooser ordering, menu placement/overflow, confirmation scope, Unicode
selection, grouped/dimmed help, narrow help and tiny bars. It is a rasterization
of captured ANSI-decoded cells, not a desktop screenshot; font metrics can leave
small gaps in box-drawing strokes. Matching `.ansi`, `.txt` and `.json` artifacts
retain source bytes, cells/styles and cursor metadata. The optional
`render-interactions.py` script uses Pillow outside the Rust application.

`frontend-interactions.ansi` is actual attached frontend output; frontend help
and confirmation text captures are adjacent. Tests inspect decoded cell styles,
cursor positions, OSC52 payloads and actual child input files. Render snapshots
are supplemental evidence, not the sole interaction assertions.

## Intentional differences and limits

- Preserve PR #20 defaults; added bindings and unbound menu actions are listed in
  README. Workspaces really contain tabs, rather than relabeling workspaces.
- Directional focus uses deterministic native rectangle centers without wrapping;
  tab switches and pane moves exit zoom. Closing the last tab leaves an empty tab;
  closing the last workspace detaches affected viewers.
- Selection is limited to the displayed viewport. vt100 lacks stable row IDs;
  output/style changes, scroll, resize or eviction conservatively invalidate it
  with a notice rather than silently changing selected text. This is not
  paragraph reflow or persistent cross-viewport selection.
- Clipboard writes require explicit opt-in and outer-terminal OSC52 support;
  there are no clipboard reads. Overlay rows are keyboard/wheel-operated, not
  clickable; native tab/workspace/pane picking remains supported.
- Paste content is bounded and drained until its end marker. A lone Escape uses
  a 35 ms disambiguation deadline; cancellation during a fragmented paste is
  ownership-safe, not reinterpretation of pasted Escape bytes as commands.
- Linux/other Unix platforms were not run. No new benchmark comparison or broad
  terminal compatibility certification is claimed. Existing process-containment
  and cross-version scene limitations remain documented in README.
