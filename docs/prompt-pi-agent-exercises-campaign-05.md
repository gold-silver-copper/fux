# Campaign 05: measure what remains, and make the harness able to measure it

## Objective

PR #42 merged the pi-driven exercise harness, eight findings, fixes for F1, F3, F5 and F8, and campaign 04 measuring those fixes. Two findings remain open by design (`fux-agent-exercises/FINDINGS.md`, "Ranked next steps"), and campaign 04 surfaced one observation the harness cannot yet measure. This work does three things, in this order:

1. Improve the harness so the remaining questions can be answered from artifacts rather than by hand.
2. Run campaign 05 to answer them.
3. Record the answers in `FINDINGS.md`, with numbers only where the sample supports them.

Do not modify fux. If a run reveals a fux defect, record it as a new finding with independent evidence, as F1 was; the fix is a separate change.

## Read first

- `fux-agent-exercises/FINDINGS.md` in full, especially F2, F4 and the campaign 04 paragraph under F8.
- `fux-agent-exercises/src/report.ts`, `src/runner.ts`, `src/scenarios/noisy.ts`, `src/scenarios/recovery.ts` and `tests/positive-control.test.ts`.
- `docs/prompt-pi-agent-exercises.md` and `docs/prompt-pi-agent-exercises-fixes.md` for the rules the harness already lives under. They still apply: one thin tool, no translation or discovery on the agent's behalf, no paid calls in `cargo test`, no npm dependencies.

## The three open questions

**Q1, from F2.** Under prompt revision 2, four `noisy` runs have answered correctly. That is not a rate. What is the fabrication rate with a sample that can support one?

**Q2, from F4.** With the `scroll` command documented, `noisy` still cost four to five repaints and two to three direct `Viewer.scrollback` writes per run to find one line. Is that the floor, and how does it scale with how far back the line sits? This is the number that decides whether a bounded plain-text history read is worth building.

**Q3, from F8.** The recovery disruption now lands on the agent's own `close`, and the agent's request meets a stale target. Neither campaign 04 agent noticed: a close of a vanished pane answers `null`, the "target no longer exists" text goes to `Viewer.notice`, and both agents sent `focus` next, which clears it. The current verifier only checks whether that text appeared in a response, which conflates "the notice was set" with "the agent looked". How often does an agent verify a command's effect before acting again?

## Harness improvements

Make each change small, covered by a test that contacts no model, and land it before the campaign so campaign 05 artifacts carry the new fields.

### Per-finding metrics in `report.ts`

Campaign 04's per-finding numbers were computed by an ad hoc script outside the repository. That must not happen again. `node run.ts report` gains a "Findings" section computed from the artifacts, one row per run where the metric applies:

- `noisy`: scroll attempts, scroll rejections, whether the first attempt was accepted, direct `Viewer.scrollback` writes (`world.insert_components` or `world.mutate_components` touching that field), `fux.frame` calls, the final offset, and the fixture's `linesBackFromBottom`.
- All scenarios: `world.list_components` calls, wrong hierarchy path guesses (a `bevy_hierarchy::` path in any request, or an `Unknown component type` response naming `ChildOf`), and the index of the first request using a `bevy_ecs::hierarchy::` path.
- `recovery`: the disruption's call index and method, and the new verification metric below.
- All scenarios: any `world.insert_components` or `world.spawn_entity` for a reflected fux component with a required field missing, and the server's answer.

Put the computation in one module the report and the tests share, and test it against a fixture `run.json` checked in under `tests/fixtures/` with credentials and the system prompt stripped. The existing campaign 03 and 04 artifacts are the source for that fixture.

### A "verified before acting again" check for `recovery`

Add to the recovery verifier an evidence field and a note stating whether, after the tool call that met the stale target, the agent's next request read state (`fux.frame` for its own viewer, `world.get_components` or `world.query` including `fux::model::Viewer`, `fux::model::ProcessState` or `fux::model::PaneView`) before its next `Control`. Record which request it was. Keep it a note and an evidence field, not a check that changes the outcome: the scenario's task is to close and focus, and an agent that succeeds without looking has still succeeded. The positive control must exercise both branches: the existing scripted agent reads its viewer after the close and must be recorded as verifying; a second scripted variant sends `focus` immediately and must be recorded as not verifying.

### A `noisy` variant that varies depth

`noisy` currently has two variants with the diagnostic 131 and 139 lines back. Q2 needs the cost as a function of depth. Add variants at roughly 40, 130 and 400 lines back, each with its own code, keeping the fixture's output shape and the task wording of prompt revision 2 unchanged. Record the depth in `baseline` as today. The verifier must not change. The positive control must pass on every variant, driven to completion through the real tool as now.

### Keep the experiment comparable

- Do not change any task prompt text. `noisy` stays on revision 2; if any wording changes, bump the revision and record it in `baseline.promptRevision` as F7 established.
- Do not change budgets, thinking level, tool semantics or the model resolution rules.
- Do not change how `sawStaleTargetNotice` is computed; add beside it.

## Campaign 05

Two parts, on the same pinned model resolved by preflight, both against the merged `main`:

```sh
node run.ts preflight
node --test --test-concurrency=1 "tests/*.test.ts"
node run.ts campaign --scenarios noisy --repetitions 10 --artifacts runs/campaign-05-noisy
node run.ts campaign --scenarios recovery --repetitions 5 --artifacts runs/campaign-05-recovery
node run.ts report --artifacts runs/campaign-05-noisy
node run.ts report --artifacts runs/campaign-05-recovery
```

`--repetitions 10` over three `noisy` variants is 30 runs; `--repetitions 5` over two `recovery` variants is 10. Expect about two dollars in total at campaign 04's cost per run. If preflight resolves a different model than campaigns 01 to 04 used, stop and report; do not compare across models.

If any run's server dies, treat it as a new F1-class finding: reproduce it without the agent before writing it up, exactly as `evidence/partial-component-panic.sh` did.

## What to report

Add a "Campaign 05" section to `FINDINGS.md` following the campaign 04 pattern: what ran, on which fux revision and README hash, cost, then one subsection per question.

- **Q1:** fabricated answers out of 30, with the fabricated line quoted and shown absent from every frame the run received, as F2 did. If the count is zero, say the upper bound the sample supports and no more.
- **Q2:** per depth, median and range of repaints, direct offset writes and total calls, and outcome. State plainly whether cost grows with depth. Do not propose an API; record the demand with numbers, as F4 does.
- **Q3:** how many of ten agents verified before acting again, and what they read. If most do not, that is a statement about the notice channel worth carrying into a later fux discussion; say so without redesigning it here.

Update "Ranked next steps" to what is still open after these answers. If F4 now has the numbers to justify a bounded history read, say that it does and what the numbers are; the design stays a separate change.

## Verification

- `node --test --test-concurrency=1 "tests/*.test.ts"` with the new report module test, both recovery verification branches and every `noisy` variant's positive control; no model calls.
- `node run.ts dry-run` on the new variants.
- `cargo test --locked` unchanged, since fux is not modified. If it is red for any reason, that is a finding, not something to fix here.
- The report's "Findings" section for campaign 04's artifacts must reproduce the campaign 04 numbers already in `FINDINGS.md` before campaign 05 is run. That is the check that the new metrics are computed correctly.

## Deliverable

One PR: harness commits first, each with its test, then the `FINDINGS.md` commit with campaign 05. Artifacts stay gitignored; the stripped fixture `run.json` and the findings are the durable record.
