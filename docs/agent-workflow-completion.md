# Finite completion checklist

Authority: [herdr-gap-closure-prompt.md](../herdr-gap-closure-prompt.md), with
[completion-steering-prompt.md](../completion-steering-prompt.md) controlling execution.
Final scope, explicitly revised by the user on 2026-09-07: R6 is deferred and outside
completion. R1–R5 and R7 are verified complete. No remote authorization, reconnect,
retention or netmon work is required to close this objective.

This checklist records acceptance evidence; historical “Next” paragraphs in the
[ledger](agent-workflow-ledger.md) describe earlier slices, not additional requirements.
Fux owns multiplexer primitives, zor all agent workflows, koh transport and authorization.

## Established requirements

| Prompt | Implemented and evidenced | Evidence |
|---|---|---|
| 1–2 | Ownership boundary, baseline/source audit, repaired composition and companion reconstruction | `agent-workflow-baseline.json`, `multiplexer-boundary.md`, ledger; `tests/agent_boundary.rs` |
| 3 | Incarnation handles, coherent conditional capture, bounded events/gaps, ordered receipts, retained final evidence, documented generic operations | `local-control-protocol.md`; `tests/ecs.rs`; `tests/verify/{event_sync,input_receipts,final_records}.py`; required real-zor integration |
| 4 | Default discovery/service, deterministic bundled startup rules and overrides, explanations, reconnect, bounded idle observation | `zor/{OBSERVATION-CONTRACT,SERVICE-API,INTEGRATIONS}.md`; `zor/tests/bundled_rules.rs`; `tests/verify/{zor_service,zor_events,zor_bindings,zor_producers}.py` |
| 5 | Separate task/prompt/outcome model, CLI/API, durable receipts, writer exclusion, human-input invalidation, bounded followers, explicit uncertainty | `zor/TASKS.md`; `tests/verify/{zor_tasks,zor_launch,zor_bindings,zor_service}.py`; real OpenCode traces |
| 6 | Owned worktrees, source/check/artifact evidence, verified handoffs, bounded manual/automatic groups | `zor/{WORKTREES,RESULTS,GROUPS,HANDOFFS}.md`; `tests/verify/{zor_workflow,zor_groups,zor_group_scheduler,zor_checks,zor_sources,zor_changes}.py` |
| 7 | Private durable journal, controller restart preserving workers, recorded-stop/check recovery, dashboard/JSON/focus/filter/attention/notifications, single-command startup | `zor/{RECOVERY,DASHBOARD}.md`; `tests/verify/{zor_recovery,zor_dashboard}.py`; service-failure comparison |
| 8.1–3, 8.6 | Reproducible narrow prompt/recovery and headless setup comparisons; not universal superiority | `comparisons/{prompt-boundary,input-retry,service-failure,controller-setup}.{md,json}` and corresponding `tools/comparisons` scripts |
| 9 | Slice owner checks, mandatory reconstructed real integrations, structural boundary checks and independent reviews | Detailed command/results/review records in ledger; final whole-change gate remains below |

“Established” applies to the listed behavior, not every clause of its prompt section.
The final-source reconstruction passed all 17 real-zor scenarios (227.91 seconds), both
controlled fixture suites and every preceding non-R6 check. Only the subsequent R6 gateway
runtime phase failed at netmon initialization. Record:
`/tmp/fux-review-final-reconstructed-v2.log`. Final zor owner checks passed on Rust 1.91.0:
85 library, 8 bundled-rule, 2 CLI, 6 OSC and 9 passthrough tests; strict clippy passed.
The earlier slice records below are historical; the final handoff controls current status.

## Acceptance checklist

