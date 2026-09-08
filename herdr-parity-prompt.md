# Close the agent gap with herdr: curated detection, agent-aware waits, and worktrees

A source audit of `references/herdr` (0.8.2), `references/zellij` (0.46.0) and `references/tmux`
(next-3.8) against fux + zor + koh found that fux has the stronger protocol, remote and safety
story but is behind herdr on the agent runtime itself. herdr detects 22 unmodified agents from
their live terminal with curated, versioned manifests, shows each pane's state inline, blocks on
agent status ("wait until that agent is blocked"), and gives each agent a git worktree. fux only
learns an agent's state when the program self-reports OSC 7877 or a thin zor rule matches, does
not show that state in its own UI, waits only on generic pane conditions, and has no worktrees.

Close that visible gap. Implement and verify; do not stop at a plan. The deliverable is a pull
request against `main`, not a push to it. Keep the project boundary (zor detects, fux displays and
orchestrates, koh is untouched) and fux's posture (`unsafe` forbidden, bounded parsers, MSRV 1.95).

## Objective

Target these areas, in order. Each is scoped so a conforming agent — a lead agent driving fux over
the control socket — can do what a herdr user's lead agent can do.

1. **Curated detection of unmodified agents (zor).** Turn zor's detection into a manifest-driven,
   engine-versioned, updatable set covering the agents herdr ships for (at least claude, codex,
   cursor, gemini, github-copilot, opencode, qwen, grok, devin, plus fux's own), matching each
   agent's real TUI in named screen regions (window title, the text after the last prompt marker,
   the top lines) and mapping to `working` / `blocked` / `idle` / none, emitting the OSC 7877 v1
   report zor already defines. fux launches agent panes through zor by default so detection needs
   no cooperation from the agent.
2. **Agent state in fux's own UI.** OSC 7877 is already parsed into `list` and `pane.agent`
   (0.6.0); now carry it in the attachment frame (the item 0.6.0 deferred) and render it in the
   bar and the command column with a small, legible legend, so a human watching fux sees at a
   glance which pane is working, blocked or idle — herdr's sidebar, in fux's bar.
3. **Agent-status-aware `wait`.** Extend the control `wait` with conditions keyed on the pane's
   agent state (`agent-blocked`, `agent-idle`, `agent-working`) and a prompt-stall variant (no
   observed working-or-blocked state within N ms of input), fed by the state fux now holds. A lead
   agent can then block until a helper is genuinely blocked or done, not just until output goes
   quiet.
4. **Git worktrees.** Add a `worktree` control-protocol family (`create`, `list`, `open`, `remove`)
   and CLI, and a worktree option on `new`/`split`/`run`, so a lead agent can fan out isolated
   worktrees for helper agents. Bounded and safe: only under a repository the caller names, never
   the main worktree without an explicit flag, branch and path validated, and removal refuses a
   dirty or checked-out tree unless forced.
5. **A parity proof.** A headless scenario (no pty) that reproduces herdr's self-orchestration
   demo over fux's protocol: a lead creates two worktrees, runs an agent (or a scripted stand-in)
   in each, waits until both are blocked or exited, reads their screens, writes a summary, and
   cleans up the worktrees and panes. It joins `tests/local_cli.rs`.

Non-goals, stated so the scope stays honest: a plugin system or marketplace, live binary handoff
(`server.live_handoff`), a sidebar TUI redesign, and matching herdr's exact manifest count on day
one. Name them as later work, not omissions.

External contracts unchanged in spirit: the local protocols carry no version numbers (a schema
changes freely as long as every consumer — CLI, viewer, koh, zor, the harnesses, the fixtures —
changes with it), the ordering guarantees in docs/design.md, the performance table in
docs/ecs-acceptance.md (no measure may regress beyond noise), MSRV 1.95 and the gate.

## Starting point and authorization

- Baseline: `main` at the 0.6.0 merge (re-fetch and record the SHA you branch from). Read
  README.md, HANDOFF.md, docs/design.md, docs/security.md, docs/ecs-acceptance.md, both protocol
  docs, `src/terminal.rs` (OSC 7877 parsing, `AgentReport`), `src/view.rs` (frame types, the
  deferred agent-in-frame note), `src/client/render.rs` and the bar/command-column code,
  `src/ecs/systems/requests.rs` (`resolve_waits`, `WaitUntil`), `src/proto/control.rs`,
  zor's `OBSERVATION-CONTRACT.md`, `src/observe.rs`, `src/rules/` and `src/state.rs`, and
  `references/herdr/src/detect/` (its manifest format and rules) and `references/herdr/src/api/`
  (its agent verbs and `wait`) for the bar it sets — study, do not copy.
