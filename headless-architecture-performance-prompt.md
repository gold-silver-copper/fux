Improve the architecture, performance and headless capabilities of fux + zor + koh in the current workspace.

Execute the work through a finite, evidence-backed milestone. Inspect the current implementation first, preserve existing user changes, and reuse unchanged valid evidence. Do not commit, push or open PRs.

Architecture boundaries are fixed:

- fux owns PTYs, terminal state, panes/layouts, rendering and generic input/output/lifecycle APIs. It must not contain agent detection, task state, provider integrations, Git policy or verification policy.
- zor owns all agent interpretation, adapters, orchestration, worktrees, checks, artifacts, recovery and presentation. It accesses fux through its public API.
- koh owns authentication and opaque byte transport. It must not interpret panes, prompts, tasks or application outcomes.
- Each component must remain independently buildable and testable. Introduce abstractions only at concrete boundaries that need independent implementation or testing.

Everything required for this milestone must run headlessly with ordinary workspace access. No sudo, system permission changes, interactive approval dialogs, privileged monitoring, external accounts or paid model calls may be required. Existing UIs may remain optional API clients.

Optional paid model calls are authorized when useful for real integration validation, using the user's existing Codex account or exported API keys. Keep calls bounded and purposeful, protect credentials from logs and retained artifacts, and record the provider/model and validation scope. Required headless checks must remain runnable without accounts or paid calls.

R6 remote runtime acceptance remains explicitly deferred. Do not pursue live remote authorization, network reconnect, retention-window acceptance or netmon work. Deterministic transport tests may validate production logic, but must not be presented as proof of real remote behavior.

1. Establish the finite plan

Inspect current code, existing tests, retained benchmarks and completion records.

Create one concise checklist covering:
- Confirmed architectural weaknesses.
- Measured performance opportunities.
- Missing headless agent capabilities.
- Concrete acceptance criteria and stopping conditions.

Distinguish implemented behavior, proposed changes and unverified claims. Do not revive resolved findings or add speculative requirements.

2. Separate deterministic logic from OS adapters

Preserve fux’s separation between ECS decisions and OS effects.

Give zor narrow boundaries for public multiplexer operations, subprocess execution and time where these are currently inseparable from coordination logic.

Separate koh’s framing/session logic from endpoint creation only as needed for independent testing. Reuse existing generic I/O support.

Exercise the production logic through bounded in-memory streams, controlled clocks and explicit fault injection. Cover relevant partial writes, backpressure, cancellation, stale identities, interrupted operations and cleanup.

Do not build a parallel simulator that reimplements production behavior. Controlled-clock tests must be labeled as deterministic logic coverage.

Provide one documented mandatory headless verification entry point. Keep every non-R6 check enabled. Clearly report deferred R6 coverage without silently skipping unrelated tests.

3. Measure and improve performance

Establish reproducible headless baselines with equal terminal geometry and pinned build settings.

Cover:
- Idle operation.
- Sustained and burst output.
- Multiple panes and viewers.
- Slow consumers and backpressure.
- Zor observation and journal activity.

Use unprivileged instrumentation. Measure relevant CPU time, latency, allocations or copied/serialized bytes, journal writes, and buffer high-water marks. Do not require privileged profiling; document unavailable measurements honestly.

Investigate these existing cost centers:
- fux: repeated snapshot construction, capture allocation and serialization across viewers.
- zor: repeated observation processing, dashboard composition, journal loading/writing and lock hold time.
- koh: copying, buffering and acknowledgement behavior in the deterministic transport harness.

Implement improvements supported by those measurements. Prefer eliminating repeated work, shared immutable data and coalescing before adding wire deltas, new storage formats or broad rewrites.

For affected queues, buffers and workers, specify bounds in bytes, item counts, concurrency and time as appropriate. Define what happens at each limit. Preserve ordering, input receipt semantics, freshness and cleanup guarantees.

Retain before/after results and explain tradeoffs. Do not invent improvement percentages, cherry-pick runs or claim a universal performance winner.

4. Deepen zor’s headless agent capabilities

Define a small explicit adapter capability contract covering:
- Launch and discovery.
- Correlation between submitted input and native messages.
- Working, input-required and response evidence.
- Interruption/cancellation where supported.
- Native-session recreation where supported.
- Explicit explanations for unavailable capabilities.

Build on the existing OpenCode implementation. Inspect the available Claude and Codex integration surfaces, then implement supported capabilities incrementally.

Keep all provider-specific behavior in zor. Use native structured evidence where available and label passive detection as fallback. Never infer verified task completion from agent state.

Every in-scope workflow must be accessible through CLI/API with:
- Stable operation identities.
- Structured errors and results.
- Read-only inspection.
- Explicit retry and uncertainty semantics.
- Bounded event delivery with documented loss/resynchronization behavior.

Use deterministic adapter fixtures and real headless executables without permission changes. Supplement them with authorized paid model calls where useful; do not make those calls a prerequisite for the mandatory headless gate. Distinguish fixture conformance from real provider validation. Do not claim live integration support based only on synthetic fixtures.

Demonstrate an unattended two-worker workflow that launches workers, submits work, encounters a blocker or interruption, reconciles safely, verifies outputs and produces a retained handoff. Unsupported operations must produce explicit capability errors.

5. Review and finish

Run targeted regressions while implementing. Batch related fixes before broader verification.

Refresh companion patches and evidence invalidated by actual changes. Do not regenerate historical hashes without rerunning the corresponding scenario. Reuse unaffected valid evidence.

Have independent reviewers inspect the complete intended diff and final fixes for ownership violations, correctness, regressions, resource bounds, unsafe retry behavior and missing meaningful tests. Validate findings before acting. Fix confirmed in-scope defects and document justified rejections.

Run the mandatory non-R6 headless gate once on settled final source. Repeat affected checks only after relevant changes or failures. Recover interrupted check handles instead of restarting merely because observation was interrupted.

Update the checklist and provide:
- What changed in each component and why.
- Boundary invariants and how they are enforced.
- Measured performance changes and limitations.
- Adapter capability coverage and unsupported operations.
- Exact verification and independent-review results.
- Remaining genuine blockers and the brief deferred R6 record.

Stop when the finite milestone’s acceptance criteria are satisfied. Do not add a plugin framework, new UI, general terminal restoration, broad terminal-feature parity or live remote acceptance to this milestone. Do not claim completion if an in-scope outcome remains unverified.