| ID | Required work | Concrete acceptance / stopping condition |
|---|---|---|
| R1 — complete | Coherent fixture contention handling (steering; §5/9) | One documented retry policy across affected fixtures: exact Busy identification, original deadlines and operation identity, errors never accepted as expected semantic rejection, non-idempotent steps inspect committed admission before retry. Controlled boundary tests and affected real fixtures pass. |
| R2 — complete | Reliable worktree/launch recovery (§6–7) | Disposable tests cover worktree-created/pane-not-created, pane-created/reply-lost, interrupted cleanup and active owned-checkout use including unadopted fux panes. Retain ambiguity when evidence is gone; provide explicit safe reconciliation/resolution without replay or ownership expansion. Existing dirty/main/symlink/cancellation guards continue passing. |
| R3 — complete | Fux-restart and adapter recovery policy (§5/7) | Document and test lost versus resumable state, allowed launch/resume action and necessary metadata; no implicit resume authorization or pending-input replay. Inventory every submission/receipt boundary against a fixture, adding only uncovered required cases. Explicit unknown/lost is valid where correlation cannot be recovered. |
| R4 — complete (recorded scope) | Required detection evidence (§4) | Retain versioned real-agent startup/working/input-required/idle/exit/resize/unknown coverage for the initial Claude/Codex/OpenCode set, and all listed negative classes including nested tools. Audit relevant herdr inventory and expand only with real evidence; identify unsupported or inaccessible agents explicitly. Synthetic provider traces must be labeled. Missing evidence remains unverified, never inferred from a process name. |
| R5 — complete (bounded scenarios) | Herdr measurements (§8.4–7) | Retain runnable, provenance-backed paired scenarios for detection FP/missed blockers/freshness/coverage; two-worker verification/artifacts/cleanup; setup commands/actionable errors; idle/burst capture traffic, latency, memory and pane scaling. Record parity, superiority, unsupported interfaces and measurement restrictions separately. No obligation to win every metric. |
| R6 — explicitly deferred | Remote scope and reconnect (§7), outside completion | Remote authorization and within/beyond-retention runtime assertions remain unverified because netmon initialization fails with EPERM. Existing work is retained; no further R6 execution is part of this objective. |
| R7 — complete | Final delivery (§9), excluding deferred R6 | Final documentation matches behavior; intended fux/zor/koh changes and companion patches receive complete independent review. Fix confirmed in-scope defects, rerun affected checks, export/verify patches, run applicable owner gates and final reconstructed gate. Report each R item with evidence or exact blocker. |

R1 evidence: `tests/verify/zor_contention.py` and its seven controlled fault tests;
all seven affected real fixtures passed (service, manual groups, automatic groups, bindings,
workflow, producers, dashboard). Record: `/tmp/fux-zor-contention-reviewed-targeted.log`,
source/binary hashes: `/tmp/fux-zor-contention-verification.json`. Independent review found
and verified fixes for reset nested deadlines; final review found no remaining issue.
`git diff --check`, `cargo fmt --all -- --check`, Python compilation, and
`python3 tools/dependencies.py verify` passed. Production binaries and companion patches
were unchanged; the previous production checks remain applicable. No full reconstructed
build or known blocked koh runtime check was repeated for this fixture-only slice.

R2 evidence: zor's bounded local fux census now protects unadopted launch cwd, changed cwd
and descendant cwd; normal/forced removal and missing-manager refusals pass in the service
fixture. That fixture also retains a created worktree through a pre-pane launch failure and
successfully retries the exact launch after availability returns. `tools/xtask/src/scenarios/zor_launch.rs` proves lost
creation-reply and killed-creator reconciliation without duplicate panes. `zor_worktree.py`
proves interrupted creation/removal, including explicit external completion followed by
reconciliation. Ambiguous destructive operations are not automatically replayed: automatic
partial-removal retry is not an additional requirement. Lost/resumable state after fux restart
remains R3. Logs: `/tmp/fux-zor-unadopted-{service-final,launch,standalone,workflows}.log`.
All ten affected real fixtures passed; Rust 1.91 owner tests, strict clippy, protocol-only tests,
formatting, package verification and companion reconstruction passed. Independent review
was unavailable because the reviewer hit a usage limit; a separate complete slice review
covered production, tests, documentation and exported companion changes. Final full-change
independent review remains R7. No unchanged koh runtime check was repeated.

R3 evidence: `launch-reconcile` distinguishes unavailable from confirmed replacement fux
ownership and retains Lost across subsequent outages. Service startup performs a finite sweep,
including submitted resume intents; it never submits Prepared intent. The
[boundary inventory](prompt-recovery-boundaries.md) maps submission/receipt stages to fixtures.

`zor task resume TASK --operation OP --instance INSTANCE` now explicitly authorizes one native
OpenCode session recreation. It retains HOME/XDG settings and native message/session binding,
requires open/reconciled task state and absent original root process, and refuses pinned group
membership or unresolved checks. Attachment archives the previous launch and advances the same
task while preserving policies, old attempts, receipts and evidence. Missing metadata and other
adapters remain unavailable for resume; Lost is not exit evidence or expanded cleanup authority.

