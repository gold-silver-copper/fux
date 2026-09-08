# Prompt and recovery evidence boundaries

This inventory supports R3 of [the completion checklist](agent-workflow-completion.md).
It separates tested terminal-delivery guarantees from adapter-specific guarantees.
The implementation is `zor/src/tasks/{submit,integration,binding,wait,launch}.rs`;
terminal receipts remain generic fux state. A receipt never establishes application processing.

| Boundary or failure | Required behavior | Current evidence |
|---|---|---|
| Prepared prompt, no reservation | No input until explicit submission; competing writers excluded | `tools/xtask/src/scenarios/zor_tasks.rs`: preparation, caller contention, discard and cancelled preparation |
| Fux reserved, reservation reply lost before zor records it | No bytes written. A new reservation is permitted because submission was impossible without a recorded receipt; the orphan remains bounded by fux retention | `tools/xtask/src/scenarios/zor_tasks.rs`: dropped reserve reply, retained Prepared/no receipt, distinct later reservation and unchanged input sequence |
| Reservation persisted, caller stops before submission | Retain the original receipt across CLI processes | `tools/xtask/src/scenarios/zor_tasks.rs`: separate reserve/reconcile/submit invocations |
| Adapter arm request accepted, ACK lost or wrong | Send no input; retry the exact arm and original receipt | `zor_bindings.py`: wrong/drop ACK, identical three arm requests, zero input before valid ACK |
| Adapter ACK received, caller dies before its next journal commit | Reconcile the same producer/arm; no replacement prompt identity | `capture_opencode_integration.py` and retained integration trace deliberately acknowledge a real native arm without journaling the ACK, then retire it with zero input. Socket fixtures also retry the identical arm after lost ACK; commit ordering keeps submission after durable acknowledgement |
| Durable Submitting, connection lost before fux receives input-submit | Retain uncertainty and the original receipt; status can establish still Reserved | `tools/xtask/src/scenarios/zor_tasks.rs`: dropped-before-forward submission, zero bytes/sequence, same receipt on subsequent submission |
| Fux accepted input, reply lost | Reconcile/retry the same operation without duplicate bytes | `tools/xtask/src/scenarios/zor_tasks.rs`, generic `input_receipts.py`, paired `comparisons/input-retry.md` |
| Caller killed after fux submission but before zor records reply | Persisted Submitting survives; reconcile receipt without replay | `tools/xtask/src/scenarios/zor_tasks.rs`: held submission reply, actual caller SIGKILL, exact input sequence after reconciliation |
| Queued versus delivered; stalled writer or partial failure | Bound queues; distinguish accepted bytes, delivered bytes and failure | Generic `input_receipts.py`; fux ECS input tests; zor receipt validation and partial-failure test; delayed receipt view in `tools/xtask/src/scenarios/zor_tasks.rs` is explicitly synthetic |
| Receipt recorded, later duplicate API/CLI submission | Return retained outcome; don't submit again or equate delivery with completion | `tools/xtask/src/scenarios/zor_tasks.rs`, `zor_service.py`, `comparisons/service-failure.md` |
| Receipt status reply lost, receipt expired or caller deadline reached | Preserve ambiguity and operation identity; caller timeout doesn't rewrite prompt deadline | `tools/xtask/src/scenarios/zor_tasks.rs`: delayed/dropped status, injected expiry and follow deadlines; generic ECS retention tests establish expiry mechanics |
| Response binding/report arrives or is retried | Scope to prompt/input/producer/native ancestry; immutable exact retry | `zor_bindings.py`, `tools/xtask/src/scenarios/zor_producers.rs`; separately versioned real OpenCode native traces in `zor/tests/fixtures/agents` |
| Immediate response without visible Working, stale prior blocker | Fresh prompt evidence required; no working-transition prerequisite for explicit reports | `comparisons/prompt-boundary.md`; synthetic protocol evidence, not universal passive-agent support |
| Human input, observer loss, event gap, immediate exit | Weaken correlation, report unknown/uncertain or distinct exit; never infer success from silence | `tools/xtask/src/scenarios/zor_tasks.rs`, `zor_bindings.py`, `tools/xtask/src/scenarios/zor_events.rs`, `tools/xtask/src/scenarios/zor_launch.rs` |
| Zor service killed/restarted | Retain task/receipt and original worker; do not replay prompt | `comparisons/service-failure.md`, `zor_service.py` |
| Fux unavailable versus confirmed replacement | Unavailable is Uncertain; positive incarnation replacement is Lost. Preserve old identities/receipts and never send to replacement panes | `zor_recovery.py`: outage/restore, actual server restart, durable Lost through a later outage, old reserved submission rejected and replacement input sequence zero |
| Managed lifecycle reconciliation on service startup | Reconcile retained tasks against the current fux ownership identity | `zor_recovery.py`: the startup sweep reconciles retained managed launches against a replacement server without per-task CLI reconciliation. Adopted panes remain passive observation; no management authority is inferred |
| Agent session resume after fux replacement | Recreate only under explicit task resume policy with sufficient adapter metadata, never replay pending input | `opencode-1.18.29/zor-resume.json` proves explicit same-task/new-attempt native resume, missing-metadata/cancelled-task refusals, dropped creation reply with one actual creation, retained old reservation without replay, fresh response and current-attempt verification. `resume.json` separately proves the application contract |

The current Lost state means loss of the original fux ownership identity. It is not a final
exit record, permission to delete a worktree, or proof that every old OS descendant terminated.
Existing immutable successful prompt/verification evidence remains historical evidence.
Restoring access to the exact original live target can restore Active observation. A later
outage cannot erase previously observed replacement; no replacement target is adopted.

Do not read this inventory as exhaustive crash-instruction coverage. Explicit OpenCode resume is now implemented and tested under the documented eligibility policy. The lost-ACK fixture and
commit-order review do not justify inventing another required test for every instruction
between receiving an acknowledgement and its journal commit.
Real Claude/Codex adapter guarantees and broader real-agent detection remain R4.
