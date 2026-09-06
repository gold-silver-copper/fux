# fux 0.3.0 rewrite handoff

Updated 2026-09-05. The bevy_ecs rewrite requested by
[bevy-ecs-multiplexer-prompt.md](bevy-ecs-multiplexer-prompt.md) is implemented, verified locally
and independently reviewed. The requirement-by-requirement audit with exact commands and results
is [docs/ecs-acceptance.md](docs/ecs-acceptance.md); the architecture is
[docs/design.md](docs/design.md); the plan written first is [docs/ecs-plan.md](docs/ecs-plan.md).

## What changed

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

## Verified locally (macOS)

Formatting, strict Clippy, root tests (lib 66, main 2, ecs 19 incl. a randomized command-sequence
test run with 2048 cases, local_cli 5, structure 8, zor_integration 1 with real zor), rustdoc,
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
