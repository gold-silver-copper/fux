# Build on main and selectively port native capabilities

Build forward from current fux main. Treat main as the architectural baseline and the native-agent-milestone branch as a reference library of capabilities, fixes and regression tests. This is a main-based implementation task, not a reconciliation of two equally authoritative implementations. Complete implementation and verification in small, tested increments.

Starting fresh means a clean branch from main, not rewriting the project or discarding proven native work. Keep main's implementation whenever it already satisfies the required behavior. Adapt milestone code only where it adds a missing capability or fixes a demonstrated defect. Reimplement a capability in main's architecture when adapting the old code would introduce competing abstractions or undo main's improvements.

The architectural decision is settled: use main as the foundation. The remaining question for each native capability is whether its user-visible behavior is still needed and missing from main, not whether its old implementation can be merged. Do not assume main is more reliable in every respect merely because its architecture is preferred; establish that with tests and measurements. Preserve valuable native fixes and regression coverage without preserving redundant machinery.

Do not attempt a branch-wide reconciliation. Evaluate each proposed addition against main independently: identify a required behavior that main lacks or a reproducible defect, choose a focused implementation consistent with main, and establish regression coverage. When main already satisfies the requirement, retain main and the useful test coverage. When a milestone implementation is redundant, leave it behind and record why. The goal is a coherent main-based result, not maximum reuse of milestone code.

If an integration checkout already exists, inspect its base, changes and verification evidence before continuing. Reuse sound work that follows this direction, and revise or remove changes that do not. Do not discard existing work or create another fresh implementation solely because this prompt says to start from main.

Work autonomously through implementation, tests, independent review and fixes. Do not stop after a plan or a partial port. Backwards compatibility and semver breaks are not concerns.

Do not commit, push, create a PR, or modify GitHub during this task. Read-only GitHub inspection is authorized. Preserve existing user changes and both published histories.

Starting references, to verify before work:
- Common ancestor: 544961626d7572ac1b7f129801333b855e8fb301
- main: a48f839501af2bb317f559d96255013bd3f3eb33
- native-agent-milestone: 01e52cc
- zor main: 2a8769ede679211f81624823247c8494f046d869
- koh main: af776a39ddea8826fe0915e712c787e303d5dbf0

Read AGENTS.md and the relevant architecture, boundary, milestone and verification documents. Fetch current remote references and inspect any changes beyond the references above. If no suitable integration checkout exists, create a fresh local integration branch in an isolated checkout based on current main. Otherwise inspect and reuse the existing integration checkout as directed above; record its actual main base and assess subsequent main changes explicitly. Establish or verify retained evidence for that main baseline before further implementation. Keep the native milestone available as an immutable reference and backup. Leave all implementation changes uncommitted for review.

Do not merge the milestone branch wholesale, cherry-pick its aggregate commit, or replace main’s runtime files with their milestone versions. There is no requirement to merge the milestone’s ancestry or preserve every implementation choice. Equally, do not rewrite proven zor functionality or generic reliability features from scratch when their implementation and tests can be adapted.

Inventory both change sets by capability before porting. For each relevant capability, record the user-visible need, main's current behavior, the milestone's added value, the implementation decision and the regression coverage that proves it. Choose among keeping main unchanged, adapting a focused milestone change, implementing the missing behavior using main's abstractions, or dropping a redundant implementation with a documented rationale. Review branch-exclusive changes as well as overlapping paths. File counts, textual conflict counts and preserving old code are not acceptance criteria.

Use this decision rule for every candidate change:
- If main already provides the required behavior, keep its implementation and add only missing regression coverage.
- If a native fix is small, independently useful and fits main, adapt that fix.
- If a required native capability depends on superseded machinery, implement the behavior directly using main's existing abstractions.
- If a native feature is outside the requirements below, obsolete or duplicative, leave it out and record the reason. Its presence on the milestone branch alone does not justify porting it.

Do not make restoration of every historical report, helper, prompt or benchmark a prerequisite for a working integration. Preserve the original evidence and its provenance where relevant, retain required regression and measurement coverage, and document unavailable historical inputs honestly. Generate current evidence for the selected implementation rather than recreating obsolete infrastructure solely to make historical checks pass.

Use the reliability guarantees below as behavioral requirements, not instructions to transplant their original machinery. If main already provides a guarantee, prove it with existing or adapted tests and move on. Do not add a parallel runtime, duplicate state model, compatibility layer or generic abstraction solely to accommodate milestone code. Prefer the smallest coherent change that satisfies the requirement. Keep useful tests even when the implementation they originally exercised is discarded.

