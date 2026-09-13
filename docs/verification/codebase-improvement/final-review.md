# Separate source review

This was a separate self-review pass; no independent subagent was authorized.
The reviewed implementation is represented by `final-review-scope.json`: 57 changed
Rust/compilation sources, 32 producer/consumer fixture files, two reviewed contract
inventories, and six prompt/document files, including relevant untracked files.
Performance analysis, the final integrated gate and fresh selected-image review
are complete in the appended final verification sections. The historical pressure
timeout remains unresolved and is not approved as fixed by this review.

The comparison uses the retained later checkpoint and its manifest, not the lost
original initial dirty snapshot. Twenty-nine vendor compilation files match their
retained hashes. The checkpoint omitted 454 existing vendor theme/license assets;
the scope record retains current hashes but does not claim they were added by this
task or prove historical equality. No vendor implementation change is attributed
to this task.

## Findings and disposition

- **Fixed: resume pin-release recovery selected the archived launch.** After resume
  attachment archives the old launch, `launch.id` addresses that archive while
  `launch.task_id()` addresses the current launch. The controlled exit-before-pin
  scenario reproduced `final launch pane mismatch`; the corrected lookup passes
  the same scenario and both complete integration runs that include `zor_resume`.
  It preserves archived final/session evidence and unsent prompts, creates no extra
  process on stable retry, and does not invent task success or replay input.
- **Fixed: verification runner omitted explicit fux binary for standalone fixtures.**
  With an external Cargo target, these tests looked in the default directory.
  The runner now supplies `FUX_BIN` beside `ZOR_BIN`. The affected 14 tests and the
  corrected complete 40-check gate pass.
- **Retained limitation: custom diagnostic directories.** Automatic artifact
  collection intentionally names the two standard harness log paths. It does not
  discover arbitrary custom task-store paths. Documentation limits the claim to
  standard roots; no journal/prompt files are swept into artifacts.
- **Rejected as a product regression: ad hoc font setup failure.** The later harness
  test invocation omitted the documented CJK font. Its wide-glyph assertion failed;
  the explicit-font invocation passes. The full gate already sets that font and
  passes. Both logs are retained; no rendering assertion was weakened.

No additional confirmed correctness, authority, durability or boundedness defect
remains from this source pass.

## Scope examined

- Controller async/pointer paths, history ownership and memory budget, modal
  completion ordering, capture tails, unfinished paste, original input target,
  fixed deadlines and finite-set read fairness.
- Typed manager/input DTOs and all migrated callers: exact operation/id/kind,
  immutable identity, explicit pending outcomes, malformed-reply refusal and
  unchanged caller retry policy.
- Launch/resume attachment, attempts, delivery, stop and worktree transitions:
  intent before effect, atomic association changes, monotonic receipt evidence,
  terminal task outcomes, adopted/historical authority, and filesystem/Git checks
  retained under the existing journal lock. Existing group policy remains its
  owner and delegates shared delivery transitions.
- Independent controller model: expected ownership/history derives from its own
  specification, stale replies are invalidated independently, exact rename and
  prefix/paste bytes are checked, state/event bounds are finite, and minimization
  retains the same reported invariant failure. This is a bounded model, not an
  exhaustive state-space proof.
- Failure recorder and terminal/root/RPC integration: bounded event/screens/logs,
  teardown retention, private permissions, nonblocking FIFO/symlink rejection,
  payload omission, and repeated child cleanup. Opt-in diagnostics do not alter
  transaction results or become authority.
- Focused viewer extraction: mouse/transfer/tiny bodies match the retained source
  after renaming; manager body is unchanged and retains its OSC-bound test.
  History keeps the shared gesture assertions and adds stronger render/dismissal
  barriers. Modal checks compare raw/noecho input, pane/PID/labels and layout.
- New benchmark modules: actual rendered offset plus changed rows, exact manager
  identity, real uncertain-launch reconciliation and no duplicate creation;
  confirmed stop cleanup remains outside measured latency. Raw samples and known
  polling/resource scope limits are explicit.
