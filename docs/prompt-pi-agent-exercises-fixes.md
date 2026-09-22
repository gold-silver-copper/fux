# Fix what the agent exercises found, on the same branch

## Objective

PR #42 (`feat/agent-exercises`) added the pi-driven exercise harness and recorded eight findings in `fux-agent-exercises/FINDINGS.md`. It deliberately changed no fux source. This work lands the fixes for those findings as further commits on the same branch, so the PR merges with the problems it found already closed.

Fix the causes, not the symptoms. Every fix must be covered by a test that fails on the current branch and passes afterwards, and `FINDINGS.md` must end up describing what was done, not only what was observed.

## Read first

- `fux-agent-exercises/FINDINGS.md` in full, and `fux-agent-exercises/evidence/partial-component-panic.sh`.
- `src/model.rs`, `src/interaction.rs`, the `execute` match in `src/server.rs`, and the remote-control section of `README.md`.
- The vendored `bevy_ecs` `reflect/mod.rs` (`from_reflect_with_fallback`), `bevy_remote` `builtin_methods.rs` (`insert_reflected_components`) and `bevy_reflect` `serde/de/deserializer.rs`. Understand from the source, not from memory, how a BRP `world.insert_components` payload becomes a component value and where the panic originates.
- `tests/remote_lifecycle.rs` and `fux-fuzz/src/scenarios/raw.rs` for the existing BRP test and fuzz style.
- `fux-agent-exercises/src/scenarios/recovery.ts`, `src/tool.ts`, `src/runner.ts` and `tests/positive-control.test.ts`.

Do not paraphrase the findings from the PR description. Reproduce each one you are fixing first, using the evidence script or a test, and record the reproduction in the commit.

## F1: a partial component payload kills the server

**Cause.** The reflect deserializer builds a dynamic struct from whatever fields arrive and does not reject missing ones. `insert_reflect` then calls `from_reflect_with_fallback`, which tries `FromReflect` (fails on a partial struct), then a reflected `Default`, then a reflected `FromWorld`, and panics when none is registered. Five components register `Component` with no fallback: `Viewer`, `Launch`, `PaneView`, `Prefix` and `Overlay`. When a type registers `ReflectDeserialize`, the deserializer produces the concrete type instead, so a missing field becomes a serde error and the fallback is never reached.

**Required change.**

1. Add serde `Serialize`/`Deserialize` derives and register `#[reflect(Component, Serialize, Deserialize)]` on `Viewer`, `Launch`, `PaneView` and `Prefix`. A partial payload must then be rejected with a JSON-RPC error naming the missing field, which is the behavior the README already promises for commands and the pattern `Status`, `Viewing`, `OnTab` and `Focused` already use. Confirm that `world.get_components` output for these four is byte-identical before and after; for `PaneView` the entity must still serialize as the bare ID.
2. Do not route `Overlay` through serde. Its `Target`, `Mode`, `Entry`, `Run` and `Action` shapes are documented as reflect enum encodings and must not drift. Give `Overlay` a reflected `Default` (or `FromWorld`) with a neutral `Mode` instead, and check that its `on_insert` hook tolerates that value.
3. Add a unit test that walks the app's type registry and asserts every registration carrying `ReflectComponent` also carries `ReflectDeserialize`, `ReflectDefault` or `ReflectFromWorld`. This is what stops the next reflected component from reopening the hole. It must fail on the current branch.
4. Add an integration test in `tests/remote_lifecycle.rs` that sends the exact `Viewer`-minus-`notice` payload from the evidence script, asserts a JSON-RPC error, and asserts the server still answers `rpc.discover` and the viewer still paints. Cover `Launch` with argv only, `PaneView` with `{}` and `Prefix` with `{}` in the same test.
5. Extend the raw-mutation fuzz scenario in `fux-fuzz/src/scenarios/raw.rs` so raw inserts sometimes omit fields. The invariant is the existing one: the server never panics.
6. Reflecting `Launch` through serde makes a read-modify-write of `Launch` over BRP an accepted request. Check whether replacing an existing `Launch` re-triggers `Added<Launch>` in `spawn_terminals` and leaks or duplicates a PTY. If it does, fix that as part of this change and test it; if it does not, say so in the commit message with the reasoning.

Do not catch the panic, wrap the BRP system, or add a fux-side allowlist of components. The fix is in the type registrations.

