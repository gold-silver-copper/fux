# Improve fux and zor architecture, reliability and maintainability

Implement this plan in the current workspace. Read the applicable `AGENTS.md`
instructions, inspect the actual checkout, and establish a fresh baseline before
editing. Historical prompts and verification reports are context, not authority
to perform their release, repository or GitHub actions.

Backward compatibility and breaking semver are not concerns. Preserve unrelated
dirty work. Do not commit, push, open a PR, release, or modify companion repositories
unless separately requested. Do not introduce hypertile as a dependency. Use the
existing Rust harness and Betamax integration.

The objective is to make ownership and lifecycle rules explicit, reduce duplicated
protocol and transition logic, restore reliable integration coverage, and make
future failures easier to reproduce. Complete the implementation and verification;
do not stop after producing another plan or splitting large files mechanically.

## Resume safely from the current checkout

This workspace may already contain partial implementation of this prompt. Read
`docs/codebase-improvement-report.md`, inspect the code and retained evidence, and
classify each requirement as verified, implemented but unverified, or missing.
Resolve contradictory progress entries against current source and actual results.
Do not repeat completed refactors or treat an old passing run as verification of
the current binaries. Finish missing work and run the final integrated gate.

Prioritize correctness and unresolved verification before additional refactoring.
Read `docs/verification/codebase-improvement/completion-audit.md` and
`docs/verification/codebase-improvement/performance-review.md` when present.
Check their claims against retained logs, current source and binary identities.
In particular, investigate any outstanding slow-reader/pressure harness timeout:
identify the failing phase, preserve reader/frame/marker evidence, and distinguish
a harness observation defect from a product stall. A later successful run is
non-reproduction, not proof of a fix. Add a controlled regression for a confirmed
cause, then rerun the affected comparison with a predetermined repetition count.
Keep failed runs visible alongside successful results.

Work in this order: baseline and remaining failures; typed boundaries and ownership;
lifecycle recovery; reusable tests and diagnostics; performance investigation;
final integrated verification and review. If an area already meets its contract,
verify it and move on instead of replacing a working abstraction. Maintain a
requirement-to-evidence checklist throughout execution.

Save the starting dirty diff, relevant untracked source files, revision and source
hashes outside build-output directories before editing. Preserve that snapshot
through cleaning and rebuilding so the final review can distinguish this task's
changes from existing work. A missing historical snapshot must be reported; do not
reconstruct it from memory or silently substitute Git HEAD for a dirty baseline.

## 1. Establish the baseline and architecture contract

Read the current manifests, CI workflow, `tools/xtask/checks.json`, protocol docs,
`crates/zor/TASKS.md`, and these reports:

- `docs/control-flow-transitions.md`
- `docs/control-flow-mode-coverage.md`
- `docs/control-flow-ux-final-verification.md`
- `docs/betamax-harness.md`
- `docs/pane-layout-performance-2026-09-12.md`

Record the starting Git revision, dirty source identity, toolchain and exact
verification commands. Distinguish failures reproduced now from historical ones.
Inspect existing abstractions before creating replacements.

Write a concise architecture contract with these ownership boundaries:

- **fux:** PTYs and process lifecycle, authoritative terminal state and retained
  history, layouts, viewer-local interactions, reliable input receipts, bounded
  events and final evidence.
- **zor:** agent interpretation, task orchestration, retries, recovery policy,
  checks, artifacts, worktrees and workflow decisions.
- **local-ipc:** authenticated, bounded local transport and socket discipline.
  Do not put multiplexer domain policy into this crate.

Do not move agent policy into fux or make zor depend on fux's ECS/viewer internals.
Prefer a narrow abstraction over a new framework. Document any necessary departure
with a concrete conflict in the current implementation.

## 2. Restore reliable integration coverage first

Reproduce the previously failing `zor_group_scheduler`, `zor_groups`,
`zor_recovery` and `zor_service` scenarios against freshly built matching binaries.
Investigate the intermittent `zor_headless` pin-release failure separately.
Do not assume that every failure is a fixture bug or that one successful retry
proves a race fixed.

- Trace each failure to its earliest incorrect request, route, identity or
  lifecycle transition. Distinguish runtime defects from obsolete fixture behavior.
- Complete the in-repository consumers and fixtures affected by manager/workspace
  routing. Use one authoritative contract; remove obsolete alternate paths after
  checking every consumer. Preserve deliberate independent-server coverage.
- Keep immutable process identity separate from its mutable workspace location.
  Lookup, input, cleanup and recovery must follow the exact retained process and
  reject replacement instances, reused numeric IDs and mismatched PIDs.