Real versioned `zor/tests/fixtures/agents/opencode-1.18.29/zor-resume.json` proves a fux restart,
same native conversation, new process/session/attempt, unchanged old reserved prompt, zero replay,
and a fresh response. Dropped creation reply produces uncertainty; retry creates no second pane.
The resumed attempt passes its required source-bound check and is explicitly verified while the
old reservation remains untouched. Nine evidence/mutation tests pass. Records:
`/tmp/zor-explicit-resume-final-reviewed{.log,/resume.json}`. Targeted launch/source/recovery/producer/group
fixtures and the final source/workflow batch pass; Rust 1.91 owner tests and strict clippy pass.
The real model fixture separately proves retained source/check/artifact/change associations.
No universal native-adapter resume support or comparative advantage is inferred.

R4 progress: audited all 21 herdr source manifests with declared states/regions, hashes and
local executable availability (`comparisons/detection-inventory.{md,json}`). Captured installed
Pi 0.80.10 startup without provider configuration; no Pi rule or working-state support is inferred.
The initial three agents now have real 80→39→80-column resize and explicit pane-release traces,
validated as no-input evidence; narrow screens remain Unknown in production rule tests. Final
exit status is unknown, so natural exit remains unverified. These probes exposed a generic fux
closed-tab ownership bug: final evidence was lost when the tab entity disappeared before its
terminating pane. Fixed lifecycle ordering, with deterministic regressions and real final/launch/
recovery checks. R4 remains open for the missing agent states and detection evidence above.

R4 additional progress: real authenticated Codex 0.153.4 capture now retains directory trust,
working, response display and application `/quit` exit status 0. The user authorized account/API
key use; the probe copied only its login cache into a private temporary home and used one no-tool
prompt per run. Zor now recognizes the recorded trust and narrow working layouts. Response display
stays Unknown, separate from task verification; broader idle and nested-tool evidence remain open.
Versioned evidence: `zor/tests/fixtures/agents/codex-0.153.4/{authenticated,approval}.json`; eight offline
acceptance/mutation tests and eight bundled-rule tests pass. The separate unanswered command-approval
trace is now recognized as Blocked; its forced-release exit status remains unknown. No agent-specific fux code was added.

R4/R5 progress: added passive OpenCode permission/question rules from existing native UI traces.
The paired production screen comparison (`comparisons/detection-screens.{md,json}`) now records
77 cases: eleven real screens and 66 derived negatives. Zor matches eleven real screens and misses
zero of six blockers; herdr matches eight and misses three. Zor has zero non-Unknown negatives;
herdr has 45 Idle fallbacks without visible evidence and 21 matched-rule negatives. These are
fixture conformance results, not held-out accuracy; OSC/title paths and live freshness are not
measured. The JSON preserves these distinctions, with four offline provenance/accounting tests.
R4 broader real-agent states and R5 live detection/workflow/usability/resource measurements remain.

R4 additional progress: real authenticated Claude 2.1.263 (bare mode, Sonnet 5, tools disabled)
now has working/response and native `/exit` status0 evidence. Only the authorized exported
Anthropic key is supplied; its preview is redacted. A narrow zor working rule uses the interrupt
footer, while the recorded manual-mode composer now has narrow Idle recognition. Four offline capture/redaction tests pass. Capturing
`--tools ""` exposed a generic argv restriction; fux config/control and zor launch now preserve
empty arguments after the executable. Generic config/new/split/final-record and real managed
launch/reconciliation fixtures verify that behavior. Claude tool approval and real nested-tool evidence remain open; other idle layouts remain unverified. The paired screen report now also includes its working/response screens.

