Implement the next bounded headless milestone for fux + zor + koh.

Read `headless-architecture-performance-prompt.md`, `docs/headless-final-verification.md`, `docs/headless-milestone.md`, and the retained performance evidence first. Treat the previous milestone as complete within its documented non-R6 scope. Preserve existing user changes and reuse unchanged valid evidence.

Do not commit, push, open PRs, or request routine confirmation.

## Fixed boundaries

- **fux:** PTYs, terminal state, panes/layouts, rendering, and generic input/output/lifecycle APIs. No agent detection, provider integration, task state, Git policy, or verification policy.
- **zor:** all agent interpretation, native adapters, orchestration, worktrees, checks, artifacts, recovery, and presentation. Access fux exclusively through its public API.
- **koh:** authentication and opaque byte transport. No interpretation of panes, prompts, tasks, or outcomes.

Each component must remain independently buildable and testable. Add abstractions only where a concrete implementation or test requires them.

All required work must run headlessly with ordinary user permissions. No sudo, system permission changes, interactive approval dialogs, privileged monitoring, external accounts, or paid calls may be required.

Optional bounded paid model calls are authorized using existing credentials when useful for real integration validation. Protect credentials and distinguish real-provider evidence from fixture conformance.

**R6 remains explicitly deferred.** Do not pursue live remote authorization, network reconnect, retention-window acceptance, or netmon runtime work.

## 1. Establish the finite scope

Inspect current source, installed provider interfaces, verification tooling, and retained evidence.

Create one concise acceptance checklist covering:

- Reproducible verification and safe continuation.
- One additional native provider adapter in zor.
- One measured performance opportunity.
- One unattended two-worker workflow demonstrating the new capability.
- Independent review and final verification.

Distinguish implemented behavior, proposed changes, and unverified claims. Do not reopen resolved findings or add speculative acceptance requirements.

## 2. Make verification reproducible

Improve the existing Rust verification runner using the concrete failures recorded in the previous milestone.

Required behavior:

- Isolate child stdin unless a check explicitly supplies input.
- Bound execution and clean up owned processes.
- Persist each check’s command, working directory, relevant non-secret configuration, toolchain identity, input fingerprints, result, and diagnostic-log location.
- Distinguish passed, failed, interrupted, and not-run checks.
- Permit continuation only when recorded inputs still match. Missing, failed, or mismatched evidence cannot count as passing.
- Record the exact mandatory command plan and explicit R6 exclusions.
- Retain the final verification manifest in a durable location, rather than relying exclusively on `/tmp`.

Keep this implementation small and specific to the existing gate. Do not build a general workflow engine or speculative dependency graph. If evidence validity is uncertain, rerun the affected check.

Test the actual runner’s stdin isolation, failure propagation, interruption handling, evidence invalidation, and owned cleanup. Do not simulate a separate implementation of its behavior.

## 3. Extend one native adapter in zor

Inspect the installed Claude and Codex interfaces and the existing OpenCode adapter. Select **one** additional provider based on demonstrated support for native message identity and turn control. Record the selection and its evidence before implementation.

Implement a narrow, provider-specific adapter in zor that supports:

- Headless launch and discovery.
- Correlation between an accepted input operation and native session/message identity.
- Structured working, input-required, and response evidence.
- Native interruption and session recreation where the inspected interface and implementation support them.
- Explicit capability errors and explanations for unsupported operations.

Preserve stable operation IDs, literal input, strict identity checks, original deadlines, and explicit retry/uncertainty semantics. Never replay input merely because a response was lost.

Keep these operations distinct:

- Cancelling coordination.
- Interrupting a native turn.
- Stopping an owned worker.
- Verifying task completion.

Native responses and idle state must never imply verified task success.

Exercise production adapter logic with deterministic fixtures covering stale responses, changed session identity, duplicate events, partial frames, missing acknowledgements, caller loss, event loss/resynchronization, and cleanup where relevant to the implemented interface.

Use bounded real-provider validation where practical and authorized. If it cannot run, document the limitation and label the adapter’s evidence accurately. Do not claim live integration support from synthetic fixtures alone.

## 4. Measure and optimize one demonstrated cost

Use the Rust performance tooling to establish a reproducible current baseline. Preserve both baseline and candidate binaries, source fingerprints, toolchain, build flags, workload configuration, and raw results.

Investigate:

- **fux:** pane-view construction, capture allocation, and per-viewer serialization.
- **zor:** journal lock duration, decoding/serialization, observation processing, and dashboard composition.
- **koh:** copying, buffering, and acknowledgement behavior in the existing deterministic production harness.

Use unprivileged instrumentation. Add only the counters or timing boundaries necessary to identify the dominant cost; avoid instrumentation that changes normal product behavior.

Select one demonstrated opportunity. Prefer eliminating repeated work, sharing immutable data, or coalescing before introducing new storage formats, wire protocols, or broad rewrites.

Compare before and after with identical workloads and settings, including single-viewer, multiple-viewer, and slow-consumer cases relevant to the change. Retain regressions as well as improvements. Report instrumentation overhead and unavailable measurements.

Accept the optimization only when evidence supports its benefit and correctness. A documented decision to reject an optimization is valid if the measurements do not justify it.

Preserve ordering, input receipts, freshness, and cleanup. Document bounds and limit behavior for any affected buffer, queue, cache, or worker.

## 5. Demonstrate one unattended workflow

Extend the existing two-worker workflow to exercise the new adapter capability through CLI/API:

1. Launch two workers with stable identities.
2. Submit work and retain correlation evidence.
3. Encounter a blocker or interrupt a turn.
4. Reconcile safely without duplicate input.
5. Continue or recreate the native session where supported.
6. Run checks, collect artifacts, and verify outputs.
7. Produce a retained handoff containing evidence and unresolved limitations.

Keep the mandatory workflow runnable with deterministic local fixtures and no account. Supplement it with a separately attributed real-provider run when practical.

Do not add a new UI or infer success from agent state.

## 6. Review, verify, and stop

Batch related fixes and run targeted regressions during implementation. Preserve all existing mandatory non-R6 checks.

Have independent reviewers inspect the complete intended diff and final fixes for:

- Component ownership violations.
- Incorrect correlation or unsafe retries.
- Stale evidence and misleading capability claims.
- Resource bounds and process/socket cleanup.
- Performance measurement validity.
- Verification-runner evidence reuse and failure handling.
- Missing meaningful regression coverage.

Validate each finding against current code. Fix confirmed in-scope defects and document justified rejections.

Refresh companion patches and only evidence invalidated by actual changes. Never relabel historical measurements or regenerate historical hashes to make checks pass.

On settled final source, run the mandatory headless gate from a fresh verification manifest. Obtain one successful end-to-end invocation; if it exposes a defect, fix it, rerun affected checks, and perform the final invocation after the source settles. During development, use the reviewed continuation mechanism to avoid unnecessary repetition.

Update the checklist and handoff with exact verification results, review dispositions, measured tradeoffs, provider-validation scope, unsupported capabilities, and the brief deferred R6 record.

Stop when:

- Verification is reproducible and its continuation behavior is tested.
- One additional native adapter is implemented with accurately attributed evidence.
- One performance opportunity is measured and its optimization accepted or rejected with evidence.
- The unattended workflow passes.
- Complete independent review and the final mandatory non-R6 gate pass.

Do not expand into a plugin framework, new UI, general terminal restoration, broad multiplexer parity, or remote runtime acceptance. Report genuine blockers honestly rather than declaring incomplete work finished.
