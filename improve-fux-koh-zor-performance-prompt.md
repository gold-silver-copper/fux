# Improve performance across fux, koh and zor

Improve the performance of fux, koh and zor as one working system. Diagnose measured bottlenecks, implement focused improvements in the appropriate owning repositories, and prove the result with repeatable measurements and behavioral verification. Do not stop at recommendations or profiling alone. Backwards compatibility and semver breaks are not concerns.

Work from the existing main-based integration in `/Users/kisaczka/Desktop/code/fux-integrated`. Inspect its current state before changing anything. Preserve all existing user work, including uncommitted and untracked integration changes. Do not restart from main, merge the native branch wholesale, or discard the integrated reliability guarantees to obtain better numbers. The checkout at `/Users/kisaczka/Desktop/code/fux` is the historical reference and contains this prompt; keep its runtime unchanged.

Do not commit, push, create a PR, or mutate GitHub. Read-only inspection is authorized. Leave a concrete, verified implementation and performance report locally. Do not make paid model calls or run live remote/provider acceptance without separate authorization.

## Establish the actual starting point

Read applicable AGENTS.md instructions, current architecture and ownership documents, `docs/native-integration.md`, `.verification/FINAL-HANDOFF.md`, and the existing benchmark tooling. Verify paths, revisions and working-tree changes rather than assuming the following recorded references are still current:

- Integrated fux base: `a48f839501af2bb317f559d96255013bd3f3eb33`, branch `integrate/native-on-main`.
- Zor owning repository: `zor`, pinned base `2a8769ede679211f81624823247c8494f046d869`.
- Koh owning repository: `references/koh`, pinned base `af776a39ddea8826fe0915e712c787e303d5dbf0`.
- Historical native reference: `01e52cc2f562e23157b0f6aa8585a2c64f569169`.

Create a reproducible snapshot of the complete pre-optimization integration, including companion patches, untracked sources and build inputs. This is the primary before/after baseline: comparing only against main would hide the cost or benefit of this task. Retain main as a secondary architectural/performance reference. Use historical native comparisons only when they answer a concrete question.

The previous integration passed a fresh 45-command local headless gate. That evidence validates its recorded source, not subsequent edits. Existing paired measurements are debug-build results, not release-performance conclusions:

- Focused real-viewer server CPU: main 1.765 versus integration 1.985 seconds per 1,000 keys, approximately +12.5%.
- Viewer CPU: 3.92 versus 4.33 seconds per 1,000 keys, approximately +10.5%.
- Eight-viewer p95 latency: 1.44 versus 1.52 ms.
- Integration improved several native comparisons but lost some burst comparisons.

Inspect `.verification/paired-main-integrated/` and `.verification/native-baseline/` for raw evidence and provenance. These results do not establish a cause. The historical memory payload emitted literal escape/newline text; it does not measure real styled scrollback. Preserve historical evidence and introduce a separately named, validated workload for actual styled history.

## Measure the whole system

Build comparable optimized release binaries for the baseline and candidate with identical toolchains, features and build settings. Retain symbols suitable for profiling. If a profiling build differs from the measured release build, record that distinction and confirm gains using the release configuration. Debug results may assist diagnosis but must not support release claims.

Use existing Rust benchmark tooling where possible. Extend it only for missing measurements. Validate workload bytes, frame reconstruction, operation counts and successful outputs so a broken or incomplete workload cannot appear faster.

Cover representative workloads across all three tools:

- Interactive typing with a real viewer, including the workload that showed higher CPU.
- Sustained output and bursts, plain and styled text, wide Unicode, split UTF-8, resize and substantial scrollback.
- One and multiple panes, one and multiple viewers, mixed sizes and deliberately slow consumers.
- Headless capture and conditional capture, unchanged and changed observations, event replay/reconnect and capture invalidation.
- Zor task launch, observation, checks, journal contention, cancellation, completion and the existing two-worker artifact-handoff workflow.
- Koh's actual supported local gateway/session paths, including attachment, streaming, reconnect and cleanup where deterministic local execution supports them. Inspect its implementation before deciding which paths to measure.
- Combined koh/zor/fux operation, idle sessions and increasing concurrency. Include bounded overload and recovery, not just the fastest successful case.

Use deterministic worker/provider fixtures for repeatable local experiments. Keep their CPU separate from product-process CPU and do not claim they reproduce live provider/network performance. If a listed operation is unsupported, document that fact instead of inventing a product feature just to benchmark it.

Measure end-to-end completion time and responsiveness together with per-process and aggregate CPU, idle wakeups, allocations where practical, peak/steady memory, transport bytes, capture/event frequency, queue growth and cleanup. Track p50/p95 latency and throughput as appropriate. Count the manager, server, viewer, koh, zor and relevant helper processes so shifting work across a boundary cannot masquerade as a system improvement.

Keep iteration fast: no single benchmark invocation, test run or measurement campaign may exceed five minutes of wall-clock time, and prefer runs under two minutes. Never launch a batch campaign of 30 minutes or more. Use short workloads (for example 300–500 keystrokes, one or two configurations) and a few repetitions per iteration; scale up only briefly and only for a final confirmation of an already selected change. Kill any run that overruns its budget and report it.

Use paired runs with alternating or randomized order, controlled warmup and enough repetitions to distinguish gains from noise. Record machine, OS, architecture, toolchain, build flags, exact source/binary hashes, workload parameters and raw samples. Separate cold startup from steady state. Start with a small diagnostic matrix, then confirm selected changes on the broader matrix. Avoid an unbounded benchmark campaign.