R5 §8.6 evidence: `comparisons/controller-setup.{md,json}` records three paired runs of a
prepared headless setup path and unavailable-service diagnostics. Both reach the actual fixture
pane. Two non-polling startup invocations for fux+zor versus three for herdr are separated from
scheduling-dependent additional reads; no human-effort or universal usability score is inferred.
Both errors provide recovery commands; herdr's absent-server CLI error is also structured JSON.
Zor controller shutdown preserves its worker; equivalent separate herdr controller stop is
unsupported. Five mandatory offline report tests and all six real runs pass. The unchanged herdr
reference was rebuilt after the old temporary build disappeared, with new verified build provenance.
R5 §8.7 partial evidence: `comparisons/resources.{md,json}` retains 18 real paired cases
(three repetitions at 1/4/8 observed panes), with calibrated CPU counters, RSS/footprint,
actual viewport sizes, output-visibility timing and normal owner/worker cleanup. Four mandatory
offline evidence tests pass. Fux+zor uses less idle CPU at 4/8 panes in this sample; herdr
uses less burst CPU and memory at all three sizes. Output timing varies by size. Debug builds,
unequal headless viewports and short fixed-order runs limit extrapolation. This establishes
bounded idle/burst resource and pane-scaling measurements, not observer capture traffic.
R5 §8.7 capture evidence: `comparisons/capture-traffic.{md,json}` adds nine live runs at
1/4/8 panes. Transparent owned proxies count only zor workspace IPC: zero captures in all
4.2-second idle windows, fresh burst-marker captures for every pane, and retained request/reply/
text/event byte accounting. Four mandatory offline evidence tests pass. Herdr's corresponding
detection text reads are in-process and remain unmeasured by this interface, explicitly null;
no capture-traffic superiority claim is made. Proxy timings are not production-state latency.
R5 §8.5 evidence: `comparisons/workflow.{md,json}` runs the existing full zor two-worker
acceptance and the corresponding live herdr worktree workload. Both create isolated workers,
produce/repair outputs, deliver a handoff and protect dirty trees until explicit force. Zor
retains source-bound artifacts and enforces verified predecessors; herdr's matching checks,
collection and handoff policy are external harness work, not built-in verification. The
inspected default herdr API lacks that policy interface; its unsupported method response is
retained. Four mandatory offline evidence tests and both live branches pass. This is a
scripted workflow comparison, not a real-agent guarantee. Prior broader crash/cancellation/
missing-artifact owning-fixture coverage remains separate.
R5 §8.4 live freshness evidence: `comparisons/detection-freshness.{md,json}` retains six
real signed-out Codex runs. Zor reports the independently visible sign-in blocker after
179–228 ms in the sampled configuration; herdr discovers Codex but stays Unknown during each
roughly three-second window. Both remove explicitly closed panes; all recorded Codex PIDs
exit. Four mandatory evidence tests pass. API/CLI polling, startup and unequal default layouts
limit timing comparisons. This closes the bounded live-freshness measurement, not broader
working/idle transitions or all-agent accuracy, which remain R4. With the screen, resource,
traffic, setup and two-worker reports above, R5's required measurement categories now have
retained outcomes or explicit unsupported-interface limits; no universal superiority is claimed.

## Environment-blocked verification

- Koh runtime initialization fails with `Operation not permitted` in netmon: latest combined
  koh stage had 8 passes and 2 failures; later gateway stages were not reached. Prior explicit
  gateway runs had the same restriction. Do not rerun this unchanged blocker until the final
  required gate or a relevant environment/code change. Static/reconstruction evidence does
  not establish remote runtime acceptance.
- `ps` remains blocked (`Operation not permitted`), but owned-process resource sampling is
  available through macOS `proc_pid_rusage`. The retained
  `comparisons/resource-sampler-check.json` validates Mach-time conversion against the process
  CPU clock and resident/footprint growth after touching a 32 MiB allocation. This removes
  the sampling-method blocker; paired idle/burst and scaling measurements remain R5 work.
- Actual desktop popup rendering is unverified. Disposable backend execution/error/timeout/
  cleanup coverage exists; visual OS delivery is not a reason to add notification features.

## Outside the completion path

Shared service waiters instead of existing bounded followers; a monolithic frozen result
object beyond retained sealed evidence; speculative new fux discovery/schema APIs; automatic
re-correlation after human input where explicit invalidation suffices; universal native adapter
support; global process/file-use surveillance; automatic branch deletion, plugin garbage
collection, graphics, floating panes, marketplaces, broad platform parity and live server handoff.
These are not new acceptance requirements. Active-use checks and required recovery cases above
remain required; this exclusion must not excuse a demonstrated unsafe deletion or replay.

## Stopping condition

Finish when the required workflows and selected improvements are evidenced, R1–R5 and R7
are satisfied, and non-R6 review/gates have no unresolved required failures. R6 is explicitly
deferred by the user and is not a completion gate. If an external
restriction prevents a required outcome, hand off that exact incomplete item and remaining
verification rather than claiming full completion. Do not append optional enhancements to
this checklist. Add work only for an uncovered original clause or a confirmed in-scope defect.