Execution order

1. Establish main’s test baseline, inspect the known native-branch failures below, and retain comparison builds before edits.
2. Apply the fux/zor ownership boundary to main and bring over the boundary checks with a reviewed inventory of main’s generic declarations.
3. Fill demonstrated generic reliability gaps in dependency order: identity and coherent capture contracts; tracked input and cleanup; event replay; final records and fux run integration. For each increment, establish what main already guarantees, adapt or implement only the missing behavior, and test it before expanding scope.
4. Adapt the existing standalone zor implementation and koh wire consumers to the resulting fux contracts. Preserve zor’s native-provider, durable task, recovery, check and artifact behavior.
5. Bring over the Rust verification gate and relevant scenarios, incorporating main’s additional regressions and replacing its active Python tooling without losing coverage.
6. Consolidate documentation and historical evidence, measure the integrated implementation, and complete the fresh gate and independent review.

Adjust this order where concrete dependencies require it, but keep increments separately explainable and tested. Do not expand the task into a redesign of capabilities that already work.

Target architecture

Use main’s multiplexer implementation as the foundation:
- Typed ECS systems, workspace/tab relationships and shared helpers.
- Viewer lifecycle and creation-order fixes.
- Retained terminal grids, changed-row attachment frames and frame pacing.
- Reusable terminal parsing buffers, improved PTY reads and bounded frame allocation.
- Generic info/wait commands, environment and initial-size options, key notation, row capture and fux run.

Ensure the resulting main-based implementation provides these generic reliability guarantees, reusing milestone work where it fits:
- Server-incarnation and workspace-lifetime identity.
- Input reservation/submission/status receipts and intervening-writer detection.
- Bounded queued input, partial-write accounting and explicit uncertain outcomes.
- Bounded event replay, stream identity and explicit gaps.
- Retained final capture and exit evidence after pane/workspace retirement.
- Coherent conditional captures, truncation reporting and metadata.
- Split-UTF-8 parsing fixes and shutdown/process cleanup fixes.
- Native zor integration, unattended workflows and the durable Rust verification gate.

Preserve the ownership boundary:
- fux owns terminal processes, panes, rendering and generic control.
- zor owns agent interpretation, provider integration, tasks, worktrees, checks, results, recovery and dashboard policy.
- koh owns transport/authentication.

Main’s OSC 7877 parsing, AgentReport, pane agent state and pane.agent events conflict with that boundary. Remove them from fux or relocate needed behavior to zor. Preserve generic terminal title/progress behavior. Do not weaken the boundary checks or merely regenerate their fixture to accept agent policy inside fux.

Key porting requirements

1. ECS and lifecycle
Port receipt, event-log and final-record behavior into main’s architecture. Preserve ordering guarantees around output, input completion, spawn completion, waits, retirement and final publication. Audit same-step arrival/departure and delayed spawn/exit cases.

2. Rendering and performance
Keep one rendering architecture based on main’s retained grids and delta frames. The milestone’s shared full-pane snapshots need not survive literally if their purpose is superseded. Preserve validation before allocation, total cell budgets, slow-viewer bounds, final updates after throttling and interactive responsiveness.

3. Identity and sequence semantics
Do not blindly unify:
- Grid sequence: refreshed observable grid changes.
- Terminal revision: conditional capture invalidation.
- Input sequence: intervening writers.
- Event cursor: stream identity and replay position.

Define their lifetimes and invalidation rules explicitly. Captures must remain correct across output, resize, history changes, metadata updates, retirement and server replacement. Never treat PTY delivery, a native turn outcome or process exit as verified task success.

4. Wire contracts
Choose one coherent control/attachment contract. Prefer main’s simplified unversioned negotiation and delta-frame representation, incorporating the milestone’s identity, receipt and replay guarantees. No compatibility layer is required.

Update all consumers together: fux CLI/viewers, zor, koh integration fixtures, Rust scenarios and shared protocol fixtures. Preserve strict parsing, bounds, deadlines and stale-identity rejection. Protocol simplification must not remove server-incarnation protection.

5. fux run
Retain it as a generic command. Replace reliance on opportunistic background row sampling for final output with authoritative retained final records where appropriate. Verify immediate exit, final bytes, nonzero exit, timeout, lost event connection and cleanup. Never destroy a pre-existing workspace.

6. Companion repositories
Keep zor and koh as separate owning repositories. Inspect main’s companion patches and determine which behavior is already present, superseded or still needs porting.

