# Complete fux pane and layout controls

Implement a complete, coherent pane/layout experience in fux, matching or improving Herdr's pane/layout controls. Deliver working code, documented keyboard/mouse/CLI behavior and meaningful verification. Do not stop at a proposal, prototype or comparison document.

This task is specifically about pane/layout controls. Multi-machine management, provider integrations, session/process restart restoration, direct terminal controller leases, transcript retrieval, graphics and plugin systems are outside scope. Layout serialization is in scope; launching processes from saved sessions is not. Do not claim whole-product Herdr parity from completing this task.

Backward compatibility and breaking semver are not concerns. Preserve user work, live processes and the existing fux/zor/koh boundaries.

Treat this as an implementation task only when this prompt is executed. Begin by inspecting the current worktree: some controls may already be implemented or partially implemented. Validate and finish that work rather than overwriting it or assuming the historical audit describes today's code.

## Read the current implementation first

Read applicable repository instructions, `docs/capability-audit-2026-09-12.md`, required CI workflows, and current layout/protocol/viewer tests. Start with:

- `crates/fux/src/layout.rs`
- `crates/fux/src/ecs/systems/layout.rs`
- `crates/fux/src/ecs/systems/input.rs`
- `crates/fux/src/client/input.rs`
- `crates/fux/src/client/render.rs`
- `crates/fux/src/commands.rs`

Follow their dependencies into pane identity, tabs/workspaces, capture, event publication and PTY resizing before designing changes. Inspect Herdr's current layout UI, CLI and API under `references/herdr` to establish the behavioral checklist. Record its reference commit and any platform exceptions. Do not assume the older audit exhaustively lists its layout features.

Use a short implementation checklist connecting each behavior below to code, tests and user documentation. Make reasonable implementation decisions and continue through completion. Keep unrelated prompt files and reference checkouts unchanged. Do not terminate user sessions for verification; use isolated runtimes. Commits, pushes and PR creation require authorization from the execution session.

## Hypertile is reference code only

`references/ratatui-hypertile` contains https://github.com/nikolic-milos/ratatui-hypertile at reference commit `92fa63300f802c4465a4fbd2b928cf3ef1b4f8d0`.

Use its source code only as a reference for useful algorithms and interaction patterns, especially:

- `src/core/state/{mod,mutation,movement,focus}.rs`
- `src/core/types.rs`, `src/core/serde_impl.rs`, `src/core/state/tests.rs`
- `src/engine.rs`, `src/input.rs`
- `extras/src/runtime/{mouse,workspace,keymap,render}.rs`

**Do not depend on hypertile in any way.** No Cargo dependency, git/path dependency, submodule, build-time generation, runtime loading, wrapper around its API, or test fixture loaded from its checkout. Do not add either hypertile crate or its extras runtime to the workspace. A clean checkout must build and test identically when `references/ratatui-hypertile` is absent.

Reading that checkout during implementation is the only permitted reliance on it. Do not invoke its engine, CLI, examples or test runner as part of fux development tooling or verification. Any algorithm, helper or fixture needed by the finished implementation must be implemented locally or selectively copied and adapted into tracked fux sources. Herdr defines the comparison workflows; fux's own invariants and tests define correctness. Hypertile defines neither the feature requirements nor the architecture.

Copy and adapt only the code needed directly into fux where appropriate; there is no requirement to reuse code if fux already has a better implementation. Prefer small, understandable pieces over importing its module/runtime architecture. Adapt them to fux's ECS, stable pane IDs, geometry, event model and existing dependencies. Fux owns the resulting implementation and its behavior: hypertile must not become an architectural dependency, an API compatibility target or the source of truth for correctness. Do not change Ratatui versions or introduce a new renderer merely to accommodate copied code. Add no dependency solely to support copied reference code when the existing implementation can express it cleanly.

Do not vendor the complete hypertile crate under another name or reproduce its public API as a compatibility layer. Once a useful piece has been copied and adapted, all subsequent compilation, testing and operation must use the local fux implementation. The reference checkout must remain disposable.

For each copied or substantially adapted piece, record its original source path and commit, destination path, purpose and substantive adaptations. Keep the resulting code independently maintainable: no upstream synchronization requirement, generated bindings or reliance on undocumented upstream behavior. Test fux's own contracts using tracked, self-contained fixtures. If no code is copied, say so explicitly; referencing an algorithm does not require importing its implementation.