R4 progress: Claude’s existing authenticated capture now drives an input-ready rule
for its version/model header and complete manual-mode composer/footer, excluding active
spinners and interrupt hints. Production-terminal tests cover pre-prompt and post-response
Idle plus derived negatives; all eight bundled-rule tests pass. Idle does not verify work.
The refreshed 77-case comparison matches 10/11 real screens in zor versus 8/11 in herdr,
with all 66 zor negatives still Unknown. Codex idle and remaining real tool-state evidence
remain open; no new real-agent capture was claimed for this rule change.

R4 progress: Codex’s recorded response composer now has narrow Idle recognition with
its version/header/footer, excluding interrupt hints, Working and MCP startup rows.
All eight bundled-rule tests pass; loading remains Unknown and trust/approval stay Blocked.
The refreshed paired screen report matches all 11/11 selected real screens in zor, with all
66 derived negatives Unknown; herdr remains 8/11. The six real signed-out freshness runs
were repeated on the new rule source: zor Blocked at 179–228 ms, herdr Unknown throughout
its window; owned process cleanup passes. These remain fixture-specific measurements.

R4 scope correction from the original §4: “Negative fixtures must include ... nested tools”
requires nested-tool negatives, already present in the 66 derived cases. It does not require
real nested-tool execution for every agent. Prior prose listing that as mandatory was too
broad. Real-agent startup/working/input-required/idle/exit/resize/unknown evidence still needs
an explicit final matrix audit; do not add capture work solely to satisfy the broader wording.

R4 final audit: [agent-state-coverage.md](agent-state-coverage.md) maps every required
state category for the initial three agents to inspected fixtures, with source hashes in
`agent-state-coverage.json`. OpenCode working evidence is native busy/activity under a
synthetic provider, not a passive Working rule. Its exit evidence is explicit forced loss
with unknown status, not natural success. Claude theme choice satisfies input-required;
tool-specific approval remains unverified. These distinctions follow original §4 and do
not create extra requirements. All six required negative classes are covered by the66
retained derived fixtures. Lifecycle4, native-event8, integration88 and startup2 evidence
tests pass; current bundled-rule8 and unchanged authenticated/screen/freshness checks are
reused. R4 is complete for this recorded scope. R6 and R7 remain open.

R6/R7 in progress: added a mandatory real fux/real zor gateway test for distinct
attachment/control identities and allowlists; compile and strict clippy pass, but runtime
fails at netmon EPERM before authorization assertions. `zor/REMOTE.md` documents endpoint
separation without claiming OS isolation from commands an attached shell can already run.
Successful runtime evidence beyond the actual30-second retention window remains required. The
reconstructed comparison gate now includes pinned herdr source instead of skipping source
hash checks; CI runs the six comparison validators. The milestone reconstructed run passed
all17 real zor scenarios, then hit the known two koh netmon failures (log:
`/tmp/fux-final-reconstructed.log`). It predates the latest R6 test, so is not final-source proof.
Independent whole-change reviews completed. Fixed and independently re-reviewed all five
confirmed findings: stale OpenCode same-message fetch admission, invalidated observations
revived by fresh heartbeat, supported journal directory capacity, historical resume rows
reported as unattached, and observer cleanup skipping remaining owners. The dashboard
row-overflow allegation was retracted after checking combined session limits.

Review-fix evidence: all85 zor library tests, bundled8, CLI2, OSC6 and passthrough9 pass
(`/tmp/zor-review-final-owner.log`); strict clippy passes. Four delayed-fetch adversarial
cases pass in the Node adapter suite. The real OpenCode resume was rerun with the current
adapter and now proves archived launches do not produce dashboard attention rows;
10 provenance/mutation validators pass (`/tmp/zor-review-fixed-resume.log`). The real
observer fixture and two controlled cleanup failures pass; independent re-review accepts
both cleanup and companion fixes. Stale Codex/Claude bundled-rule counts were corrected.

R6 now also has a real beyond-retention scenario: wait the actual30-second production
interval plus one second, reject the old authenticated token without new application
connection or input replay, then require a fresh attachment to preserve shell PID and
advance its input counter exactly once. Independent review accepts the sequencing;
its identified early-bind directory cleanup gap is fixed with an owned-directory guard.
The targeted runtime still fails at netmon EPERM before exercising those assertions
(`/tmp/koh-actual-retention-runtime.log`). At that earlier checkpoint, R6 runtime acceptance and the full gate remained blocked.
The user has since explicitly deferred R6; the successful non-R6 gate evidence remains valid. No credential access can resolve this OS
monitor permission failure.