Because commits/pushes are not authorized here, express any new companion changes using the repository’s pinned-base patch workflow. Keep manifest bases, patches and CI checkout refs consistent. Verify exact reconstruction against the owning working trees. Do not publish unreleased commit references.

7. Tooling and evidence
Port the Rust xtask runner, durable manifests, bounded processes, validated continuation and explicit exclusions. Preserve main’s added test assertions and benchmark scenarios by integrating them into the Rust tooling. Retire superseded Python implementations only after their relevant coverage is preserved.

Keep historical measurements and logs attributed to their exact original sources/binaries. If integration invalidates active provenance checks, archive the original evidence and generate the needed current evidence. Do not replace historical hashes with current hashes or claim that old passing gates validate the integrated tree. Do not copy all milestone reports and prompts into the active documentation merely because they exist; retain what supports the port, its historical evidence and current acceptance requirements.

8. Documentation
Consolidate current architecture, protocol, ownership and readiness guidance. Preserve useful historical evidence and main’s removal of obsolete documentation where compatible with retained references. Record the integrated contracts and any deliberately superseded implementation.

Known baseline failures

Inspect the native branch’s hosted CI, including run 34258670819 and any later runs. Use the native checkout as a diagnostic reference; fixes belong in the new integration checkout, without requiring repair or publication of the historical branch first.

- The native branch’s CI pins koh at 50a8270... and zor at fb6a1ef..., while its published dependency manifest pins af776a3... and 2a8769e.... The dependency assembly job fails. When adopting the companion commits, update the integration checkout’s manifest and duplicated CI pins together and establish a check that prevents this mismatch.
- Linux input_receipts_survive_reconnect_and_bound_a_stalled_pty failed with an owned-child wait timeout. Diagnose whether this is runtime cleanup, harness behavior or another cause as part of porting tracked input and cleanup. Do not import a known failure unquestioningly, dismiss it as flaky or simply increase the timeout without evidence.

Distinguish main’s baseline failures, known milestone failures and regressions introduced by the port. Fix confirmed in-scope defects; do not broaden into unrelated repairs.

Verification and review

Start with targeted tests for each integration area, then run all required formatting, strict linting, tests, documentation/package checks and companion reconstruction.

Explicitly cover:
- Interrupted/partial input delivery, duplicate submission and stalled PTY shutdown.
- Stale server/workspace identities and intervening writers.
- Event reconnect, replay eviction, gaps and delayed events.
- Immediate process exit, final output retention and cleanup.
- Capture consistency across resize, history, metadata and cache invalidation.
- Delta-frame reconstruction, hostile frame bounds and slow viewers.
- Generic waits, deadlines and connection loss.
- The native two-worker zor workflow and verified artifact handoff.
- The multiplexer ownership boundary.

Preserve both branches’ regression intent for retained capabilities even where APIs and implementation change. Tests for deliberately superseded behavior should be replaced by tests of the chosen main-based contract, with the decision recorded; they should not force obsolete behavior back into the product.

Retain comparison builds and run paired measurements of the integrated implementation against main and the relevant milestone baselines. Include sustained output, multiple viewers, slow consumers, interactive input and headless capture/observation. Report regressions honestly; do not assume combining optimizations preserves their benefits.

Use independent subagents for bounded review scopes. Review the complete intended root diff against the chosen main base and companion diffs against their pinned bases. Check the capability inventory for omissions as well as inspecting the ported code. Validate findings against current code, fix confirmed in-scope defects, rerun affected checks and obtain a final independent review.

Finish with one fresh complete mandatory headless gate on settled source. Historical gates or resumed runs do not replace this final invocation.

R6 live remote authorization, reconnect, retention-window and netmon acceptance remain deferred. Do not turn deterministic transport coverage into a remote-runtime claim. Do not make paid model calls or claim live-provider turn/resume validation without separately authorized execution.

Completion and handoff

Leave a concrete, reviewable integration result locally, without committing or publishing it. Report:
- Exact compared revisions and integration checkout.
- Capability inventory: behaviors already supplied by main, ported, relocated or superseded, with rationale and regression coverage.
- Root and companion changes and reconstruction results.
- Independent review scope and finding dispositions.
- Verification commands/results and performance comparisons.
- Baseline CI failures fixed or still unresolved.
- Remaining risks and exact blockers.

Since GitHub mutations are not authorized, new hosted CI execution is a subsequent publication step; clearly distinguish local verification from hosted evidence. If Linux execution or another required check is unavailable, report that limitation rather than claiming full completion.