- Work on a branch (`agent/herdr-parity` or similar) in an isolated worktree. Never push to
  `main`. This prompt authorizes local implementation, verification, documentation, independent
  reviewer subagents, commits, pushing the branch and opening one pull request (draft until the
  completion gate passes, then ready). It does not authorize merging, force-pushing `main`,
  touching personal sessions or runtime directories (`~/Library/Caches/fux-runtime`,
  `$XDG_RUNTIME_DIR/fux`), or killing any server the task did not start. Every real-process run
  uses disposable HOME/XDG directories.
- zor and koh are pinned repositories. The detection expansion is a real zor change: carry it in
  `dependency-patches/` so `python3 tools/dependencies.py verify --build` reconstructs it, and
  list it in the PR as the companion zor PR to open. koh needs nothing; if it does, that is a
  finding. Machine-local `zor` and `references` symlinks in worktrees are ignored; never commit
  them. `references/herdr`, `references/tmux`, `references/zellij` are read-only comparison sources
  and are never committed or built by the gate.

## 1. Curated detection (zor)

In zor, replace or extend the hand-written rules with a manifest set in the style of herdr's
`src/detect/manifests/*.toml`: per agent an id, an engine version, and ordered rules with a
priority, a screen region (window title, the region after the last prompt marker, the top N
non-empty lines), a matcher (`contains`, `regex`, `all`, `any`) and a resulting state with the
visible-evidence flags zor's schema already carries. Ship manifests for the agents named in the
objective, each with a working spinner rule, a blocked "needs input / trust this directory /
action required" rule, and an idle-at-prompt rule; keep a documented fallback for a known agent
whose screen matches nothing. Manifests are data, loaded at startup and reloadable, versioned so
they can be updated without a zor release; validate them (bounded count and size, valid regex
compiled once) exactly as zor already bounds its inputs. Every manifest gets a fixture screen
(`zor check`) proving the state it detects, mirroring herdr's per-agent test assets. The detection
still emits OSC 7877 v1 unchanged, so fux needs no zor library dependency.

fux side: make an agent pane observed by default. Add a config and a `new`/`split`/`run` option
that wraps the pane command as `zor --title never -- COMMAND` (or the configured observer), so an
unmodified agent is detected without emitting anything itself; document that a pane can still
self-report OSC 7877 and be seen with no observer. Nothing in fux spawns, supervises or trusts
zor beyond running it as the pane command and reading the OSC.

## 2. Agent state in fux's UI

Carry `agent` in the attachment `PaneUpdate` (the 0.6.0 deferral): it changes rarely, so treat it
like the title — part of the pane's metadata, sent when it changes, advancing the pane's output
sequence so the delta path already carries it; koh's gateway tests read `cells[].text` and must
keep passing. `PaneView` keeps it; `apply`/`merge`/`within_bounds` handle it; the wire stays
bounded (the id and message limits from 0.6.0). Render it in the bar for the focused pane and in
the command column per pane with a compact glyph or word and a color from `[style]` (a legend in
the README), with the same restraint as the progress indicator, and never let a hostile agent id
or message break layout (it is already bounded and control-character-free). A viewer test
(`tests/verify/viewer.py` or a new harness) proves the bar shows the state and that a flapping or
hostile report cannot corrupt the screen or the terminal.

## 3. Agent-status-aware wait

