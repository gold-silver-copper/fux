# Two-worker verification and cleanup

Both live branches passed in disposable repositories with the same baseline and
worker outputs: alpha returns `result-alpha`, beta first returns `incorrect-beta`,
then repairs it to `result-beta`. Both workers exist before results are collected.
Workers are scripted processes, not model agents; this measures workflow contracts.
No account credentials or external repositories are used.

| Outcome | fux + zor | herdr 0.8.2 reference |
|---|---|---|
| Two isolated worktrees and worker processes | Passed | Passed |
| Fresh response evidence | Prompt-scoped fixture reports tied to input receipts | Harness observes response markers through pane reads |
| Reject incorrect beta output | Required check fails inside zor | External harness comparison fails |
| Collect repaired output | Stored, source-bound artifact | External harness reads worktree file |
| Gate handoff on both verified predecessors | Built-in policy rejects early/failed handoff; succeeds after repair | No matching built-in policy in inspected default API; external harness decides when to send |
| Deliver handoff | Sealed references, operation retry and receipt checks pass | Externally assembled text reaches the lead pane |
| Normal dirty-worktree removal | Refused | Refused with `dirty_worktree_requires_force` |
| Explicit forced cleanup | Owned trees removed; main preserved | Fixture trees removed; main preserved |
| Evidence after worktree removal | Zor's verified artifacts remain inspectable | External harness retains copied strings; not a herdr artifact store |

This establishes a narrow advantage in built-in verified handoff and retained
artifacts, alongside parity on the exercised worktree creation and dirty-removal
behavior. It does not claim herdr cannot support equivalent policies through
external scripts or plugins. Nor does it claim identical prompt-correlation strength:
herdr's fresh marker observation here is a harness observation, not a prompt receipt.

## Evidence and boundaries

`fux-xtask capture-workflow` first runs the existing real
`tests/verify/zor_workflow.py` acceptance fixture and retains its successful output,
exit code and source hash. That fixture asserts early handoff rejection without
journal mutation, required checks/artifacts, beta repair, CLI/API handoff identity,
sealed predecessor generations, receipt reuse, interrupted stop-intent recovery,
dirty-tree refusal and artifact inspection after cleanup. Its raw CLI traffic is
not copied into this report; the pinned executable fixture and pass result are the
evidence. Other crash, cancellation and missing-artifact coverage remains in the
owning launch/check fixtures and the completion checklist; this comparison does not
rerun every prior failure case.

The herdr branch uses the unchanged reference binary and real worktree/pane API.
It retains every request/reply, external comparison values, the lead's captured
handoff, three worker PIDs' confirmed disappearance, and normal server exit. The
worker performs the same write/add/commit operation as the zor fixture. Both trees
are made dirty before normal removal is attempted, and preserved files are checked
before explicit force. The main checkout must retain only its baseline output.
The lead's handoff is assembled and authorized by the harness after external checks;
it is deliberately not labeled a herdr verification result.

Source inspection of `references/herdr/src/api/schema.rs` and its schema modules
found no native required-check, retained-artifact, or verified-predecessor workflow
API. The live `task.verify` request returns `invalid_request` with `unknown variant`
and the supported method list. That response alone establishes only the unavailable
method; the source inspection supports the broader default-API comparison.
`server.live_handoff` concerns server replacement, not verified task predecessors.
Installed third-party plugins are outside this isolated default-config scenario.

## Reproduction

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- capture-workflow \
  --fux target/debug/fux --zor zor/target/debug/zor \
  --herdr target/herdr-reference/build/debug/herdr \
  --herdr-provenance target/herdr-reference/provenance.json \
  --output /tmp/workflow.json
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- verify-workflow /tmp/workflow.json
```

Use a new output path. `workflow.json` contains binary/source hashes and clean
reference-build provenance. Both live branches and four mandatory offline evidence
checks passed. The checks reject omitted failed checks/repair, unrelated errors
masquerading as unsupported-method evidence, and unproven cleanup. The Rust capture passed strict lint and its fresh report passed the same evidence contract.
Historical measurements remain unchanged.


The subsequent [live freshness report](detection-freshness.md) supplies the bounded
real Codex blocker/loss measurement. R5 measurement categories are now retained;
broader real-agent coverage remains R4, with R6/R7 also open.

The capture above remains historical. Its original workflow acceptance and capture sources
are retained as non-executable text under `tools/archive/`, with their recorded hashes
unchanged. Current acceptance runs in `tools/xtask/src/scenarios/zor_workflow.rs`; the current
comparison capture caller invokes that Rust scenario. Offline integrity checks now run via
`cargo run --manifest-path tools/xtask/Cargo.toml --locked -- verify-workflow`.