Hypertile uses the MIT license. Retain the required copyright and license notice for copied or substantially adapted code in an appropriate tracked attribution/license file, and identify its upstream commit and affected modules. Review copied code like new code: its widget assumptions and tests do not establish PTY or multi-client correctness. All adapted implementation and test assets must live in the normal tracked project tree.

## Required controls

Implement any missing behaviors and make existing ones consistent across keyboard, mouse, CLI and generic control API:

1. Horizontal/vertical split, close, directional focus, explicit pane focus, deterministic next/previous traversal, and return to the last focused pane. Specify traversal order, wraparound and history scope. Keep viewer history private, handle deleted targets safely, and enforce normal destination admission rules when navigation crosses workspaces. Multiple requests processed before a rendered frame must still preserve their logical focus history.
2. Directional and explicit split resizing, with predictable increments and minimum-size handling. Expose initial split ratios and deliberate focus/no-focus behavior for split and transfer operations through CLI/API. Document which pane a ratio describes, its units and bounds, and how focus options interact with zoom and private viewer selection.
3. Zoom/unzoom without destroying the underlying split tree. Restore the original arrangement and valid focus after zoom, resize, move or close.
4. Pane swap and directional relocation, with clearly distinct semantics. Define whether an operation exchanges pane identities between positions or removes/reinserts a leaf beside a target; avoid ambiguous “move” behavior.
5. Move a live pane between tabs/workspaces on the same fux server; reorder tabs/workspaces. Handle the emptied source container explicitly. Cross-host process migration is not part of this task.
6. Mouse border dragging and pane dragging/drop targeting, with clear target feedback, cancellation and sensible modifier/key conventions. Preserve mouse forwarding to terminal applications whenever fux has not deliberately captured a layout gesture.
7. Export and apply layouts for existing panes, with deterministic serialization and validation. Include split orientation/ratios, pane mapping, ordering and documented focus/zoom semantics. Plain layout import must never launch commands.
8. Discoverable help, command naming and disabled-state/error feedback. Use the existing command registry and configuration approach rather than maintaining separate conflicting lists of actions.
9. Pane labels/renaming and contextual pane/tab/workspace layout actions where supported by the Herdr baseline. Keep manual labels distinct from application-provided titles. Context menus must capture stable target identities, handle target disappearance safely, and offer keyboard navigation and cancellation. Expose equivalent mutations through CLI/control APIs; do not require a mouse to complete a layout workflow.
10. Read-only pane geometry, directional-neighbor and edge queries for automation, consistent with interactive navigation. Specify how hidden panes, zoom, zero-sized geometry and pending resizes affect results; queries must not mutate layout or focus.
11. Per-pane right-click behavior where present in the pinned baseline: explicitly choose the fux context menu or forwarding to the terminal application. Expose the policy through discoverable controls and CLI/API, preserve it through live pane movement, and define its interaction with mouse-reporting modes and layout-gesture modifiers.

If baseline inspection identifies additional material pane/layout controls, include them in the checklist and implement them. Do not expand this into unrelated terminal or agent features.

For the acceptance checklist, use one row per concrete workflow with columns for pinned Herdr behavior, fux behavior, keyboard/mouse/CLI/API entry points, code/test evidence, and remaining limitations. Mark a workflow complete only when it works end to end. A documented restriction or a primitive without a usable control surface does not establish parity.

Verify each advertised input path independently. A menu that opens with the mouse must also support completing its action, selecting a destination and cancelling with the mouse; keyboard-only completion does not prove mouse support. Exercise next/previous traversal in both directions through nested layouts, after removal and with a single pane, including wraparound and viewer isolation. Synchronize integration assertions on observed state or protocol acknowledgements rather than fixed delays.

## Identity and state guarantees

Moving, swapping, zooming and reordering must preserve the same live pane, PTY, child process and terminal contents. Do not emulate movement by closing/relaunching the child. Send PTY size changes only when the effective size changes, with deliberate behavior for hidden/zoomed panes.

Keep layouts server-authoritative. Specify which state is shared and which is viewer-local, including focus, zoom and viewport size. Define deterministic behavior when viewers differ in terminal size or perform concurrent mutations. Publish coherent layout changes and an appropriate generation/revision so stale drag/apply requests cannot mutate the wrong pane. Reuse existing ordering/identity mechanisms where suitable.

Cross-workspace movement changes routing membership even when pane/process identity stays stable. Audit zor observation, managed attempts, pending input, event cursors and other consumers. Either update routing through an explicit coherent transition or reject movement while a constraint cannot be preserved. Never leave a live task silently targeting an old route or retarget it to another pane. Document any restriction, test it and account for it in the parity checklist. Keep task policy in zor; fux primitives remain generic.