Final-gate provenance correction: the first review-final reconstruction stopped at the
controller-setup source-hash assertion because dashboard.rs had changed. Reran all six
real paired cases and replaced the report with new transcripts/hashes; all cases pass.
Current totals are3/3/3 invocations for zor and6/6/6 for herdr, including polling reads;
intrinsic setup remains2 versus3. Separately reran the real OpenCode question/adapter-reload
integration with the current adapter and retained the new producer-reload fixture; all88
integration validators pass. These are new executions, not reassigned historical hashes.

## Final handoff — 2026-09-07

| Requirement | Final state |
|---|---|
| R1 contention | Complete; operation-specific replay rules and affected real fixtures pass. |
| R2 worktree recovery | Complete; ownership census, interrupted operations and refusal/recovery evidence retained. |
| R3 explicit resume | Complete for documented OpenCode policy; fresh real resume and dashboard trace passes. Other adapters remain explicitly unavailable for native recreation. |
| R4 agent-state evidence | Complete for the recorded initial-agent scope; passive/native, synthetic-provider and unknown-exit limitations remain explicit. |
| R5 comparisons | Complete for bounded scenarios; refreshed setup transcripts pass. No universal superiority or unmeasured herdr capture-count claim. |
| R6 remote runtime | Explicitly deferred, outside completion. Authorization and within/beyond-retention runtime assertions remain unverified due to netmon EPERM. |
| R7 delivery | Full intended fux/zor/koh tracked/untracked source review and re-review complete, no outstanding confirmed findings. Formatting, lint, owner checks and companion reconstruction pass. All non-R6 final checks pass; deferred remote runtime failures do not gate this revised objective. |

Final commands/results:

- `cargo +1.91.0 test --manifest-path zor/Cargo.toml --locked`:85 library,
  8 bundled-rule,2 CLI,6 OSC and9 passthrough tests pass.
- `cargo +1.91.0 clippy --manifest-path zor/Cargo.toml --locked --all-targets -- -D warnings`
  and corresponding koh command on1.95.0 pass. Owner formatting checks and whitespace checks pass.
- `python3 tools/dependencies.py export` and `python3 tools/dependencies.py verify`:both
  companions export and reconstruct byte-for-byte.
- `python3 tools/dependencies.py verify --build`:current reconstruction builds; expanded
  Node suite,123 retained native/lifecycle evidence validators,25 comparison validators,
  boundary3/ECS30/local-CLI11/structure8, and the mandatory real-zor wrapper pass. That wrapper
  runs17 real scenarios plus contention/cleanup controlled suites, completing in227.91s.
  Koh gateway library then reports8passed/3failed, all failing to create netmon with
  `Operation not permitted (os error 1)` before transport assertions.
  Log: `/tmp/fux-review-final-reconstructed-v2.log`.
- With current absolute `FUX_BIN`, `ZOR_BIN`, `KOH_REQUIRE_FUX_BIN=1` and
  `KOH_REQUIRE_ZOR_BIN=1`, `cargo +1.95.0 test --manifest-path references/koh/Cargo.toml --locked --test gateway`
  reports0passed/3failed, each at the same netmon boundary. Log:
  `/tmp/koh-final-gateway-integration.log`. This separately covers the command the combined
  fail-fast gate cannot reach after the gateway-library failures.

The revised objective is complete: no unresolved non-R6 finding or verification blocker
remains. The previous check handles are terminal/missing; nothing was restarted because
observation was interrupted. The final-source non-R6 results above are reused unchanged.
Companion reconstruction and final document/whitespace checks were reconfirmed after the
scope update. No production code, benchmark evidence or test assertions changed in this
closure pass; all non-R6 checks remain mandatory.

Independent review covered the complete intended tracked/untracked fux/zor/koh diff and
all five fixes. Root reviewer `review_submission` accepted observer cleanup and refreshed
setup evidence. Companion reviewer `review_check_recovery` accepted the OpenCode race,
capacity and both dashboard fixes, independently rerunning 85 library tests, the expanded
Node suite, 10 resume validators and 88 integration validators. No confirmed finding was
left unresolved; the additional dashboard overflow allegation was rejected after checking
the combined session limit. R6 runtime outcomes remain unverified as recorded above.

No CI run, commit, push, PR mutation or external message was requested or performed.
Fux remains a multiplexer/API, zor owns agent behavior, and koh owns transport/authentication.