**Harness follow-up.** The crash-detection test in `fux-agent-exercises/tests/positive-control.test.ts` ("a fux server that dies mid-run is reported as a crash") induces the crash with this payload and asserts on `panicked at` in the log. Once fux is fixed it must fail. Change it to kill the server by its recorded pid with SIGKILL and assert on the crash being detected, without the panic-text assertion. Move the partial payload into a new positive test asserting the server survives and the tool returns the JSON-RPC error. Update the evidence script's header comment so it documents the fixed behavior and exits non-zero if the server dies.

## F3 and F5: README gaps

Add to the remote-control section of `README.md`, adjacent to the paragraph that documents `reorder` and `order`:

- `scroll {order}` moves the focused pane's history by half the viewer height; `previous` shows older output and `next` newer, clamped to retained history. `Viewer.scrollback` is the resulting offset in lines above the live bottom; zero is live. State this in the README's existing terse register and add one `fux rpc` line for `scroll` to the sample block. The wording must match `execute` in `src/server.rs`; do not describe behavior you have not read.
- The full component paths `bevy_ecs::hierarchy::ChildOf` and `bevy_ecs::hierarchy::Children`, named once, with one `world.get_components` example that reads a pane view's parent. The architecture section already uses the short names; leave it alone.

Rerun the `noisy` and `discovery` scenarios afterwards (see "Verification") so the README change is measured, not assumed.

## F8: the recovery disruption fires too early

The disruption in `recovery.ts` fires on the first response that mentions the target id, which is always the opening query, so no run ever met a stale-target error. Firing on the first `close` or `focus` that names the target is necessary but not sufficient: `onToolCall` runs after the request has completed, so the agent's own close has already succeeded by then.

Add a pre-forward hook to the scenario interface in `src/scenarios/types.ts`, invoked by `src/tool.ts` before a request is sent, receiving the method and parameters. Keep it as small as the existing `onToolCall`. Fire the recovery disruption from that hook on the first `Control` whose command is `close` or `focus` naming the target pane view, so the agent's request then fails with "target no longer exists". Record the triggering call index and method as today. Update the scenario's header comment and the positive control for `recovery` so it drives through the new trigger and asserts `sawStaleTargetNotice` is true.

## F2, F4, F6, F7

- F7 is fixed in the PR. Leave it, but make sure `promptRevision` is still recorded.
- F2 has no code fix. Do not add prompt wording aimed at discouraging fabrication; that would change the experiment.
- F4 (a bounded plain-text history read) stays out of scope. Do not add a `fux.text` method or anything like it here.
- F6 is a baseline. Do not touch it.

## Constraints

- Fixes go in separate commits per finding, on `feat/agent-exercises`, each with a message that states the reproduction, the cause and the test that covers it.
- No new dependencies in fux, fux-fuzz or the harness. The harness keeps zero npm dependencies and no lockfile.
- No changes to the exercise prompts, budgets, model, or tool semantics beyond F8. The measurement must stay comparable with campaigns 01 to 03.
- Do not weaken the README's existing statements to make the new ones fit.
- Do not modify fux to make an exercise pass. Modify fux only where a finding classifies the cause as a fux bug or documentation gap.

## Verification

All of the following must pass and be quoted in the final report:

- `cargo test --locked`, including the new registry audit and the F1 integration test.
- `cargo run -p fux-fuzz -- raw` (or the equivalent scenario invocation) for long enough to exercise the new partial-insert mutations, with no panic.
- `./fux-agent-exercises/evidence/partial-component-panic.sh target/release/fux PORT` reporting the server alive after the partial payload.
- `node --test --test-concurrency=1 "tests/*.test.ts"` in `fux-agent-exercises`, including the changed crash test and the new survival test.
- One fresh campaign, `node run.ts campaign --repetitions 2 --artifacts runs/campaign-04`, on the same pinned model as campaigns 01 to 03. Report it in `FINDINGS.md` as campaign 04 with the same table, and in the per-finding sections record: for F1, that the organic path now yields an error and zero server crashes; for F3, how many `scroll` attempts were accepted on first try; for F5, how many runs still spent a `world.list_components` call on hierarchy discovery; for F8, how many `recovery` runs saw the stale-target notice. If any number did not improve, say so and do not adjust the fix to chase it.

## Deliverable

Update the PR description: keep the findings narrative, add a "Fixes" section that lists each finding with its commit, and replace the sentence saying fux was not modified with an accurate statement of what changed and why. Update the "Ranked next steps" in `FINDINGS.md` so it only lists what remains (F4 and re-measuring F2).
