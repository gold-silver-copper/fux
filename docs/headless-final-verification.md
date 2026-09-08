# Final headless milestone verification

Status: complete; all mandatory non-R6 checks passed with the documented continuations.
This record supplements the chronological
[milestone](headless-milestone.md) and [migration inventory](python-rust-migration.md).
No commit, push, PR, system permission change or paid model call was used.

## Scope and boundaries

- fux retains generic multiplexer/PTY, terminal, lifecycle, input receipt and attachment
  APIs. Immutable pane views are shared within a publication pass; agent policy stays out.
- zor owns capability reporting, provider launch policy, native evidence, tasks,
  orchestration, worktrees, checks, artifacts, recovery and presentation. Git and check
  execution use the bounded production subprocess runner; pure identity checks and
  deterministic fixture boundaries exercise coordination without external accounts.
- koh session framing accepts generic stream halves; deterministic production tests
  cover partial writes, pressure, deadlines and cancellation. Koh interprets no tasks.
- All 74 inventoried first-party Python files and two embedded execution sites now use
  separately buildable Rust tooling. Every original required check remains wired into
  the mandatory gate. Historical `.py.txt` files are nonexecutable provenance records;
  upstream reference projects are outside the migration.

## Evidence and limitations

[Performance results](headless-performance/README.md) retain the full paired matrix,
including the single-viewer regression. Four-pane burst CPU medians improved from
201.15 to 174.03 ms and from 193.03 to 175.68 ms with a slow reader; single-pane,
single-viewer burst CPU rose from 29.88 to 33.01 ms. These fixed-order dev-build samples
establish neither a universal speedup nor a wire-volume reduction. Original measurements
are unchanged. Rust capture timings include bounded subprocess polling and are not
substituted into historical comparisons. Internal allocation/lock-hold metrics remain
unavailable. Journal read/replay produced no extra replacements; no speculative storage
rewrite was made. Koh cost measurements are deterministic, not remote runtime evidence.

Zor's headless capability contract describes implemented launch/discovery and evidence
semantics with explicit unsupported reasons. Claude/Codex native message correlation
and recreation are unavailable; native interruption is unavailable for all three
adapters. OpenCode recreation is conditional and refuses stop intent. The required
two-worker workflow exercises literal launch arguments, blocker/check failure, repair,
reconciliation, verification and retained handoff using local scripted providers.
Optional local OpenCode integration captures are separately attributed; fixtures and
signed-out executable inspection are not proof of live model success.

## Final migration checks and review

The final binding scenario passed once in 27.55s. The final service scenario passed in
69.87s after two accepted-socket blocking-mode fixes. Strict Rust tooling Clippy and
root/tool formatting passed. Logs:

- `/tmp/fux-rust-bindings-runtime.log`
- `/tmp/fux-rust-bindings-clippy.log`
- `/tmp/fux-rust-service-final-runtime.log`
- `/tmp/fux-rust-service-final-clippy.log`
- `/tmp/fux-rust-service-root-format.log`
- `/tmp/fux-rust-service-tool-format.log`

Independent review compared both complete Python originals with their Rust scenarios,
fixtures and dispatch. All original assertions and Busy replay allowlists are represented.
The service reviewer accepted rejection of separate failure-path restoration for
mutations confined to the disposable root: failures abort the scenario and owned
children stop before root deletion; no retained state or following test reuses it.

The complete root intended-diff review covered HEAD changes and relevant untracked
production, test/tool, gate and documentation files, reusing detailed incremental
reviews. It found no confirmed production/migration integration defect. Its stale
protocol-consumer documentation pointer was corrected. Complete companion review
also accepted the zor and koh intended diffs, ownership boundaries and patch/gate
linkage without remaining confirmed findings. No review duplicated benchmarks or
runtime checks.

The final test-only zor lint corrections were exported and byte-for-byte companion
reconstruction passed. Evidence: `/tmp/fux-headless-final-patches-export.log` and
`/tmp/fux-headless-final-patches-verify.log`. Koh's patch is unchanged. No production
change or historical measurement was invalidated by these final test corrections.