- Verify creation-pin release, lost replies, retries, workspace moves and recovery
  with controlled barriers or injected failures. Never let cleanup close a moved
  pane's unrelated destination workspace or terminate an adopted process.
- Convert an intermittent failure into a deterministic regression before claiming
  its cause fixed. Use bounded observations instead of sleep-based synchronization.

Do not skip scenarios, weaken identity assertions, retry until green, or enlarge
product deadlines to conceal stalls. Report genuinely unrelated failures precisely;
the named routing and pin-release failures are within this task's scope.

## 3. Introduce a typed fux client boundary for zor

Inspect `crates/zor/src/fux.rs`, `tasks/route.rs` and their callers. Replace repeated
JSON request construction and nested response inspection for routing, pin release,
input receipts and final evidence with typed operations and validated results.

- Separate bounded transport, wire decoding/validation and caller policy.
- Represent operation-specific success, pending, conflict and evidence-unavailable
  outcomes explicitly. Retry policy belongs to the caller; malformed responses
  must not become retryable pending results.
- Make instance/process identity, request correlation and deadlines part of the
  operation contract. Maintain current output bounds and authentication guarantees.
- Share wire declarations only where ownership and packaging justify it. A small
  protocol crate is acceptable if it removes real duplication without pulling in
  terminal, ECS or agent dependencies. Otherwise keep typed consumer DTOs with
  exhaustive producer/consumer contract tests. Explain the choice.
- Migrate all in-scope callers and remove superseded parsing paths. Do not retain
  compatibility aliases or change wire semantics accidentally during extraction.

Tests must cover actual serialized requests and producer responses, malformed and
wrong-operation replies, stale identities, lost replies and timeout behavior. A
protocol change should fail locally at the contract boundary rather than surface
only as a distant process-scenario timeout.

## 4. Refactor fux interaction ownership around explicit transitions

Inspect `client/controller.rs`, `input.rs`, `mod.rs`, `copy.rs`, `popup.rs`,
`effects.rs`, `read_window.rs`, `context.rs`, `drag.rs`, rendering and hints.
Preserve the completed control-flow UX contract and its regressions.

- Extract cohesive owners for pane history, modal interactions and gesture capture.
  Keep ordered effects and bounded history reads in their existing dedicated lanes.
- Replace ambiguous handoffs and scattered cleanup side effects with explicit
  transition results describing event ownership, next interaction, requested
  effects and capture disposition. Use the simplest representation that enforces
  those rules; do not add a generic state-machine framework.
- Keep byte decoding, prefix policy and interaction policy distinct. Reuse parsing
  primitives where safe without losing unfinished-paste ownership or ESC/Alt/CSI/
  SS3 disambiguation. Byte-identical Escape-plus-key and Alt remain ambiguous.
- Define target invalidation and cleanup once for each owner. Ensure render/hint
  code reflects the actual state without mutating authoritative focus.
- Make the core transition logic testable without sockets, wall-clock sleeps or
  terminal processes. Pass explicit events/time where needed.

Required preserved behavior includes independent A → B → A history, viewer
isolation, one Escape to normal input, exact once-only application bytes, prefix
and paste isolation, focus policy, application mouse/Shift routing, auxiliary
button tails, fresh-press recovery, immediate layout edits, drag cancellation,
resize/buffer invalidation, stale reply rejection, fixed deadlines, bounded queues
and finite-set fair read scheduling.

Refactor incrementally with behavioral checks between steps. File length alone
is not a success criterion; demonstrate fewer places that must coordinate to add
or dismiss an interaction.

## 5. Make zor lifecycle transitions explicit and recoverable

Inspect the task model and launch, delivery, recovery, stop, group and worktree
code. Split the model by responsibility where useful, while concentrating lifecycle
rules into small transition APIs used by both normal operation and recovery.

- Encode valid launch/attempt/delivery/cleanup transitions explicitly. Avoid freely
  updating related phase and identity fields across unrelated call sites.
- Preserve managed versus adopted ownership and immutable launch identity versus
  current location. Make destructive operations require validated authority.
- Keep existing journal locking, bounded storage and atomic replacement protections.
  Do not replace durable storage merely to support the refactor.
- Specify each operation's durable intent, external effect, recorded result and
  reconciliation behavior when a crash or lost reply separates those steps.
- Exercise crash/failure injection around those boundaries: restart after intent,
  effect-before-record, duplicate retry, stale server, moved pane and missing or
  expired evidence. Prove no duplicate launch/input and no unauthorized cleanup
  where the underlying primitive supports that guarantee; represent uncertainty
  explicitly where it does not.

Prefer existing operation IDs and receipt mechanisms. Do not infer task completion
from a passing command, disappearance of a process, or unavailable evidence.

