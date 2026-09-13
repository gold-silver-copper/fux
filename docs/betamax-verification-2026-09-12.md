# Betamax verification — 2026-09-12

Betamax is integrated into the headless harness. The exercised fux viewer screens
passed visual review after fixing a real redraw defect. The complete headless
test run is **not green**: four existing zor protocol scenarios fail with capture
disabled as well. No tests were excluded.

## Changes

- Added the optional `betamax-core` 0.1.11 harness dependency, enabled automatically
  by `FUX_BETAMAX_DIR` in the integration-test launcher. It is not a production dependency.
- Feed real PTY output into Ghostty alongside the existing vt100 assertions.
  Save raw output, resize boundaries and labeled terminal-state snapshots.
- Replay the exact checkpoint byte offsets after the interactions, compare every
  replayed state against its live snapshot, then render PNGs and an HTML index.
  Synchronous rasterization initially perturbed timing-sensitive interactions;
  deferred rasterization restored the passing viewer scenario without removing
  its assertions or increasing its deadlines.
- Wait for synchronized-output end markers before saving checkpoints, and reject
  unfinished frames during replay. Visual review caught a partial pane redraw
  that had been captured midway through a PTY read; a chunked-frame regression
  now covers this case. The same safeguard exposed the cold-start probe returning
  at the beginning of its first redraw; it now waits for the end marker.
- Applied documented local Betamax fixes for live resizing, wide-character
  rendering/text extraction, and connected box-drawing strokes. Preserved upstream
  licenses and provenance in [the vendored patch record](../tools/xtask/vendor/betamax-core/FUX-PATCHES.md).
- Fixed fux's full redraw: reset SGR attributes before clearing the display.
  Previously, terminal background-color erase inherited the status bar color,
  while the diff assumed a default-colored blank screen. This left gray patches
  after tiny-size recovery and overlay changes. Added both a terminal-state unit
  regression and a real-viewer background assertion.
- Added a dedicated two-workspace-transfer input regression and a Linux CI job
  that retains rendering evidence, including when tests fail.

## Verification results

All commands ran locally on macOS/arm64. Exact binaries, source hashes, toolchain,
font provenance and counts are in [summary.json](verification/betamax-2026-09-12/summary.json).
Build/run commands and native prerequisites are in [the harness guide](betamax-harness.md).

| Check | Result |
| --- | --- |
| Standalone harness tests with `--features betamax` | 66 passed, 0 failed |
| `cargo +stable test -p fux --locked --no-fail-fast --test local_cli --test automation_integration -- --test-threads=1 --nocapture` | Integration: 19 passed, 4 failed. Local: 13 passed, 1 capture failure; corrected local rerun: 14 passed. No skips. |
| Targeted fux render unit tests | 7 passed, including the inherited-background regression |
| fux all-target Clippy and Betamax-enabled harness all-target Clippy | Passed with `-D warnings` |
| Workspace/harness formatting and `git diff --check` | Passed |
| Harness all-target check on Rust 1.95, without the optional renderer | Passed |
| Full viewer scenario and offline replay | Passed; all 241 final checkpoints reproduce their live states exactly |

Logs: [headless suites](verification/betamax-2026-09-12/all-headless-tests.log),
[harness tests](verification/betamax-2026-09-12/harness-tests.log),
[render regression tests](verification/betamax-2026-09-12/fux-render-tests.log),
[viewer replay](verification/betamax-2026-09-12/viewer-replay.log).
The complete run after adding synchronized-frame validation passed 19 integration
scenarios and failed the four fixtures below. Its local suite passed 13 and caught
the incomplete cold-start capture. After fixing that probe, the entire local
suite was rerun (see [local rerun](verification/betamax-2026-09-12/local-rerun.log)).
The new GitHub workflow has not been run remotely; no CI success is claimed.

## Visual review

The final gallery contains **241 complete-frame PNG checkpoints** from the zor
integration run and corrected local-suite rerun. Replay exactly matched every
live state and rejected unfinished synchronized frames. Of these images, 228 are
byte-identical to images already visually reviewed; all 13 new images were
reviewed on a contact sheet. The formerly partial pane redraw was also checked
at full resolution and now has continuous separators across the full viewport.

The gallery is `target/betamax-verified-complete-frames/index.html`. The retained
[checkpoint manifest](verification/betamax-2026-09-12/reviewed-checkpoints.json)
records labels, dimensions, image hashes and prior-review matches;
[replay log](verification/betamax-2026-09-12/complete-replay.log),
[new-image contact sheet](verification/betamax-2026-09-12/final-new-images.png), and
[completed pane redraw](verification/betamax-2026-09-12/completed-pane-redraw.png)
provide additional evidence. The earlier review covered all 204 viewer PNGs on
contact sheets, with tiny-size recovery, Unicode labels, pane-drag preview and
close confirmation inspected at full resolution.

The reviewed screens cover nested panes; hidden-pane recovery; focus and zoom;
resize, swap and drag feedback; pane/tab/workspace menus and ordering; overflow
tab destinations; simultaneous viewers; copy overlays; live workspace transfers;
and mouse close/cancel workflows. Backgrounds, borders, labels and overlays are
clean in the regenerated output. Blank content in 1×1 terminal states and after
detachment is expected, not a missing render.

Selected retained evidence:

- [Before the redraw fix](verification/betamax-2026-09-12/resize-before.png)
  and [after the fix](verification/betamax-2026-09-12/resize-after.png).
- [Unicode tab label](verification/betamax-2026-09-12/unicode-label.png).
- [Pane-drag destination and hint](verification/betamax-2026-09-12/pane-drag.png).
- [Workspace close confirmation](verification/betamax-2026-09-12/workspace-close.png).

Rendering uses the explicitly selected Noto Sans Mono CJK SC font. It verifies
Betamax's software rendering of actual fux output, not a desktop terminal's GPU
renderer or OS handling of mouse/modifier input. Native clipboard contents and
subjective interaction feel remain outside this verification.

## Remaining failures

These protocol fixtures were not changed as part of the rendering integration:

| Scenario | Observed failure and relevant fixture behavior |
| --- | --- |
| `zor_groups`, `zor_group_scheduler` | Alias-runtime adoption fails with a missing socket. The alias fixture exposes `default.sock`, while current pane discovery also needs `manager.sock`. |
| `zor_service` | Slow-proxy task adoption fails with a missing socket. Its proxy fixture exposes the workspace control endpoint, not the manager endpoint now used for pane discovery. |
| `zor_recovery` | The test expects reconciliation to fail after parking only the workspace control socket; reconciliation succeeds through the still-available manager route. |

Capture-disabled reproductions are retained for
[groups](verification/betamax-2026-09-12/capture-disabled-groups.log),
[service](verification/betamax-2026-09-12/capture-disabled-service.log), and
[recovery](verification/betamax-2026-09-12/capture-disabled-recovery.log).
The scheduler variant reaches the same group-fixture alias failure. Resolving
these requires updating the routing/proxy/outage fixtures and rerunning the zor
integration suite. Full headless-suite acceptance remains incomplete until then.