## Profile before choosing fixes

Profile representative release workloads and attribute costs to processes and call paths. Use available platform tools for sampled stacks, allocation behavior, syscalls, wakeups and lock contention. If a profiler is unavailable, use bounded instrumentation or controlled experiments and state the limits of the resulting evidence.

Investigate these existing hypotheses without treating them as established causes:

- Per-input replay events and `EventLog::push` serializing JSON into a temporary allocation solely to calculate the encoded byte budget.
- Additional output scanning in `ServerTerminal::process` for incomplete UTF-8 handling.
- Nonblocking PTY read/poll behavior required by cancellable writes.
- Repeated captures, parsing, serialization, reconnect work, journal contention, polling and duplicate observation work across koh and zor.
- Viewer-side parsing, rendering, copying and wakeups. The server event hypothesis alone does not explain the measured viewer CPU increase.

For each selected optimization, record the measured hot path, proposed mechanism, expected metric and correctness risks. Prefer changes that remove demonstrated redundant work or allocation. Do not introduce caches, batching, background threads, generic frameworks or parallel state models without evidence that their complexity is justified.

Change one coherent area at a time. Run targeted behavior checks and paired measurements after each change. Retain demonstrated improvements; revise or revert speculative changes that do not help. It is acceptable for one repository to need no runtime changes if measurement shows its implementation is already appropriate. Evaluate all three; do not manufacture edits to satisfy a repository count.

## Preserve behavior and ownership

Keep fux a generic terminal multiplexer, zor the owner of agent/task policy, and koh within its documented gateway/session responsibilities. Preserve main's typed ECS, retained grids, delta frames and pacing unless a measured, reviewed replacement is clearly better. Do not move agent-specific policy into fux or duplicate authoritative state across repositories.

Optimizations must preserve:

- Incarnation identity and stale-instance rejection.
- Coherent capture revisions, independent grid sequencing and correct invalidation for output, history, metadata and resize.
- Input reservation/submission/status semantics, exact partial-delivery accounting, idempotent duplicate submission, conflicts and intervening-writer detection.
- Bounded replay with correct cursors, gaps, ordering and reconnect behavior, including when no subscriber is currently attached.
- Authoritative final output/exit records, retention bounds and correct natural/forced termination handling.
- Slow-consumer bounds, hostile-frame bounds, backpressure and bounded memory under overload.
- Cancellable stalled PTY writes, reliable shutdown, deadlines and cleanup of owned processes without destroying pre-existing workspaces.
- Unicode correctness, terminal semantics and equivalent final visible output.
- Zor journal/check concurrency, operation identity and artifact-handoff correctness; koh's verified transport/session contracts.

Do not obtain gains by dropping required events, lowering workload volume, shortening required retention, weakening byte limits, increasing timeouts to hide stalls, reducing observability, or silently increasing input/output latency. Batching and caching require explicit bounds and invalidation tests. Preserve the exact encoded-byte bound if changing event size accounting. Do not restore blocking PTY behavior without proving stalled-writer cancellation on Linux.

Keep zor and koh as separate owning repositories. Express companion changes through the existing pinned-base patch workflow, keep manifests and CI reconstruction consistent, and verify that reconstructed sources exactly match the owning working trees. Do not invent unpublished commit references.

## Verification and independent review

Add focused regression tests for behavior affected by optimizations, especially malformed/split UTF-8, event bounds, cache invalidation, concurrency and cancellation. Avoid tests that merely mirror implementation details. Run targeted checks during development, then required formatting, strict linting, tests, documentation/package checks and exact companion reconstruction on settled source.

Use independent subagents for bounded review of the optimization diff, cross-repository contracts and measurement methodology. They should inspect the existing integration context but distinguish this task's changes from pre-existing work. Validate findings against current code, fix confirmed in-scope defects, rerun affected checks, and obtain final independent review after fixes.

Finish with a fresh mandatory headless gate on the settled implementation:

```sh
cargo run --locked --manifest-path tools/xtask/Cargo.toml -- dependencies verify --build --headless
```

Confirm the current repository still uses this command and run any additional required checks. Exercise affected platform-sensitive runtime paths on macOS and Linux where available, particularly PTY I/O, cancellation, wakeups and cleanup. Report unavailable coverage accurately. Do not claim a complete Linux gate from targeted Linux tests. Hosted CI and live R6/provider acceptance remain distinct from local evidence.

## Completion criteria and handoff

Aim to remove the measured integration overhead where profiling supports it and improve total system efficiency on representative combined workloads. Do not impose an arbitrary percentage target or promise parity with main before measurement. Accept changes based on repeatable release gains, preserved correctness and explicit latency/memory/throughput tradeoffs. Investigate material regressions before retaining a change; do not hide them in aggregate averages.

Save a concise performance report with exact commands and evidence paths. Include:

- Starting snapshot, final source identity and changes in each owning repository.
- Proven bottlenecks versus rejected or unresolved hypotheses, including the higher interactive CPU question.
- Before/after release measurements with raw samples, variability, per-process and total costs, plus meaningful main comparisons.
- Why each retained optimization helps, behavioral coverage and any remaining tradeoffs.
- Companion reconstruction, verification results and independent review dispositions.
- Unavailable profiling/platform coverage and remaining bottlenecks, with the next concrete experiment for each significant unresolved question.

Leave all work uncommitted and reviewable. If no safe improvement is demonstrated in an area, report the evidence honestly rather than weakening guarantees or claiming an unmeasured speedup. Complete the feasible implementation and verification; clearly identify any exact blocker that prevents the remaining work.