Add to `WaitUntil`: `AgentBlocked`, `AgentIdle`, `AgentWorking`, and `PromptStalled { ms }` (no
observed working-or-blocked state within `ms` after the pane's last input). Evaluate them in
`resolve_waits` from the pane's current `AgentReport`, with the same bounds, cancellation and
timeout as the existing conditions, and the same reply shape (`fired`, the pane's seq and exit
status, plus the agent state that fired). A pane that never reports an agent state fails such a
wait only at its timeout, never spuriously. Deterministic `tests/ecs.rs` cases drive OSC 7877
through a pane and assert each condition fires on the right transition and that `PromptStalled`
fires only when input produced no state; the CLI grows `fux wait PANE agent-blocked` etc.

## 4. Git worktrees

Add a `worktree` control family: `create {repo, branch?, base?, path?, label?}` returns the
worktree path; `list {repo?}`; `open {path}` (attach a pane there); `remove {path, force?}`.
Fulfil `create`/`remove` through a single reviewed helper (the OS/adapter layer, joined like a
pane task — the structure invariant stays true), running `git worktree add`/`remove` with the repo
root validated (a real git repository the caller named), `O_NOFOLLOW` on the directory, the branch
and path validated like workspace names, and a bound on worktrees per server. `remove` refuses a
dirty or still-checked-out tree unless `force`, and never removes the repository's main worktree.
`new`/`split`/`run` gain a `worktree { repo, branch?, base? }` option that creates the worktree and
spawns the pane with its `cwd`. Every bound is in docs/security.md with a rejection test; a
`tests/verify/` harness creates a throwaway git repo in a disposable dir, drives the worktree
family end to end, and asserts nothing outside the disposable dir is touched.

## 5. Parity proof

`tests/verify/agent_orchestration.py`, joining `tests/local_cli.rs`: with no pty, a lead creates
two worktrees in a throwaway repo, runs a scripted agent stand-in in each (a shell script that
prints a prompt, emits OSC 7877 `working` then `blocked`, waits for input, then exits), waits until
both panes are `agent-blocked`, reads their rows, sends each an answer with `send-keys`, waits for
both to exit, reads the final screens, writes a one-line summary, removes both worktrees and
confirms the repo and the runtime directory are clean. This is herdr's self-orchestration demo,
proven over fux's documented protocol with no agent-specific code in fux.

## 6. Preserve the contract, prove it

Every existing test passes or is migrated with its assertion intact and an assertion-ledger entry:
`tests/ecs.rs`, `tests/structure.rs`, `tests/local_cli.rs`, `tests/fixtures.rs`,
`tests/zor_integration.rs`, the fixture-child suite, the koh gateway suites. Add fixtures under
`tests/verify/fixtures/` for the new requests, events and the agent-carrying frame, round-tripped
by `tests/fixtures.rs`. Rerun `tools/measure.py`, `measure_frames.py`, `measure_viewer.py` and
`measure_memory.py` on both binaries; no measure in the 0.6.0 table may regress beyond noise (the
agent field sits on the frame path — profile if the keystroke bytes or burst CPU move). Run the
full gate on the final tree and record it in docs/ecs-acceptance.md:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
ZOR_BIN=$PWD/zor/target/debug/zor FUX_REQUIRE_ZOR_BIN=1 PROPTEST_CASES=2048 cargo test --locked -- --test-threads=1
cargo doc --no-deps --locked
cargo +1.95.0 check --all-targets --locked
cargo test --locked --manifest-path tests/verify/fixture-child/Cargo.toml
FUX_BIN=$PWD/target/debug/fux KOH_REQUIRE_FUX_BIN=1 cargo test --manifest-path references/koh/Cargo.toml --test gateway --locked
FUX_BIN=$PWD/target/debug/fux KOH_REQUIRE_FUX_BIN=1 cargo test --manifest-path references/koh/Cargo.toml --lib gateway:: --locked
tests/verify/release-package.sh --allow-dirty
python3 tools/dependencies.py verify
git diff --check
```

The lints stay; `unsafe` stays forbidden. No new fux dependency without a measurement or a finding;
git is invoked, not linked. Linux runs through the branch CI matrix as before (record the run
link).

## 7. Work in bounded slices

Suggested order: zor manifests and `zor check` fixtures → observed-by-default pane wrapping →
agent state in the frame and the bar → agent-status waits → worktrees → the orchestration proof →
docs. Each slice commits with its findings and their fixes named; a behavior is not done until its
fixture, its doc row and its bound test exist.

## 8. Independent review and acceptance

Fresh reviewer subagents that did not implement the reviewed slice, once after the agent-status
wait slice and once on the complete branch diff against the merge base. They review whether
detection can be spoofed or wedged by a hostile pane, whether the agent field can regress the
frame path or break koh, whether a worktree operation can escape its repository or touch the main
worktree, whether the waits can hang or fire wrong, whether the manifests are actually loaded and
bounded, and every claim in the documents. Fix confirmed P0/P1 and in-scope lower findings,
document rejected findings with reasons, rerun affected checks, and obtain a final review.

## Deliverables and completion

- The branch with slice-sized commits, pushed to origin as authorized.
- One pull request against `main` whose description contains: the baseline SHA, a short before/after
  capability comparison against herdr for the four areas, every new request/event/manifest with its
  bounds and tests, the worktree safety argument, the performance re-measurement table, both gate
  summaries (macOS and the Linux CI run link), the assertion ledger, the fixtures list, and the
  review findings with dispositions, plus the companion zor PR to open. Draft until both gates and
  the final review are clean.
- Updated README.md (observed-by-default agents, the bar legend, `fux wait … agent-blocked`, the
  worktree commands), docs/design.md (agent state in the frame, detection ownership), docs/security.md
  (every new bound: manifests, worktrees, agent-status waits), both protocol docs (the new families
  and the agent frame field), docs/ecs-acceptance.md (a "herdr parity" section with the evidence),
  CHANGELOG (0.7.0), a short HANDOFF.md, and `dependency-patches/` with the zor manifest change.

Do not declare completion while any new bound is unenforced by code and a test, any consumer reads
a shape that changed without its test changing, a worktree operation can touch anything outside the
repository it names, the zor suites fail against the pinned base plus patch, or the PR is a draft
with known findings. Do not merge, do not push to `main`.