## Mandatory final gate

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- dependencies verify --build --headless
```

The initial invocation stopped at a real historical-workflow provenance failure:
the retained report still hashed the now-deleted Python contention helper. Its exact
original bytes were recovered from the existing package and retained as a nonexecutable
archive. Only the original path/hash selects it; a wrong-hash regression rejects
substitution. Four affected evidence tests and independent final-delta review passed.
Historical measurements were not changed. Initial failure log:
`/tmp/fux-headless-final-gate.log`; regression log:
`/tmp/fux-headless-final-workflow-archive-tests.log`.

The corrected invocation, `/tmp/fux-headless-final-gate-fixed.log`, passed commands
0–31 in the mandatory `tools/xtask/checks.json` headless plan, including reconstruction,
both tooling suites, every retained validator and all real-process integrations. It
then found an `expect()` lint violation in the shared notifier test helper. Only the
failure diagnostic changed to the existing explicit-panic pattern. Independent review
confirmed that successful runtime evidence remains valid.

The failed check and remaining commands were continued on the final workspace with
the gate's explicit binary requirements and `target/dependency-verification` target.
Unchanged successful checks were reused rather than repeating the full integration
suite. Further findings were confined to tests:

- Five fixture-child scenarios used an obsolete immediate-manager-exit expectation.
  All five failed only at the server shutdown deadline after their behavioral assertions
  passed. They now assert workspace/socket/descriptor retirement, then stop their owned
  server explicitly. The related autostart scenario authenticates its live manager peer
  before cleanup. Existing pane/viewer/event and explicit-shutdown assertions remain.
  Independent review accepted the change; all eight binary scenarios passed in 16.71s.
- Zor capability tests used unchecked indexing/unwraps forbidden by strict Clippy.
  Checked access and explicit diagnostics preserve exact field values, literal argv,
  unsupported-agent errors and read-only behavior. No production code or lint policy
  changed. Independent review, strict lint and the affected tests passed.
- The temporary continuation runner initially exposed its command-list stdin to a
  passthrough test. This was a runner error, not a product failure. Child stdin was
  isolated; the entire nine-test passthrough group passed on retry. No source fix was
  made for that observation.

All 45 mandatory plan commands now have passing evidence. The original monolithic
invocation did not exit successfully; acceptance combines its unchanged passing prefix
with the affected-check and remaining-command continuations below. No failed command
was silently skipped and no non-R6 exclusion was added.

| Verification | Final result / evidence |
|---|---|
| Reconstruction, tooling and retained validators | Passed; `/tmp/fux-headless-final-gate-fixed.log`: fux tooling 20 library + 17 binary tests, zor tooling 15 tests; all retained validators passed |
| Real fux/zor integrations | Passed in the reconstructed tree: boundary 3, ECS 31, local CLI 11, structure 8, real-zor 20 (243.27s), including two-worker workflow |
| Root formatting, strict lint, unit tests and docs | Passed; `/tmp/fux-headless-final-gate-remaining.log`: 84 library + 3 binary tests |
| Fixture-child strict lint and tests | Passed; `/tmp/fux-headless-final-gate-fixture-fixed.log`: 3 unit + 8 binary + 2 lifecycle tests |
| Zor formatting and strict all-target lint | Passed; `/tmp/fux-headless-final-zor-format.log`, `/tmp/fux-headless-final-zor-clippy-fixed.log` |
| Zor all-feature tests | Passing groups in `/tmp/fux-headless-final-gate-companions.log`: 91 library, 8 bundled-rule, 3 CLI, 6 OSC; final 9 passthrough and doctests in `/tmp/fux-headless-final-gate-last.log` |
| Zor no-default-feature build, koh formatting/lint and deterministic gateway tests | Passed; `/tmp/fux-headless-final-gate-last.log`: koh 12 passed, only the named R6 cases excluded by the gate |
| Packaging | Passed; `/tmp/fux-headless-final-gate-last.log`: 176 files, 3.1 MiB, packaged fux compiled successfully |
| Complete diff and final corrections | Independent root and companion reviews accepted; every later archive, helper, fixture and capability-test correction independently accepted |

The source inventory is 74/74 ported plus both embedded sites. No unresolved non-R6
blocker remains. Repository commits, pushes and PRs were not performed.

R6 remains explicitly deferred: live remote authorization, network reconnect,
retention-window acceptance and netmon runtime behavior are unverified. The gate
names only the deferred remote cases and keeps all non-R6 checks mandatory.