## 6. Turn the behavioral matrix into reusable test infrastructure

- Split the large viewer scenario into focused history, modal, gesture, transfer
  and tiny-layout scenarios sharing bounded fixture helpers. Retain an end-to-end
  composition scenario. Keep every existing substantive assertion reachable.
- Add a small independent reference model for ownership/transition invariants.
  Generate bounded sequences of input, resize, target loss, reply, cancellation
  and buffer changes. Include multiple panes/viewers and delayed operations.
- Check no duplicate or misdirected input, no canceled operation resurrection,
  bounded retained state, deterministic dismissal and absence of unintended
  mutations. Do not merely mirror implementation branches in the test model.
- Save seeds and minimized failing traces. Make a trace replayable through a
  focused controller test; use representative traces in real-PTY scenarios.
- Keep behavioral assertions and Betamax visual checks complementary. Verify exact
  raw bytes/request targets as well as synchronized rendering, borders, cursor,
  selection and visible exit instructions.

Provide one documented local command for targeted checks and another for the full
gate. Reuse the repository's tooling rather than creating a competing test runner.

## 7. Add useful diagnostics and trustworthy performance evidence

Add opt-in structured diagnostics for interaction transitions, request IDs,
attachment/process identity, queue pressure, timeouts, cancellation and recovery
decisions. Reuse existing logging infrastructure, keep terminal output clean and
bound any retained diagnostic data. Do not log terminal contents, paste contents,
prompts, credentials or environment values by default.

Make a failing harness scenario retain its seed/trace, source and binary identity,
relevant diagnostics and final terminal checkpoint automatically.

Rerun representative performance workloads with release builds on an adequately
idle host: idle panes, sustained output, many panes/viewers, scroll bursts, resize,
manager operations and zor journal/recovery work. Build before measuring, alternate
baseline/candidate order and retain raw samples. Report latency distributions,
CPU, peak memory, queue pressure and frame bytes where relevant.

Name precisely what each instrument measures. Distinguish sampled RSS from peak
RSS, decoder backlog from internal queue occupancy, and child CPU from elapsed
CLI time. State polling resolution, sample counts and host-contention limits.
If the original dirty baseline is unavailable, label any later checkpoint
comparison explicitly and limit conclusions to changes between those identities.
Do not describe unmeasured metrics or lost historical comparisons as verified.

Investigate measured regressions before optimizing. The earlier frame-byte increase
is a hypothesis to investigate, not proof that all metadata should be removed.
Do not weaken identity/revision checks, backpressure or coherent rendering for
speed. If host contention prevents a conclusion, report that limitation instead
of claiming a performance improvement.

## 8. Verification, documentation and completion

Start with affected tests, then run the repository's required checks. At minimum:

- Root and standalone-harness formatting and strict all-target Clippy.
- Workspace tests and the supported zor feature combinations used by CI.
- Protocol, boundary, structure, ECS and producer/consumer contract tests affected
  by the refactor. Review semantic changes before updating fixture snapshots.
- Standalone harness tests, then all `local_cli` and `automation_integration`
  scenarios against the exact fresh fux/zor binaries, with required-binary flags
  and no exclusions. Run timing-sensitive scenarios without overlapping builds.
- Betamax capture in a fresh directory, exact replay/report generation and visual
  inspection of labeled normal/tiny-size checkpoints for affected transitions.
- Relevant package checks if crate boundaries or dependencies change.

Keep a single current architecture and verification entry point. Clearly archive
historical execution ledgers without deleting useful failure evidence. Update
bindings, protocol docs and manual acceptance steps when behavior changes.

Perform a separate full-diff review against the recorded starting snapshot,
including relevant untracked files. Review identity, destructive authority,
durability, ordering, boundedness, parser handoffs and missing tests. Use an
independent reviewer if authorized and available; otherwise explicitly record a
separate self-review. Validate findings, fix confirmed in-scope defects, rerun
affected checks and review the final changes again.

Deliver `docs/codebase-improvement-report.md` with:

1. The resulting architecture and concrete duplication/coupling removed.
2. Each baseline failure's cause, fix and deterministic regression evidence.
3. Lifecycle/ownership invariants and their named assertions.
4. Exact verification commands/results, source identity and Betamax evidence paths.
5. Performance measurements, uncertainty and any remaining regressions.
6. Review scope, findings and unresolved limitations.

Do not declare completion while a named in-scope failure remains unresolved, an
ownership/durability regression is known, or required verification has not run.
If an external dependency prevents completion, state the exact blocker and
remaining work. Do not claim universal correctness, native OS-input validation or
cross-product parity from headless tests.
