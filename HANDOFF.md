# fux 0.3.2 handoff

Current work (2026-09-08) is **complete within the required non-R6 scope** under
[headless-native-agent-milestone-prompt.md](headless-native-agent-milestone-prompt.md).
The new acceptance checklist and execution record are in
[docs/native-agent-milestone.md](docs/native-agent-milestone.md). Bounded verification
execution and durable validated continuation are implemented, regression-tested and
independently reviewed. Codex native protocol/stdio core and atomic native journal
operations are implemented and reviewed. Nonblocking idle polling and native thread
start/resume validation are also implemented and independently reviewed. The first
managed Codex worker path now passes a real-fux fixture regression for literal
submission, response correlation, contention, prepared-input notifications, completion
followed by EOF, and child cleanup. Durable history-read and interrupt controls now
also pass real-fux CLI/service fixture coverage, including lost acknowledgement,
duplicate control requests, cancellation remaining distinct from interruption, and
near-capacity journal expiry/retirement. These fixes are independently reviewed.
Native storage identity is now retained and tested, including allocated paths whose
dated directories do not yet exist. The installed metadata probe succeeded; an
exploratory empty-thread resume was rejected because no rollout had been written.
The installed evidence remains separately attributed; no live model turn is claimed.
Recreation orchestration now passes state regressions and real-fux fixture coverage:
the owned wrapper stops its provider before resuming retained native storage, keeps
the fux session, and reconciles history without input replay. Independent review
found and accepted a fix to enforce the original deadline again at final publication.
Codex capability reporting now describes the implemented native paths and their limits.
Retained native evidence has tested, read-only age invalidation; duplicate events do
not renew freshness, and availability remains separately unprobed.
The existing attention view now consumes bounded native summaries joined to current
pane observations; expiry and observer invalidation produce unknown state. Dashboard
unit and real-fux integration tests pass, and independent review accepted the change.
The N2 failure coverage map is complete; the current native suite passes 38 tests
with two optional installed probes ignored. N3 baseline binaries/source/toolchain and
the three-repetition viewer capture are retained under `.verification/native-performance-20260908/baseline/`.
N3 measured an ASCII validation optimization and rejected it after mixed paired results,
including a slow-consumer sustained regression. Baseline/candidate artifacts and the
full decision are retained in `docs/native-performance-decision.md`; independent review
verified the measurements. Production validation is restored. The extended native
two-worker workflow now passes and retains its verified artifact handoff under
`.verification/native-workflow-20260908/`; independent review accepted its final fixes.
N5 companion refresh and complete intended-diff/final-fix reviews passed. The fresh
final gate `.verification/gate-tbzpPi/manifest.json` records all 45 mandatory commands
passed, a finalized checkpoint and `complete: true`; its invocation exited 0.
Exact results, review dispositions and retained failed attempts are documented in
[docs/native-final-verification.md](docs/native-final-verification.md).
No unresolved non-R6 blocker remains. No live model-turn or
materialized real-provider recreation validation is claimed.

The previous milestone is **complete within the non-R6 scope**: it includes migration
of all first-party Python to Rust. The authoritative final verification is
[docs/headless-final-verification.md](docs/headless-final-verification.md), with context in
[docs/headless-milestone.md](docs/headless-milestone.md) and
[docs/python-rust-migration.md](docs/python-rust-migration.md). All 74 inventoried
Python files and both embedded execution sites are replaced and reviewed. Complete intended-diff
review, companion reconstruction and every mandatory non-R6 check passed. The verification
record identifies actual gate failures, confirmed test fixes and targeted continuations;
unchanged successful evidence was reused. No unresolved non-R6 blocker remains.
R6 remote runtime work remains explicitly deferred. Earlier completion records below describe
their historical milestones. No repository commits, pushes or PRs were made.

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
  `cargo run --manifest-path tools/xtask/Cargo.toml --locked -- dependencies verify --build`.
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