Define layout invariants: unique pane membership, valid split structure, deterministic focus, in-bounds non-overlapping pane rectangles except intentional overlays, and exact usable-area accounting including borders/gaps. Specify a deterministic collapse/hide policy when the terminal cannot fit every minimum-size pane. Zero-sized terminal geometry must not panic or request invalid PTY sizes.

Handle odd dimensions, rounding, nested splits, empty containers and removal of a focused/zoomed/dragged pane. Cancel gestures safely after disconnect, target deletion or layout generation change. Render feedback without introducing excessive allocations or redraw work in normal terminal output paths.

## Layout import/export contract

Use a documented format owned by fux. Validate before mutation: duplicate/unknown pane IDs, ownership outside the target scope, omitted panes, invalid orientations, non-finite/out-of-range ratios, excessive depth/node counts and oversized input. Define how exported pane identities are mapped when applying a layout to a different set of existing panes.

Apply a valid layout atomically. Invalid input or a stale target must leave layout, focus, pane membership and processes unchanged. Resource bounds must prevent recursive stack exhaustion and pathological allocations. Do not assume hypertile's deserializer provides these protections. Exporting and reapplying an unchanged layout should not cause spurious process resizes or identity changes.

## Verification and acceptance

Run the required repository formatting/linting and relevant suites. Add tests that prove behavior and failure boundaries rather than mirroring helper implementations:

- Geometry and mutation invariant/property tests over sequences of splits, moves, swaps, resizes, zooms, closes and container changes.
- Directional focus/resize tie-breaking and small/zero/odd geometry cases.
- Layout round trips, explicit pane remapping and rejection of malformed, oversized, excessively nested, duplicate, missing and foreign-pane input with no partial effects.
- Real PTY integration tests proving PID/PTY identity, terminal content and ongoing output survive layout mutations; exercise running zor tasks when routes change.
- Competing viewers, stale generations, target exit and disconnect during drag/apply. Verify coherent capture/events and correct resynchronization.
- Keyboard and mouse interaction, cancellation, application mouse forwarding, disabled commands and terminal restoration after exit/error.
- Clean-checkout build/package verification with reference directories absent. Confirm manifests, lockfiles, build scripts and tests have no hypertile dependency or reference-path reliance.

Perform that independence check in a temporary clean source copy containing the intended changes and excluding all reference checkouts; do not delete or move the user's reference directories. Run the required build and relevant tests there, and record the exact commands and results. Attribution and documentation may mention upstream source paths, but executable code and tooling must not require them.

Manually exercise the resulting UI in a real terminal: nested panes, keyboard resize/move, mouse border/pane drag, zoom/unzoom, tab/workspace moves, export/apply and simultaneous viewers. Inspect visual output and record what was actually exercised. Compare the pane/layout checklist against the pinned Herdr baseline. For each baseline capability, record the corresponding fux keyboard/mouse/CLI/API behavior, verification evidence and any remaining gap. The acceptance target is parity or better for every material pane/layout capability, not a count of implemented features; a restriction that prevents an equivalent workflow remains a gap until resolved. Measure representative layout/resize and steady output paths before/after; investigate material regressions rather than assuming a new layout engine is faster.

Review the complete diff in a separate pass for correctness, identity/routing regressions, invalid import handling, races, hot-path costs, missing tests and attribution. Use an independent reviewer if available; otherwise perform an explicit separate full-diff review. Fix confirmed in-scope findings and rerun affected checks. Apply the user's full PR completion gate if PR work is authorized.

Deliver the implementation, updated command/help/protocol/layout-format documentation, changelog and required third-party attribution. Report supported controls, layout parity gaps if any, exact verification results and known limitations. Do not declare completion while required behavior is missing or relevant checks are failing. A missing environment or external permission is a specific blocker to report, not a passing result. The final product must have no build, test or runtime dependency on hypertile or its reference checkout.

Keep the user-facing control guide in `docs/pane-layout-controls.md` and the workflow checklist, pinned baseline, verification evidence and unresolved acceptance items in `docs/pane-layout-implementation.md`. Update existing documents rather than replacing accumulated evidence. Distinguish implemented behavior, automated verification and manual verification explicitly. The final handoff must state whether pane/layout parity is proven, incomplete or blocked, with concrete evidence for that status.