- Contract inventories and fixture changes were reviewed semantically before
  normal tests passed. Producer fixtures round-trip actual fux enums and match
  consumer bytes; raw CLI input-status has an executed consumer assertion.

## Verification following fixes

`full-gate-passed.json` records all 40 checks plus successful Betamax replay and
unchanged before/after source identity. After benchmark-only additions, strict
harness Clippy, 36 library plus 36 binary tests, candidate release workload smoke
checks and Rust 1.95 workspace/all-target checking pass. Product sources were not
changed after that full gate. The final verification and performance disposition
below supersede the earlier pending status; the historical timeout remains open.


Follow-up measurement-only review: `headless_journal::cpu_us` is now shared within
xtask; recovery samples read its reaped-child counter before and after each zor
CLI operation. The live fux server is measured separately and is not included in
that reaped-child delta. Counter reads sit outside reported wall-time intervals.
No lifecycle, wire or cleanup behavior changed. Strict Clippy, 36 library plus 36
binary harness tests and the harness build passed after this addition; focused
baseline/candidate execution supplies the actual measurement check.


Separate review of pressure failure diagnostics: the added stage label and bounded
error string are emitted only after a failed measurement. Reader evidence is
limited to counters and the three fixed synthetic markers already retained by
the workload. The success path, 12-second observation deadline, resource sampling
and cleanup ordering are unchanged. The fixed diagnostic run passes all 20 cases;
this validates execution with diagnostics but does not explain the original
timeout. Strict Clippy/build logs are retained as `pressure-diagnostics-final.log`.
The source-scope hashes have been refreshed. The final Betamax-enabled harness rerun
passed 36 library and 36 binary tests; see `pressure-final-tests.log`.


Separate review of recovery timestamps and control-response assertions:
`launch_proxy` enables bounded timing retention only when requested by the
recovery benchmark. Guards record completion on ordinary and early return,
without extending I/O/cleanup deadlines or retaining payloads. The 72 measured
retries verify two ordered requests and timestamps inside the CLI interval;
analysis rejects incomplete or unexpected traces. The pressure workload now
checks successful split/input replies and retains only typed readiness summary
fields. Strict harness Clippy, 36 library and 36 binary tests pass after these
changes (`pressure-response-checks.log`). All four real pressure cases pass with the stronger response/readiness checks
(`pressure-response-validation.json`).
No product source changed; the historical timeout remains unexplained.


Prepared proxy-readiness change reviewed separately: after nonblocking accept
returns WouldBlock, poll waits for readable listener readiness for at most 10 ms;
EINTR returns to the cancellation/accept loop, and other poll errors retain the
existing proxy failure path. The listener remains owned by its thread, and no
request handling or injected fault is reordered. The same 10 ms maximum idle
cancellation-check interval is preserved. Formatting, strict Clippy, 36 library + 36 binary tests and the harness build
passed after the 200-case immutable-executable campaign completed. The controlled
recovery comparison and final integrated gate remain pending.


## Final integrated verification and visual follow-up

The final full gate passed all 40 checks, exact Betamax replay/report and unchanged
before/after source identity. It includes all 24 automation scenarios and all
18 local CLI scenarios with the readiness-based proxy. Per-command outputs are
retained under `full-gate-final-logs/`; the result is `full-gate-final.json`.
Cargo subsequently confirmed all four measured release binaries fresh and
unchanged, with the final harness matching the readiness comparison manifest
(`release-provenance-final.json`).

Nine fresh checkpoints from the 411-frame final gallery were inspected directly.
A and B retain independent offsets; one Escape restores A to live output while B
remains scrolled, without entering the command menu. Selection highlighting and
its Escape instruction are visible. The four-row popup clips with a more-items
indicator, while the other viewer remains outside the popup. The two-column
keyboard/zoom states remain bounded. Manager failure is a notice with no command
menu. No new visual defect was confirmed. This supplements the prior 20-image
review and does not claim exhaustive/native visual coverage.

The fixed 200-case campaign did not reproduce the historical pressure timeout.
No cause or fix is established for that failure; this source review and the
passing integrated gate do not close that outstanding requirement.
