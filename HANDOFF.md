# fux 0.3.2 handoff

Updated 2026-09-06 (bar at the bottom with its own background, attachment v5); the 0.3.0 rewrite
notes below still apply. The bevy_ecs rewrite requested by
[bevy-ecs-multiplexer-prompt.md](bevy-ecs-multiplexer-prompt.md) is implemented, verified locally
and independently reviewed. The requirement-by-requirement audit with exact commands and results
is [docs/ecs-acceptance.md](docs/ecs-acceptance.md); the architecture is
[docs/design.md](docs/design.md); the plan written first is [docs/ecs-plan.md](docs/ecs-plan.md).

## Bar and separators (0.3.1, 0.3.2)

[top-bar-design-prompt.md](top-bar-design-prompt.md) is implemented, with the bar moved to the
bottom row on its own background in 0.3.2 (attachment v5): an always-visible one-row bar
(workspace, tabs with the current one reversed, focused pane `id: title` or a two-second notice),
no pane frames, shared one-cell separators bold next to the focused pane, and a `[style]` table
with muted defaults. Geometry changed (bar row reserved, one-cell sibling gap, leaf rectangle is
the content area). Evidence and the independent
review are in the "Top bar" section of docs/ecs-acceptance.md. The attachment protocol is now v4 because the frame's rectangle contract changed (an independent
reviewer caught the missing bump by attaching a 0.3.1 viewer to a 0.3.0 server); koh's real-fux
tests follow through `dependency-patches/`. Gate on the final tree (macOS): fmt, strict Clippy, root tests (lib 70, main 3, ecs 19, local_cli 6 incl. the v4 attachment,
detach-drain and migration harnesses, structure 8, real zor 1), rustdoc, MSRV 1.95 check,
fixture-child 3 + 8 + 2, koh gateway 2 + 10 against the v4 binary, packaged binary 8, dependency
patches verified, `git diff --check`; all passed on 2026-09-06.

## What changed in 0.3.0

- `src/` and `tests/` are new trees. The old host, router, state store, control queue, popup
  panes, pickers, hooks, notifications, observation adapter and sidecar supervision are gone; the
  old files show as deleted in `git status`.
- Authoritative state is a `bevy_ecs` 0.19.1 World (workspaces, tabs, panes, viewers as entities)
  advanced by one ordered single-threaded schedule per event-driven step; adapters own PTYs,
  processes and sockets and exchange typed messages/effects with the World.
- Attachment protocol v3 and control protocol `FUXCTL2`. koh's real-fux tests and zor's observe
  adapter received one-line version edits, exported to `dependency-patches/` and verified with
  `python3 tools/dependencies.py verify --build`.
- Version 0.3.0, MSRV 1.95, CI updated (`ci.yml`, `nightly.yml`), docs rewritten, earlier
  documents labelled historical.
- After the first `main` merge: an interactive dialog when an older, incompatible session server
  owns the runtime directory (explain, list its recorded workspaces, stop it after a typed
  confirmation or show how to run alongside it; non-interactive runs only report). Covered by
  `tests/verify/migration.py`.

## Verified locally (macOS)

Formatting, strict Clippy, root tests (lib 67, main 3, ecs 19 incl. a randomized command-sequence
test run with 2048 and 8192 cases, local_cli 6, structure 8, zor_integration 1 with real zor), rustdoc,
MSRV 1.95 compilation, fixture-child (3 unit, 8 binary, 2 lifecycle), packaged-binary verifier,
required real koh (2 + 10) integrations with explicit binary paths, zor's own suites, dependency
reconstruction, and the performance measurements against the 0.2.1 baseline (idle, memory, burst
and latency budgets hold in every run; startup holds at the median of 40 ms but two of five runs
exceeded the 50 ms budget). Exact commands and numbers are in the audit.

## Review

Two independent passes by agents that implemented none of the code. The first found no P0,
four P1 (exit racing spawn completion stuck a pane `Live`; a released `Starting` reservation leaked
its process; `view` reads were not scoped to the attachment's workspace; the viewer's Escape
deadline was reset by every frame) and ten P2 (outbox leak on disconnect, no SIGHUP grace on
release, reap-gate TOCTOU, signal starvation under a hot pane, pending-workspace kill race, viewer
limit bypass on switch, undocumented control idle timeout, double `exited`, doc mismatches, test
gaps). All were fixed with regression tests. The second pass verified every fix and found one new
P1 in the changed viewer code (a resolved lone Escape was re-fed through the filter, never
reaching the pane and spinning the viewer); fixed and covered by a real-viewer check. The
randomized ECS test independently found three invariant defects (an orphaned pane after closing the
only tab; a tab attached to a not-yet-open workspace; a starting reservation orphaned when its tab
closed under it), all fixed. Accepted P3 residuals are listed
in the audit.

## State of the worktree and remaining limits

- Everything is unstaged or untracked in fux for your review; koh (`references/koh`) and zor
  (`zor/`) carry only the patch-exported edits at their pinned bases. Nothing was committed, pushed,
  tagged, released or commented on GitHub, and no hosted workflow was rerun.
- No personal session was touched, no key cleared, no user workspace killed; all tests used
  disposable HOME/XDG directories. A running 0.2.x server, if any, is incompatible with 0.3.0
  viewers: save work and stop it with its own binary before using the new one.
- Runtime evidence is macOS only; Linux and Android are configured CI targets without an executed
  run of this tree. Emulator-specific clipboard/mouse behaviour and koh relay/NAT scenarios remain
  manual.
