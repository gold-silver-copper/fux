# Requirement audit — blocked on missing historical failure evidence

Objective: execute `improve-fux-zor-codebase-prompt.md`, without narrowing its scope.
This evidence worksheet supplements `docs/codebase-improvement-report.md`.

| Requirement | Current evidence and scope | Verdict |
| --- | --- | --- |
| Preserve unrelated work and provenance | Later external checkpoint, retained dirty diff/manifest, full-gate before/after source identity, release binary hashes. Original initial dirty snapshot was lost; report explicitly discloses this and does not substitute HEAD. Companion repositories and publication are outside this execution. | Verified with disclosed historical evidence loss |
| 1. Baseline and architecture contract | Current report assigns terminal/runtime ownership to fux, orchestration policy to zor and transport to local-ipc; manifests preserve those crate boundaries. Baseline failure logs distinguish fixture routing from runtime races. | Verified |
| 2. Named integration failures | Full gate passes `zor_group_scheduler`, `zor_groups`, `zor_recovery`, `zor_service`, `zor_headless` and all 24 automation scenarios. Controlled launch exits guard pin-release windows; explicit resume regression reproduces and fixes the archived-launch lookup. Scenario assertions retain immutable instance/pane/PID, moved-resource and adopted-ownership checks. | Verified |
| 3. Typed boundary | `fux/manager.rs` and `fux/input.rs` validate typed operation/reply/status/identity and distinguish pending from malformed evidence. All migrated route/submit/binding/integration/launch/wait/run callers reviewed. Sixteen identical producer/consumer fixture pairs round-trip actual producer enums; consumer tests exercise wrong operation/malformed replies. Full contract tests and fux/zor/local-ipc package checks pass. | Verified |
| 4. Interaction ownership | `history.rs`, `capture.rs`, `interaction.rs` centralize retention, gesture ownership and key/frame completion; ordered effects and correlated reads remain dedicated owners. Full controller/model and real-PTY tests pass, including independent histories, one-Escape dismissal, fragmented prefix/paste, mouse tails, target invalidation and bounded scheduling. Twenty original and nine final-gallery image inspections cover affected normal/tiny states. | Verified |
| 5. Lifecycle and recovery | Shared launch/attachment/delivery/worktree transitions used by normal and recovery callers. Transition contract records intent/effect/publication/reconciliation; adopted/historical stop is rejected. Group admission retains its existing policy owner and calls shared delivery. Unit and real launch/task/group/recovery/worktree/resume scenarios pass. Persisted crash-state fixtures are explicitly distinguished from actual killed callers and fsync injection. | Verified within stated failure-injection scope |
| 6. Reusable infrastructure | Focused viewer modules retain substantive assertions; mouse/transfer/tiny bodies match the checkpoint after renaming, history retains extracted gesture assertions and stronger synchronization. Independent 256 × 128 controller model, minimized trace replay and real-PTY counterparts pass. Documented targeted/full commands reuse existing bounded gate_process. Full entry point executes successfully with no exclusions. | Verified |
| 7. Diagnostics and automatic artifacts | Opt-in bounded private logs; no prompt/input payload fields. Failure probes retain source/binary identity, RPC metadata, final screens and standard-root diagnostics after teardown. Tests cover file/record/event bounds, contention and FIFO/symlink refusal. Generated model saves seed/minimized trace separately. | Verified; custom diagnostic directories are not automatically discovered |
| 7. Release performance | Both release builds and candidate benchmark smoke checks pass. Alternating three-block comparison covers idle/output, many viewers, 2/8/32-pane resizing, retained history memory, rendered scrolling, manager lookup, journal operations and actual lost-create-reply recovery. All 48 main comparison runs passed; raw samples and host observations are retained. The focused follow-up failed in a slow-reader case with insufficient phase evidence; diagnostic runs and the fixed 200-case reproduction campaign passed without reproducing it. Four measured release binaries were confirmed fresh and unchanged. | Verified measurements and regression investigation; historical timeout is assessed separately below |
| 8. Verification | The final full gate passes all 40 checks, exact Betamax replay/report and unchanged source identity, including the readiness-based proxy. After benchmark-only edits including failure diagnostics, strict harness Clippy and 36 library + 36 binary tests pass; Rust 1.95 workspace/all-target check passes. One ad hoc invocation omitted the documented CJK font and failed its wide-glyph test; explicit-font rerun and original failure logs retained. | Verified automated scope |
| 8. Final report and separate review | Separate source review completed in `final-review.md`; it identifies/fixes the resume defect and verifies remaining changes. Final source scope includes 57 compilation sources, fixture pairs, inventories and relevant untracked documents. Reviewed PNGs are retained locally. Main and focused performance analyses are retained; retry latency attribution is documented, while the historical timeout remains open before claiming the objective complete; the final integrated rerun passed. | Verified deliverables and review; unresolved timeout explicitly disclosed |
| Outstanding pressure-timeout requirement in the resume instructions | The original failing phase, rejected-control status and terminal state were not retained. Failure diagnostics and strict control-response checks now exist; all 200 fixed reproduction cases passed. Source and power-log investigation did not establish the cause. No deterministic regression reproduces that historical failure. | Incomplete: cause and causal regression evidence remain unavailable |

Representative named invariant tests, confirmed in current source and covered by
workspace verification:

- `wheel_browsing_is_passive_and_accepts_another_pane`
- `passive_histories_resume_only_the_focused_pane_and_expire_by_identity`
- `copy_escape_dismisses_instead_of_requesting_commands`
- `canceled_modes_keep_owning_unfinished_pastes`
- `cancelled_lookup_cannot_populate_a_reopened_chooser`
- `replies_match_both_identity_fields_and_do_not_extend_deadlines`
- `queue_is_bounded_ordered_and_rejects_a_replacement_workspace`
- `manager_delayed_input_is_byte_exact_and_server_targeted`
- `attachment_records_once_and_final_attachment_does_not_claim_task_success`
- `adopted_session_cannot_receive_stop_intent`
- `delivery_rejects_replacement_and_regression_without_partial_mutation`
- `receipt_publication_preserves_terminal_wait_and_release_evidence`
- `uncertain_creation_cannot_be_resubmitted_or_change_pane`
- `outage_does_not_erase_replacement_evidence_and_live_keeps_needs_input`
- `lost_removal_cannot_repeat_or_change_force_and_closure_needs_intent`

Limits requiring accurate final reporting: no native OS-input validation or live
provider conversation-restoration claim; bounded generated traces are not exhaustive;
manual visual inspection is selected by affected transition, not every frame;
resource observations must distinguish sampled RSS and viewer decoder backlog from
kernel peak memory and internal runtime queues. The historical pre-task dirty
baseline remains unavailable. None of these limits authorizes treating the unresolved
pressure timeout as fixed; its original diagnostic evidence cannot be recovered
from the retained generic error. The performance comparisons themselves are complete.
