# First-party Python to Rust migration

User scope: all first-party scripts (74 files at inventory), including verification,
evidence capture/validation and performance tooling in fux and zor. Koh has no first-party
Python files. Upstream reference projects remain outside this migration.

The migration extends the active headless milestone. It does not authorize remote R6
checks, system permission changes, commits, pushes or PRs. Existing changes are preserved.

Final status (2026-09-07): **complete within the non-R6 scope**. All 74 files and both
embedded execution sites are ported, independently reviewed and covered by passing
mandatory checks. Companion patches were refreshed and reconstruction verified.
[Final verification](headless-final-verification.md) records the complete review,
actual gate failures, confirmed fixes and targeted continuations that reused unchanged
passing evidence. It supersedes pending-status statements in the chronological history.

- Put developer tooling in separately buildable Rust tooling crates, outside production
  fux/zor/koh binaries. Keep provider policy in zor and opaque transport in koh.
- Port dependency reconstruction first, then shared process/socket/fixture support,
  real-process scenarios, evidence validators, capture tools and performance tools.
- Preserve each script's positive and negative assertions, cleanup, bounds, CLI behavior
  and retained evidence semantics. A Rust launcher for Python is not a migration.
- Replace embedded Python fixture workers with Rust fixtures as well. Update Rust test
  callers, CI and documentation. Required checks remain enabled during the transition.
- Preserve original retained measurements and their historical source hashes. When source
  retention is needed for provenance, retain archival text explicitly rather than relabeling
  a new Rust run as the old Python measurement. Rerun only evidence actually invalidated.
- Track every inventoried script in `python-rust-migration.json`; a file is complete only
  when its Rust replacement and callers pass relevant checks and independent review.
- Final acceptance: no first-party Python executable/tooling dependency, all non-R6
  checks mandatory in the Rust verification entry point, independent intended-diff review,
  refreshed companion patches, and the settled-source final gate. No checks are dropped.

Initial inventory: 23 fux tools (3,233 lines), 34 fux verification files (7,391 lines),
and 17 zor tools (2,993 lines). Total: 74 files / 13,617 lines. Inventory counts are
initial scope, not a completion claim.

The inventory also tracks embedded Python in `tests/verify/release-package.sh` and
`zor/tools/test_opencode_adapter.mjs`. Python workers inside inventoried `.py` files
are included with their parent scripts.

Current verified migration: all 74 inventoried files, plus both separately
inventoried embedded execution sites. All 11 local CLI scenarios use Rust. The real
fux/zor observer, scripted headless launch, contention regressions and process-cleanup
regressions also run through Rust from their normal Cargo entry points. No first-party
Python executable or caller remains; historical `.py.txt` archives are provenance only.
The Git-change and check-worker scenarios also passed, including hook isolation, raw
filenames, bounded admission, responsive coordination and shutdown of active checks.
The historical lifecycle validator covers all retained traces and original mutations;
additional regressions preserve Python's refusal of absent viewport/provenance fields.

The viewer port uses a private vt100 model with a private clipboard callback. Review
found an unfinished-OSC buffer regression; a guard now preserves the original escape
limit across terminal reads, with an overflow regression. PTY originals use CLOEXEC;
terminal cleanup kills/reaps owned viewers before stopping their authenticated manager.
Server diagnostics fail on overflow instead of establishing negative evidence from
truncated output. No live provider call or deferred R6 check was used.

Integration scenarios have separate Cargo test names for targeted checks. A shared mutex
preserves their original sequential execution. Both standalone Rust tool owners are in
the mandatory full and non-R6 verification plans. The final gate, remaining migration,
companion patch refresh and complete intended-diff review are still pending.

Latest batch: lifecycle and startup capture implementations, authenticated Codex/Claude
validators and capture tools, and local recovery now have Rust replacements. The startup
Python module remains solely for four pending capture callers importing its RPC helper.
Historical lifecycle and authenticated capture sources are archived verbatim as `.py.txt`;
o retained provider evidence is relabeled as a Rust capture.

Verification on 2026-09-07:
- `cargo test --manifest-path tools/xtask/Cargo.toml --locked`: 18 tests passed.
- `cargo +1.91.0 test --manifest-path zor/tools/xtask/Cargo.toml --locked --all-targets`:
  six test functions preserve the original lifecycle, startup failure matrix, Codex/approval,
  Claude and redaction cases, including negative mutations; passed.
- Both tooling crates pass formatting and strict all-target Clippy. The affected root
  integration target passes formatting and strict Clippy.
- `ZOR_BIN=... FUX_REQUIRE_ZOR_BIN=1 cargo test --locked --test zor_integration zor_recovery -- --exact`:
  passed against real local fux/zor, including durable stop recovery, uncertain/lost distinction,
  retained history/receipts and no input replay into the replacement process.
- Actual fux capture plumbing passed with the Rust lifecycle fixture for lifecycle/startup
  and the Rust provider fixture for Codex response, unanswered approval and Claude response.
  Dummy credentials only; no provider calls. The first scripted Claude test correctly failed
  validation when its brief working screen was missed; extending that fixture state to one
  second made the affected test pass. These temporary capture artifacts contain the legacy
  provider label expected by the validator and MUST NOT be cited as real provider evidence.
- Independent source reviews accepted the capture ports and recovery port. Confirmed missing
  lifecycle-field checks and startup signal-race handling were fixed and regression tested.
  Strict lint found the gate policy's newly single-element loop; it is now a direct assertion.

Temporary logs: `/tmp/zor-rust-provider-final-{tests,clippy}.log`,
`/tmp/fux-rust-recovery.log`, `/tmp/fux-rust-recovery-root-clippy.log`,
`/tmp/fux-rust-migration-current-{tests,clippy}.log`.
Scripted captures: `/tmp/zor-rust-lifecycle-check.dgh2VS/evidence`,
`/tmp/zor-rust-startup-check.eDwiGE/evidence`, and
`/tmp/zor-rust-provider-check.yQMkhC` (Claude success is `claude-authenticated-recheck`).
These paths are ephemeral, supplemental evidence; committed validators retain the historical
real traces and provenance checks.

Twenty-three inventoried scripts remain: 19 pending and four with Rust support whose Python
callers still need migration. The final `verify --build --headless` gate, complete intended-diff
review and invalidated-evidence reconciliation remain pending. All non-R6 requirements stay
mandatory; R6 remote runtime checks remain explicitly deferred.

Companion checkpoint: `cargo run --manifest-path tools/xtask/Cargo.toml --locked --
dependencies export` and `dependencies verify` both passed for koh and zor after this batch.
Logs are `/tmp/fux-rust-migration-current-patches-{export,verify}.log`. This verifies exact
source reconstruction only; it does not replace the remaining final build/check gate.

Dashboard/workflow increment (2026-09-07): both normal Cargo integration entries pass
against real local fux/zor. The dashboard's notifier is `zor-notifier-fixture`; the workflow
worker/check are Rust `fixture-worker` modes. No embedded Python remains in these scenarios.
Dashboard parity fixes use the actual artifact-problem map and mask macOS PENDIN in the
fresh libc termios snapshot; nix's cached inner flags otherwise made its wrapper equality
stricter than the original check. Notification privacy, deduplication, timeout/reaping,
terminal restoration, focus and stale rows after service loss all pass.
The workflow verifies two isolated workers, failed dependency repair, sealed handoff retries,
corrupt journal rejection/restoration, stop recovery and artifacts after checkout removal.
The previously pending H5 capability CLI/API/read-only checks now pass in this workflow.
Independent source reviewers accepted both final ports. All 18 fux tooling tests, six zor
tooling test functions, affected strict Clippy checks and formatting pass. Logs:
`/tmp/fux-rust-dashboard.log`, `/tmp/fux-rust-workflow.log`,
`/tmp/fux-rust-dashboard-workflow-{unit,clippy,root-clippy}.log`,
`/tmp/zor-rust-notifier-{tests,clippy}.log`.

Workflow comparison integrity now runs as Rust `verify-workflow` in both gate profiles.
Its original success and four negative mutations pass against unchanged retained JSON.
Historical capture-source hashes select exact archival text under `tools/archive/`; current
capture hashes select current source. Independent review caught and verified the correction
that prevents a new capture from being checked against an old archive. A regression covers
current source acceptance and incorrect hash rejection. The pending comparison capture tool
now invokes the Rust workflow scenario, so no live caller needs the deleted Python scenario.
The complete fux tooling suite now contains 20 tests: the previously passing 18 plus two
comparison evidence tests. The final affected evidence tests and strict lint passed in
`/tmp/fux-rust-workflow-evidence-final-{tests,clippy}.log`.

After the dashboard/workflow increment, companion export and exact reconstruction both
passed for koh and zor. Logs: `/tmp/fux-rust-dashboard-workflow-patches-{export,verify}.log`.
All check handles in this increment were recovered to terminal status; none remain running.
No paid calls, system notification services, or deferred R6 runtime checks were used.

Detection inventory now runs through `cargo run --manifest-path tools/xtask/Cargo.toml
--locked -- detection-inventory --output PATH [--binary AGENT=PATH]`. Exact JSON payload
and Markdown parity passed against the original for ordinary PATH plus override, empty PATH
with a cwd executable, and alias-only PATH plus primary override. Review found and accepted
fixes for empty-PATH semantics and insertion order in Markdown. The original source is
archived at `tools/archive/tools/comparisons/detection_inventory.py.txt`, preserving the
retained inventory's harness hash. No agent is executed and availability is not authentication
or accuracy evidence. Current tooling uses Rust binary harness attribution.
All 20 tooling tests, strict Clippy and formatting pass. Logs:
`/tmp/fux-rust-inventory-final-{tests,clippy}.log`; parity driver
`/tmp/check-rust-inventory.rb` and ordinary outputs `/tmp/fux-{rust,python}-inventory.{json,md}`.
Companion sources are unchanged since the previous successful reconstruction; that evidence
is reused. Forty-two inventoried files and the final milestone acceptance remain outstanding.

Comparison validator increment: capture-traffic, controller-setup and resources offline
checks are Rust, including all original negative mutations and complete 9/6/18-row matrices.
Existing source hashes still match; retained records are unchanged. Float round-trip parsing
is enabled only in xtask to preserve Python's exact duration equality (a default JSON parser
rounding difference initially failed traffic validation). No timing tolerance was relaxed.
Both gate profiles, CI and active comparison docs invoke Rust; the previously stale CI
workflow-validator entry is corrected too. Independent review accepted the final code/wiring.
All 23 tooling tests, strict all-target Clippy, formatting, standalone validator commands
and affected gate policy pass. Logs: `/tmp/fux-rust-comparison-validators-final-{tests,clippy}.log`,
`/tmp/fux-rust-{traffic,setup,resources}-cli.log`, `/tmp/fux-rust-validator-gate.log`.
Companion dependency sources remain unchanged, so the prior valid reconstruction is reused.
Current remaining scope is 39 inventoried files (35 pending, four with Rust support but
pending Python callers), followed by the final milestone acceptance. No benchmark, paid call
or deferred R6 runtime check ran in this increment.

Detection validator increment: screen and freshness checks are Rust in both gates and CI.
Screen validation reconstructs all 11 real fixtures and 66 derived negatives, checks source
pointers/text hashes and recomputes all scores. Freshness validates all six observations,
censoring, controller state agreement, pane loss and the original negative mutations.
Review found a missing-null-rule field could be accepted; explicit required rule fields and
a regression fix it. The historical screen hash for `zor/src/main.rs` was reconciled by
reversing only the known H4 headless CLI additions and restoring original argv field order.
Exact hash `23b0a94f3ca7249425afc8ba95d64536e042b4da791370e328efc39c3191a534`
was recovered; bytes are archived in `tools/archive/zor/src/main.rs.txt`. Only that exact
historical path/hash pair selects the archive. Current captures still validate current
sources. The reviewer inspected the archive/current diff and accepted the reconstruction.
This preserves historical attribution; it does not claim the old capture used current code.
All 25 tooling tests, standalone detection validator commands, formatting and strict lint
pass. Logs: `/tmp/fux-rust-detection-final-all-tests.log`,
`/tmp/fux-rust-detection-validator-final-clippy.log`,
`/tmp/fux-rust-detection-{screens,freshness}-cli.log`. All comparison-validator CI commands
now use Rust. Thirty-seven files remain (33 pending and four awaiting caller migration).
No benchmark or live provider rerun, remote R6 check, or production code edit was made.
Companion reconstruction evidence remains valid because dependency sources are unchanged.

Native and zor resume evidence validators now run in Rust via `verify-opencode-resume`
and `verify-zor-resume` in zor-xtask. All original 8+9 negative mutations pass, with two
review-driven regressions requiring chat-part type and provider-message role fields.
Native session/message identity, complete receipt bytes, no replay before resumed input,
provider history, semantic response correlation, pending/historical operation equality,
new attempt verification and historical-launch exclusion remain checked. Original capture
scripts and retained hashes are unchanged. Both full/headless gate entries and active fixture
documentation use Rust. Independent final review accepted the code and wiring.
Rust 1.91 all-target tooling tests (eight test functions), formatting and strict Clippy pass;
affected gate policy and both CLI commands pass. Logs:
`/tmp/zor-rust-resume-final-{tests,clippy}.log`, `/tmp/zor-rust-{native,zor}-resume-cli.log`,
`/tmp/fux-rust-resume-gate.log`. No live resume, provider call or R6 remote check was run.
Thirty-five inventoried files remain (31 pending; four still supporting pending callers).

Resume checkpoint: both companion exports and exact source reconstructions passed after
these zor tooling changes. Logs: `/tmp/fux-rust-resume-patches-{export,verify}.log`.
All check handles in this increment are terminal. The final combined build gate remains pending.

Native-event validation is now `zor-xtask verify-opencode-events`, preserving exact retained
outcomes and all 12 adversarial mutations. It checks distinct native prompts, receipt bytes
and order, correlated completed assistant messages, later idle, completed read of the owned
fixture, and final text after intermediate tool-stop. Wrong session/tool/path, stale or
missing final text and partial/intervening input cannot pass. Independent review accepted
both the implementation and required gate wiring. All nine zor tooling test functions,
strict Rust1.91 lint, formatting, standalone command and exact gate assertion pass.
Logs: `/tmp/zor-rust-native-events-final-{tests,clippy}.log`,
`/tmp/zor-rust-native-events-cli.log`, `/tmp/fux-rust-native-events-gate.log`.
No capture, provider call or remote R6 check ran. Thirty-four inventoried scripts remain
(30 pending and four with pending Python callers), plus final milestone verification.

Native-event checkpoint: companion export and exact reconstruction passed for both
repositories. Logs: `/tmp/fux-rust-native-events-patches-{export,verify}.log`.
All checks in this increment are terminal; the combined final build gate remains pending.

### Managed integration validator continuation

`verify-opencode-integration` replaces the Python test in both required gate profiles.
All four retained contracts and the original mutations (including question tests inherited
by producer reload) pass. Review identified a missing dashboard-key rejection; required
lookup and its regression now match Python. The provider-response lookup likewise requires
its field while preserving short-circuit behavior. Final independent review accepted.
Historical reload instrumentation is nonexecutable text used only for exact hash checks.
The capture remains Python and pending migration.

Rust 1.91 all-target tests (10 test functions), strict all-target clippy, formatting and
CLI execution pass. Logs: `/tmp/zor-rust-integration-final-{tests,clippy}.log`,
`/tmp/zor-rust-integration-cli.log`, `/tmp/fux-rust-integration-gate.log`.
Current inventory is 41 ported, four ready with callers pending, and 29 pending.
Final intended-diff review and the settled-source mandatory non-R6 gate remain outstanding.
No paid calls, real provider evidence, or R6 runtime checks were performed in this increment.
Companion patches were refreshed and reconstructed successfully for koh and zor;
logs: `/tmp/fux-rust-integration-patches-{export,verify}.log`.

### Owned worktree scenario continuation

The normal `zor_worktree` Cargo entry point now runs the Rust scenario, including its
Rust checkout-filter worker. Original creation/removal intent replay, filter-held caller
crash, journal capacity, allocation recovery, dirty/ignored data, replacement-path refusal,
and interrupted removal assertions are retained. Test and independent review both found
that injected parent paths needed canonicalization on macOS; the fix passes the complete
scenario and final independent review.

`ZOR_BIN="$PWD/zor/target/debug/zor" FUX_REQUIRE_ZOR_BIN=1 cargo test --locked --test zor_integration zor_worktree -- --exact --nocapture`
passes (`/tmp/fux-rust-worktree-final.log`). Strict all-target tooling clippy passes
(`/tmp/fux-rust-worktree-final-clippy.log`); root and tooling formatting checks pass.
Companion repositories are unchanged in this increment, so the preceding successful
patch export/reconstruction evidence remains valid. Current inventory: 42 ported,
four ready with callers pending, 28 pending. Full migration, intended-diff review and
settled-source mandatory non-R6 verification remain incomplete. R6 stays deferred.

### Resource sampler check continuation

`resource-sampler-check --output PATH` in fux-xtask replaces the Python harness and
its embedded allocation worker. The existing C sampler remains unchanged. A real
owned Rust child passed all three CPU calibrations, six invalid-PID checks, stable
start identity, both >=24 MiB memory-growth assertions, and normal child exit.
Readiness, compilation, sampling and cleanup remain bounded. New evidence carries
Rust harness provenance; historical Python measurements are unchanged and labelled.

Evidence: `/tmp/fux-rust-resource-sampler-check.json`; execution and strict all-target
lint logs: `/tmp/fux-rust-sampler{,-clippy}.log`. Tooling formatting and independent
source review pass. Companion sources are unchanged, so prior patch reconstruction
remains valid. Inventory now has 43 ported, four ready with callers pending, and
27 pending. Final intended-diff review and settled-source non-R6 gate remain pending.
No paid calls, model credentials, permissions, or R6 runtime checks were used.

### Headless performance evidence validator continuation

`verify-headless-performance` replaces the original Python validator in the existing
required headless gate. Paired matrices, geometry, calibration, owner identity and
cleanup, counters, explicit build/source provenance, and all journal activity/replay
assertions remain enforced. All four original mutations pass. Review identified loss
of integer precision through f64 comparisons; exact integer comparisons and a valid
large-counter report plus three one-unit rejection cases now cover that defect.
Final independent review accepted the fixes and wiring.

Targeted validator and headless gate-policy tests, strict all-target tooling lint,
formatting and the CLI command pass. Logs:
`/tmp/fux-rust-headless-evidence-final-{tests,clippy}.log` and
`/tmp/fux-rust-headless-evidence-cli.log`. Retained performance evidence and current
source hashes are unchanged; no benchmark rerun was needed. Journal/performance
capture tools still await migration. Companion sources are unchanged, preserving
prior patch reconstruction evidence. Inventory: 44 ported, four ready with callers
pending, 26 pending. Final intended-diff review and settled-source non-R6 gate remain
incomplete; R6 stays explicitly deferred.

### Journal capture continuation

`headless-journal --fux PATH --zor PATH --output NEW_JSON` replaces the Python capture.
All three final-source repetitions of 96 operations passed against real local binaries:
32 adoptions replace the journal, 32 inspections and 32 idempotent adoptions do not,
final generation is 32, and server/worker cleanup completes. The report records Rust
harness and process-runner hashes and discloses that completion polling contributes
to elapsed time; it must not be compared directly with historical Python latency.

Final new capture: `/tmp/fux-rust-journal-final-capture.json` and corresponding `.log`.
The original source remains byte-identical archival text for the retained baseline;
after removing the executable Python file, retained evidence and headless gate-policy
tests pass. Strict tooling lint and formatting pass. Logs:
`/tmp/fux-rust-journal-final-{tests,clippy}.log`. Independent final source/provenance
review accepted. Companion sources are unchanged, so prior reconstruction remains valid.
Inventory: 45 ported, four ready with callers pending, 25 pending. Full intended-diff
review and settled-source mandatory non-R6 verification remain outstanding. No paid
calls, credentials, permission changes, or R6 runtime checks were used.

### Paired screen capture continuation

`capture-detection-screens` replaces the Python capture, sharing the reviewed Rust
corpus and score accounting. Final-source results for all 77 cases, summaries, scope,
and limitations exactly match a one-time original Python run on the same production
binaries. No live agents or accounts are launched. Review caught the old validator's
Python-only harness hash assumption; Rust provenance now verifies the Rust source,
while historical reports require the exact archived Python path/hash. The verifier
accepts an optional report path and retains default regression behavior.

Final capture: `/tmp/fux-rust-screen-final-capture.json`; original parity report:
`/tmp/fux-python-screen-parity.json`. Both fresh Rust and retained historical reports
pass validation (`/tmp/fux-rust-screen-{fresh,retained}-final.log`). Targeted tests,
strict all-target lint, formatting and final independent review pass; logs:
`/tmp/fux-rust-screen-capture-final-{tests,clippy}.log`. Original source is archived
as nonexecutable text; executable Python removed. Companion sources are unchanged,
so prior reconstruction remains valid. Inventory: 46 ported, four ready with callers
pending, 24 pending. Full intended-diff review and settled-source mandatory non-R6
verification remain outstanding; R6 stays deferred.

### Producer integration scenario continuation

`zor_producers` now dispatches to the Rust scenario. Its original Node adapter worker
is retained byte-for-byte as JavaScript; only Python orchestration was replaced.
Original duplicate endpoint, producer reload/retirement, heartbeat reset, report replay,
input sequence, arm-before-input crash, abandonment and local history-capacity checks
remain. Exact Busy retries use the existing explicit replay allowlist. Review found a
failure-path drop-order issue; server fallback cleanup now precedes temporary root removal.
Final independent review accepted the fix and complete port.

The final-source normal required real-binary Cargo integration test passes:
`ZOR_BIN="$PWD/zor/target/debug/zor" FUX_REQUIRE_ZOR_BIN=1 cargo test --locked --test zor_integration zor_producers -- --exact --nocapture`.
Logs: `/tmp/fux-rust-producers-final.log`, `/tmp/fux-rust-producers-final-clippy.log`.
Strict all-target tooling lint and affected formatting pass. Companion sources remain
unchanged, preserving prior reconstruction evidence. Inventory: 47 ported, four ready
with callers pending, 23 pending. Full intended-diff review and settled-source non-R6
verification remain outstanding. Native SDK events are synthetic; no paid calls,
accounts, permission changes or remote R6 runtime checks were used.

### Paired workflow capture continuation

`capture-workflow` replaces Python orchestration and its embedded Python worker.
The generated shell launcher executes a bounded Rust worker. The full zor durable
workflow acceptance and herdr's three-worker worktree/failed-beta/repair/handoff
workload pass against real local binaries. Herdr checks remain external harness
checks, never claimed as built-in verified completion. Dirty-tree protection,
explicit forced removal, main-repository preservation and cleanup are retained.

Fresh evidence `/tmp/fux-rust-workflow-capture.json` passes `verify-workflow REPORT`;
the default command still runs all original retained regressions after Python removal.
Logs: `/tmp/fux-rust-workflow-capture{,-verify,-retained}.log` and
`/tmp/fux-rust-workflow-capture-final-{tests,clippy}.log`. Targeted evidence tests,
strict all-target tooling lint, formatting and final independent source/wiring review
pass. Existing exact historical archives are unchanged. Companion sources are unchanged,
preserving prior reconstruction evidence. Inventory: 48 ported, four ready with callers
pending, 22 pending. Full intended-diff review and settled-source mandatory non-R6
verification remain outstanding. No paid calls/accounts, permissions or R6 runtime work.

### Legacy local measurement continuation

`measure BINARY --version N --samples N` replaces `tools/measure.py`, preserving its
JSON fields, workload bytes, idle duration and median/p95 selection. The version default
remains 3; the real current-binary validation explicitly used version 6 and five samples.
Review found a decimal rounding edge, fixed with decimal formatting and regression cases.
Startup retries now recognize both Rust socket error types for absent/refused endpoints,
with tests that reject permission errors. New documentation is `docs/measure-tool.md`.

Final real report: `/tmp/fux-rust-measure-final.json`; runtime log:
`/tmp/fux-rust-measure-final.log`. Two targeted tests, strict all-target tooling lint,
formatting and final independent review pass (`/tmp/fux-rust-measure-final-{tests,clippy}.log`).
No historical measurements or production sources were changed. Legacy path mentions in
historical prompts/reports remain historical; no active Python caller remains. Companion
sources are unchanged, preserving reconstruction evidence. Inventory: 49 ported, four
ready with callers pending, 21 pending. Full intended-diff review and settled-source
non-R6 verification remain outstanding; no permission changes or R6 checks performed.

### Signed-out freshness capture continuation

`capture-detection-freshness` replaces the Python capture. All six real local cases
(three repetitions per backend) pass with empty account storage and no prompt input.
Visible blocker checks, the three-second sample window, observed classifications
including censored blocker latency, pane close-to-absence and normal owner/agent cleanup
are retained. Fresh Rust and historical reports validate; the latter uses the exact
nonexecutable archived Python path/hash. The verifier accepts an optional report and
retains default original regressions. Independent final source/wiring review found
no confirmed issue.

Fresh evidence: `/tmp/fux-rust-freshness-capture.json`. Runtime, fresh verification and
after-deletion retained checks: `/tmp/fux-rust-freshness-capture{,-verify,-retained}.log`.
Strict all-target lint, targeted regressions and formatting pass; logs:
`/tmp/fux-rust-freshness-capture-final-{tests,clippy}.log`. Companion sources are unchanged,
preserving prior reconstruction evidence. Inventory: 50 ported, four ready with callers
pending, 20 pending. Full intended-diff review and settled-source mandatory non-R6
verification remain outstanding. No account/model calls, permissions or R6 runtime work.

### Traffic proxy support continuation (capture caller still pending)

Added `tools/xtask/src/support/traffic_proxy.rs`, the Rust counterpart of the transparent
proxy in `capture_traffic.py`. It preserves request/reply/event bytes, capture metadata,
shared-epoch timestamps, connection/buffer bounds and explicit cleanup errors. Two
end-to-end tests cover fragmented capture/event forwarding, byte accounting, idle
subscription shutdown, and malformed-preface rejection. Review found that the test
backend could block accepting its second connection after an early failure; bounded,
cancellable accepts now resolve that failure path. Final independent review accepted.

Targeted tests, strict all-target tooling lint and formatting pass. Logs:
`/tmp/fux-rust-traffic-proxy-final-{tests,clippy}.log`. No capture caller has switched yet,
so inventory remains 50 ported, four ready with callers pending and 20 pending.
Next work is the capture-traffic caller and its fresh/historical provenance wiring.
No production or companion source changed. Full migration, intended-diff review and
settled-source non-R6 gate remain outstanding; R6 remains explicitly deferred.

### Traffic capture caller continuation

`capture-traffic` now uses the Rust proxy and caller, sharing the original reviewed
summarization and row validation. All nine real cases (1/4/8 workspaces ×3) pass,
including zero idle captures, fresh burst coverage for every workspace, exact byte
accounting and normal owner/worker cleanup. The original C worker is retained exactly
as `tools/xtask/src/traffic-worker.c`. Herdr internal reads remain explicitly unmeasured.
Historical reports use the exact archived Python path/hash; optional report validation
supports fresh Rust captures without weakening the default matrix or negative cases.

The initial runtime matrix passed, but its report correctly failed provenance after
formatting changed the recorded source. A settled-source rerun passed all nine cases
and full report validation: `/tmp/fux-rust-traffic-final-capture.json` and
`/tmp/fux-rust-traffic-final-{capture,verify,retained}.log`. The rejected preliminary
report was not relabelled. Targeted proxy/validator regressions, strict lint, formatting
and independent final source/wiring review pass; logs:
`/tmp/fux-rust-traffic-capture-final-{tests,clippy}.log`. Python caller removed.
Companion sources are unchanged, preserving reconstruction evidence. Inventory:
51 ported, four ready with callers pending, 19 pending. Full intended-diff review and
settled-source mandatory non-R6 verification remain outstanding. No accounts, permission
changes or R6 runtime checks were used.

### Controller setup capture continuation

`capture-controller-setup` replaces the Python caller with Rust orchestration and
retains the exact C worker. All six real cases (zor/herdr ×3) pass: unavailable-service
diagnostics, populated initial overviews, disclosed command/poll counts, controller
reuse, worker survival across zor-only shutdown, no prompt input and normal cleanup.
Fresh reports verify their actual Rust/worker sources; historical evidence keeps its
exact original Python hash through a nonexecutable archive. Reproduction uses Rust,
and `verify-controller-setup [REPORT]` supports fresh reports while preserving the
retained six-case matrix and original negative regressions.

Independent review found that missing service identities could compare equal through
JSON null indexing. The corrected capture requires both fields. Its regression rejects
either/both missing identities and changed identities. Both targeted setup tests, strict
all-target lint, formatting and final independent source review pass. The corrected
six-case runtime was rerun on settled source and verified, rather than relabelling the
pre-fix capture: `/tmp/fux-rust-setup-final-capture.json`. Logs:
`/tmp/fux-rust-setup-final-{capture,verify,retained,tests,clippy}.log`.

Inventory: 52 ported, four ready with callers pending, 18 pending. No companion source
changed; previous reconstruction evidence remains valid. The remaining migration,
complete intended-diff review and mandatory final non-R6 gate are still outstanding.
R6 remote runtime remains deferred. No accounts or permission changes were used.

### Event observation scenario continuation

`tests/verify/zor_events.py` is replaced by `scenarios/zor_events.rs` and its private
`events_proxy.rs`. The normal `zor_events` Cargo entry now runs Rust. The real
fux/zor scenario retains the original 4.2-second idle window (at most two lists and
zero captures), fallback-scan alignment, event-driven refresh, input sequence, gap
invalidation, fresh resubscription, preserved handle, duplicate-cursor failure and
recovery, and two-second rule-reload observation. The fault proxy retains framing
bounds, 256 total/16 live connection limits and independent cleanup of every owner
and thread. Agent interpretation remains in zor; the proxy is test tooling only.

The required test passed once through the normal Cargo entry:
`FUX_REQUIRE_ZOR_BIN=1 ZOR_BIN=/Users/kisaczka/Desktop/code/fux/zor/target/debug/zor cargo test --locked --test zor_integration zor_events -- --exact --nocapture`.
Log: `/tmp/fux-rust-events-runtime.log`. Strict all-target tooling lint and formatting
pass (`/tmp/fux-rust-events-clippy.log`). Independent source review found no remaining
in-scope issues. No performance benchmark was rerun or prior measurement relabelled.

Inventory: 53 ported, four ready with callers pending, 17 pending. Companion sources
remain unchanged. Full migration, complete intended-diff review and final mandatory
non-R6 verification remain outstanding. R6 runtime stays explicitly deferred.

### Native OpenCode capture continuation

`zor-xtask capture-opencode-events` now runs real OpenCode through fux with a Rust
loopback provider and the exact original JS probe. It preserves two fixed prompts,
distinct reservations, delivered receipts, native parent/session correlation, actual
owned-file read completion, intermediate tool stop, final text and later idle. The
existing reviewed native-event validator checks the fresh normalized report. New
provenance identifies and hashes the actual Rust executable; historical reports are
unchanged. No cloud account, model call or task-success inference is involved.

Self-review restored failure diagnostic parity by recording a submission before the
idle wait. The corrected real OpenCode1.18.29 capture passed at
`/tmp/zor-rust-native-final-capture/events.json`. Four targeted native tests (including
all original native-event mutations), strict Rust1.91 all-target lint and formatting
pass. Independent final source review found no remaining confirmed defects. Logs:
`/tmp/zor-rust-native-final-{capture,tests,clippy}.log`. Reproduction and limits are in
[native-events-tool.md](native-events-tool.md).

Companion patches were refreshed and reconstructed successfully:
`/tmp/fux-rust-native-patches-{export,verify}.log`. This supersedes the earlier unchanged
companion checkpoint. No production code changed. The Python module remains because
native resume, zor resume and integration captures still import its helpers; it is
explicitly ready with callers pending. Inventory: 53 ported, five ready with callers
pending, 16 pending. Full migration, complete intended-diff review and settled-source
mandatory non-R6 verification remain outstanding. R6 remains deferred.

### Native resume capture continuation

`zor-xtask capture-opencode-resume` now exercises real OpenCode persistence across
two owned fux lifetimes. It uses the shared bounded loopback provider with a resume
response mode and retains both request histories. The original native validator
checks distinct server/process/message identities, the same native session, zero
input before each prompt, delivered receipts, prior viewport/provider history,
correlated responses and clean shutdown. No zor resume policy or cloud behavior is
inferred. The event reader and normalization helpers are shared with the native
capture; its existing provider branch and tests remain intact.

The real OpenCode1.18.29 run passed on the reviewed source at
`/tmp/zor-rust-native-resume-capture/resume.json`. Four targeted native tests, strict
Rust1.91 all-target lint and formatting passed. Independent review found no remaining
confirmed defects. Logs: `/tmp/zor-rust-native-resume-{capture,tests,clippy}.log`.
The fixture README reproduction now invokes Rust. Original historical Python evidence
and its hashes are unchanged. Companion export and reconstruction passed:
`/tmp/fux-rust-native-resume-patches-{export,verify}.log`.

`capture_zor_resume.py` still imports the original native validation helper, so the
Python module is explicitly ready with callers pending, not deleted or counted as
complete. Inventory: 53 ported, six ready with callers pending, 15 pending. The full
migration, complete intended-diff review and final mandatory non-R6 gate remain open.
R6 remains explicitly deferred; no account, paid model call or permission change was used.

### Explicit zor resume capture continuation

`zor-xtask capture-zor-resume` now exercises real OpenCode through zor's explicit
resume policy. The original assertions remain: stable task/native session/history,
distinct fux/process/attempt/session identities, zero replayed input, preserved old
prompt and never-submitted reservation, live/conflicting-intent refusals, missing
metadata/cancelled/terminator refusals without journal mutation, one actual creation
after a dropped creation reply, fresh bound responses, and current-attempt source/check
verification. The dashboard hides historical launches. Private fixture Git commits,
provider responses and report-token redaction remain within the owned root.

The first real run failed before fault injection: the macOS accepted proxy socket
inherited nonblocking mode. Explicit blocking mode fixed it; the real socket regression
checks one forwarded creation with a discarded reply and transparent list responses.
The corrected full capture passes at `/tmp/zor-rust-task-resume-fixed-capture/resume.json`;
the failed capture was not accepted or relabelled. All13 zor tooling tests, strict
Rust1.91 all-target lint, formatting and final independent source/wiring review pass.
Logs: `/tmp/zor-rust-task-resume-fixed-capture.log`,
`/tmp/zor-rust-task-resume-final-{tests,clippy}.log` and
`/tmp/zor-rust-resume-proxy-tests.log`.

Both native and zor resume Python files are removed, releasing the native resume
caller's pending status. Retained evidence verifies the exact filename/hash archives;
new captures identify their actual Rust executable. Reproduction commands use Rust.
Companion patches were refreshed and reconstructed:
`/tmp/fux-rust-task-resume-patches-{export,verify}.log`.
Inventory: 55 ported, five ready with callers pending, 14 pending. Full migration,
complete intended-diff review and final mandatory non-R6 verification remain open.
No accounts, paid calls or permission changes were used; R6 remains deferred.

### Integration reload launcher prerequisite

The embedded `RELOAD_EXEC` Python launcher in the remaining OpenCode integration
capture now has a Rust replacement: `zor-xtask reload-opencode-fixture AGENT [ARGS]`.
It substitutes only the final plugin, exports the original adapter path to the
existing test reload plugin, and execs the real agent with literal argv. The exact
existing reload plugin is extracted as `zor/tools/xtask/src/reload-plugin.mjs`.
This introduces no production reload endpoint or agent behavior in fux.

The targeted transformation regression and a real exec probe pass. The latter
checks unchanged foreground PID, empty/spaced/flag arguments, original plugin
identity and preservation of the remaining config. Strict Rust1.91 all-target lint,
formatting and independent source review pass. Logs:
`/tmp/zor-rust-reload-fixture-{tests,exec,clippy,build}.log`.
Companion export/reconstruction pass:
`/tmp/fux-rust-reload-fixture-patches-{export,verify}.log`.

The integration capture caller has not switched yet. Its provider modes, retirement
boundary, permission/question observations, producer reload, heartbeat/dashboard
checks, capture provenance and actual runtime variants still need conversion and
validation. Inventory remains 55 ported, five ready with callers pending, 14 pending.
No new acceptance requirements were added. Full migration, intended-diff review and
final mandatory non-R6 verification remain open; R6 stays deferred.

### Managed OpenCode integration capture continuation

`zor-xtask capture-opencode-integration` replaces the final managed capture caller,
using the Rust local provider, exact probe/reload JS and the reviewed Rust exec launcher.
Real OpenCode1.18.29 permission, question and question-with-reload variants pass the
existing integration validator. This includes pre-submit crash/arm retirement with
idempotent abandon and no input, correlated delivered responses, unanswered blockers,
producer replacement, retained response/storage identity, heartbeat freshness and a
native blocked dashboard. Task outcome stays Open. No cloud/model-quality inference.

The first permission capture failed storage validation because the shared private root
used noncanonical `/tmp` environment paths. The caller now sets canonical HOME/XDG
paths just like the original Python `.resolve()` fixture. Independent review also found
blocking adapter connection establishment before timeout setup; nonblocking connect,
deadline polling and socket-error validation fix that defect. A full-live-backlog
regression passes (including macOS's refusal behavior after queue saturation).
Corrected runtime captures are `/tmp/zor-rust-integration-reviewed-{permission,question,reload}/integration.json`
with corresponding `.log` files. Earlier failed evidence was not accepted or relabelled.

All15 zor tooling tests pass after deleting the Python scripts; strict Rust1.91
all-target lint, formatting and independent final source review pass. Logs:
`/tmp/zor-rust-integration-post-delete-{tests,clippy}.log`. Companion patches refreshed
and reconstructed: `/tmp/fux-rust-managed-integration-patches-{export,verify}.log`.
The reload validator retains the exact historical Python hash contract while checking
fresh Rust launcher identity against the executed capture binary. Native-event and
startup captures now have no Python importers, so their ready status is also complete.
Exact historical sources remain nonexecutable archives; retained JSON is unchanged.

Inventory: 58 ported, three ready with callers pending, 13 pending. All inventoried zor
Python scripts and its embedded Python execution site are migrated. Remaining scripts
are fux verification/comparison/performance tooling. Full intended-diff review and
settled-source final mandatory non-R6 verification remain outstanding. No accounts,
paid model calls or permission changes were used. R6 stays explicitly deferred.

### Group orchestration and scheduler continuation

The manual and automatic group scenarios now run Rust from their normal Cargo test
entry points. `scenarios/zor_groups.rs` retains admission/cycle/alias checks, all six
invalid journal mutations, explicit Busy replay policy, step reconciliation after
possible committed admission, concurrent manual steps, automatic failed-group fairness,
pause/resume, retained scheduler state across service SIGKILL/restart, wrong-state-root
refusal, failed-check repair, verified dependency/capacity release, dashboard provenance,
exact deduplicated input logs, artifact retention and owned worktree/process cleanup.
The worker appends input logs in Rust and the content check uses the existing Rust
fixture command. Both embedded Python execution sites leave with their parent scripts.

Both required real-process tests passed once via:
`FUX_REQUIRE_ZOR_BIN=1 ZOR_BIN=/Users/kisaczka/Desktop/code/fux/zor/target/debug/zor cargo test --locked --test zor_integration zor_group -- --nocapture`.
Log: `/tmp/fux-rust-groups-runtime.log` (two passed, 37.89s). Strict all-target tooling
lint and affected formatting pass; lint log `/tmp/fux-rust-groups-final-clippy.log`.
Independent review found no remaining confirmed defects. Both original Python files
were removed. Companion sources are unchanged, preserving the managed-integration
reconstruction checkpoint. No unchanged benchmark was rerun.

Inventory: 60 ported, three ready with callers pending, 11 pending. Remaining migration,
complete intended-diff review and final mandatory non-R6 verification stay open.
R6 remains explicitly deferred. No accounts, paid calls or permission changes were used.

### Managed-launch fault proxy prerequisite

`support/launch_proxy.rs` ports the two-socket control/manager fault proxy from the
remaining `zor_launch.py` scenario. It retains creation/kill counts; drop-before,
drop-after and held replies; waiting for short-command final records; workspace
vanishing; unavailable listings; event gaps/duplicate openings; expired/mismatched
final records; and oversized UTF-8 final evidence. Handshake/frame I/O is bounded and
cancellable, and closing the fixture releases held replies and joins both workers.
It remains development tooling, outside production fux/zor/koh binaries.

Two targeted regressions pass for fault transformations/precedence and cancellation
of a partial handshake. Strict all-target tooling lint, formatting and independent
source review pass. Logs: `/tmp/fux-rust-launch-proxy-{tests,clippy}.log`.
The caller has not switched yet; real managed-launch scenario validation remains
required before removing its Python implementation or counting it complete. The next
step is that caller, including killed creator/stopper, observation-loss recovery,
closed lifecycle evidence, no blind replacement and untouched host-pane assertions.

Inventory remains 60 ported, three ready with callers pending, 11 pending. Companion
sources and their reconstruction evidence are unchanged. Full migration, intended-diff
review and the final mandatory non-R6 gate remain open. R6 stays deferred.

### Managed-launch caller continuation

The normal `zor_launch` Cargo test now invokes `scenarios/zor_launch.rs` and the
reviewed Rust proxy. The full real scenario passes: managed/adopted authority, marker
and empty-argv preservation, stable-ID launch retry, dropped-before/after creation,
killed creator and stopper, no blind replacement, unavailable-observation stop safety,
idempotent stop, preserved task/prompt history on close, short-lived final recovery,
oversized UTF-8 final caps, gap/stream/expiry/duplicate refusal, vanished-workspace
recovery, and unchanged host-pane identity. The original Python caller is removed.

Required verification passed once:
`FUX_REQUIRE_ZOR_BIN=1 ZOR_BIN=/Users/kisaczka/Desktop/code/fux/zor/target/debug/zor cargo test --locked --test zor_integration zor_launch -- --exact --nocapture`.
Log `/tmp/fux-rust-launch-runtime.log` (one passed, 11.36s). Proxy regressions,
strict all-target tooling lint and affected formatting pass; logs
`/tmp/fux-rust-launch-final-proxy-tests.log` and `/tmp/fux-rust-launch-clippy.log`.
Independent review of the full caller/proxy/dispatch found no remaining confirmed
issues. No production or companion source changed, preserving prior reconstruction
and benchmark evidence.

Inventory: 61 ported, three ready with callers pending, ten pending. Full migration,
complete intended-diff review and final mandatory non-R6 verification remain open.
No accounts, paid calls or permission changes were used; R6 remains deferred.

### Task-state caller continuation

`zor_tasks` now runs Rust through its normal Cargo entry. It preserves durable/offline
adoption, exclusive preparation and journal permissions/corruption refusal, lost
reservation and submit replies, ambiguous retry deduplication, human interference,
submission deadlines, release/cancellation history, killed-submitter recovery,
producer token/input-operation correlation, process-exit distinction, and follow-wait
admission, caller budgets, cancellation and crash recovery.

The first real run exposed a fixture race: killing an admitted waiter could interrupt
the proxy's reply write, producing BrokenPipe and stopping observation. The fixture
now holds that waiter's next list request before forwarding, kills/reaps the waiter,
and releases the held connection. It suppresses no socket errors and retains all
original recovery assertions. No production code changed.

The final real run passed (10.53s):
`FUX_REQUIRE_ZOR_BIN=1 ZOR_BIN=/Users/kisaczka/Desktop/code/fux/zor/target/debug/zor cargo test --locked --test zor_integration zor_tasks -- --exact --nocapture`.
Evidence: `/tmp/fux-rust-tasks-runtime-fixed.log`,
`/tmp/fux-rust-tasks-final-clippy.log`, `/tmp/fux-rust-tasks-proxy-tests.log`.
Affected formatting passes. Companion source/reconstruction and benchmark evidence
remain unchanged. Inventory: 62 ported, three ready with callers pending, nine pending.
Full migration, complete intended-diff review and final mandatory non-R6 verification
remain open; R6 stays explicitly deferred.

### Paired resource capture continuation

`capture-resources` replaces `tools/comparisons/resources.py`. It preserves the
original 1/4/8-pane, fux+zor/herdr, 1–3-repetition interface, unchanged synthetic C
worker and native resource sampler, raw four-sample accounting, sequential component
sampling, one-second warmup, three-second idle and one-second post-burst settling,
visible-output/read counts, viewports and normal owner/worker cleanup. Each fresh row
runs the retained Rust accounting validator before publication. Historical capture
bytes are archived and selected only by their exact original path/hash.

Independent parity review found no confirmed defects. One full six-case matrix at
one repetition passed; no second measurement run was needed. Evidence:
`/tmp/fux-rust-resources-capture.json` and `/tmp/fux-rust-resources-capture.log`.
Fresh source hashes and calibration were checked. The retained 18-case matrix and
all accounting/PID/cleanup negative cases passed in
`/tmp/fux-rust-resources-retained.log`; strict all-target tooling lint passed in
`/tmp/fux-rust-resources-final-clippy.log`. Affected formatting passes. New Rust
subprocess polling adds sampler-launch overhead: these measurements verify the port,
not a claimed speedup or replacement for historical results.

Inventory: 63 ported, three ready with callers pending, eight pending. All inventoried
zor Python is already migrated. Companion reconstruction evidence is unchanged because
this increment only changes fux development tooling. Remaining migration, complete
diff review and final mandatory non-R6 verification remain open. R6 stays deferred.

### Service-failure comparison continuation

`capture-service-failure` ports all three original SIGKILL scopes: zor controller,
fux PTY owner and herdr PTY owner. Controller restart preserves worker/session/receipt,
retries do not duplicate input, a fresh bound report establishes response observation,
and task outcome stays open. Owner death ends its worker. The exact original C worker
keeps its 45-second alarm, setup remains below 30 seconds, and the crash observation
window is eight seconds. Cleanup attempts every owner and observes worker exit without
signalling a raw worker PID. No session-resume or real-provider claim is added.

The real three-case capture passed at one repetition; independent parity review found
no confirmed defects and strict all-target tooling lint passed. Evidence:
`/tmp/fux-rust-service-failure-capture.json`,
`/tmp/fux-rust-service-failure-capture.log`,
`/tmp/fux-rust-service-failure-clippy.log`. Fresh source hashes were checked.
The available verified herdr binary is the later build of the same reference commit:
an explicit `--herdr-provenance` records that build, retaining the original default
and leaving historical comparison evidence unchanged. Original Python is archived.

Inventory: 64 ported, three ready with callers pending, seven pending. Full migration,
complete intended-diff review and mandatory final non-R6 verification remain open.
Companion sources are unchanged; R6 remains deferred.

### Prompt-boundary and input-retry comparison continuation

`capture-prompt-boundary` preserves all seven backend/case rows, including prior
prompt report seeding, stale idle rejection, immediate/working response distinctions,
herdr's preblocked refusal, explicit state-change sequence evidence and exact input.
An asynchronously sent request with deferred receive replaces the Python threadpool;
separate hook requests still run while the prompt is pending. Public zor results
exclude report tokens and private paths. The exact synthetic C worker is extracted
as `tools/xtask/src/prompt-worker.c`.

`capture-input-retry` preserves all four backend/fault rows, dropped-before-forward
and dropped-after-consumption timing, clean EOF versus malformed-response refusal,
identical herdr request byte hashes, stable zor operation identity, exact input counts,
and final input verification after normal owner and worker cleanup. Its owned proxy
uses bounded, cancellation-aware I/O. A focused test rejects partial replies as
lost-reply evidence. The shared comparison harness stops its server before its root.

Both real matrices passed at one repetition, with independent parity reviews reporting
no confirmed issues. Evidence: `/tmp/fux-rust-prompt-capture.json`,
`/tmp/fux-rust-prompt-capture.log`, `/tmp/fux-rust-input-retry-capture.json`,
`/tmp/fux-rust-input-retry-capture.log`, `/tmp/fux-rust-input-retry-proxy-tests.log`.
Fresh source hashes and cleanup records were checked. The historical Python sources
are archived verbatim; exact prompt-helper path/hash dispatch preserves retained
setup/resource evidence without relabelling the Rust runs as historical captures.

Inventory: 65 ported, four ready with callers pending, five pending. The standalone
comparison captures now have reviewed Rust replacements. Final review caught the
headless performance capture's remaining import of `comparisons.prompt_boundary`;
the exact Python helper was restored and remains until that caller is migrated.
Remaining Python comprises four integration scenarios, their three shared helpers,
the headless performance capture, and its prompt-boundary helper. Final
complete-diff review and mandatory non-R6 gate remain open. No production or companion
source changed; unchanged reconstruction/benchmark evidence remains valid. R6 stays
explicitly deferred and no account or paid model call was used.

Final comparison lint/format and retained evidence checks passed:
`/tmp/fux-rust-comparisons-final-clippy.log`,
`/tmp/fux-rust-comparisons-final-format.log`,
`/tmp/fux-rust-comparison-archive-tests.log` (eight passed). The confirmed final-review
import finding was fixed by retaining the helper, not by weakening or removing the
pending headless performance check. Fresh capture evidence and archive hashes remain
unchanged; no measurement rerun was needed.

### Headless viewer performance continuation

`headless-performance` ports the remaining capture, preserving the exact C workload,
80×24 viewer geometry, four pane/viewer/slow-reader cases, 25 ms delay per decoded
message on the slow viewer, idle/burst/streaming phases, ordered resource samples,
received/decoded byte counts, frames and pending-buffer high-water marks. The reader
handles fragmented length-prefixed frames and closes before owned servers; all workers
must disappear. Linux retains explicit unavailable resource values; macOS uses the
unchanged native sampler. No account, network endpoint or permission change is needed.

The four-case matrix passed once at one repetition on settled source. The capture
validates the complete raw counter/identity/cleanup report before publication, and
`verify-headless-performance [REPORT_JSON]` also accepts fresh Rust captures with their
declared 1–5 repetitions. The historical default still requires its full 12-row matrix
and all original negative cases. Review found that an empty source map could bypass
helper hash checks; the validator now requires all five exact source paths, with
regressions for an empty map and deletion of every individual path. Final review
accepted that correction.

Evidence: `/tmp/fux-rust-headless-performance-capture.json`,
`/tmp/fux-rust-headless-performance-capture.log`,
`/tmp/fux-rust-headless-performance-final-tests.log` (four passed), and
`/tmp/fux-rust-headless-performance-final-clippy.log`. Historical Python is archived
under its exact hash. The previously reviewed prompt-boundary module is now removed
because its final Python import caller is migrated. Existing paired performance
artifacts are unchanged. Rust subprocess polling and scheduling affect new timings;
the new run verifies the port and does not establish another optimization benefit.

Inventory: 67 ported, three ready with callers pending, four pending. Only the four
integration scenarios and their three shared Python helpers remain. All comparison
and performance captures now have Rust implementations. Companion reconstruction is
unchanged. Complete intended-diff review and the mandatory final non-R6 gate remain
open; R6 stays explicitly deferred.

Post-deletion verification also passed: the fresh four-case report in
`/tmp/fux-rust-headless-performance-verify.log`, retained and missing-source tests
in `/tmp/fux-rust-headless-performance-post-delete-tests.log` (two passed), and
formatting in `/tmp/fux-rust-headless-performance-format.log`. Final independent
review accepted the deletion/archive/provenance delta with no remaining findings.

### Unused shared-helper closure

A dependency audit found no remaining executable callers of `cleanup.py` or
`owned_processes.py`. Their previously reviewed Rust replacements already serve the
migrated local CLI and observer scenarios, and the root owned-process regression
entry already dispatches Rust. Independent review confirmed preserved authenticated
manager ownership, deadlines, fallback kill/reap and accumulated cleanup failures.
Both unused originals are now removed. The Rust code and valid prior checks are
unchanged, so no repeated scenario or measurement run was needed.

Current inventory: 69 ported, one ready with callers pending, four pending. Remaining:
`zor_bindings.py`, `zor_checks.py`, `zor_service.py`, `zor_sources.py`, and their
`zor_contention.py` helper. Full intended-diff review and final mandatory non-R6 gate
remain pending. The objective stays active; R6 remains deferred.

### Check-evidence integration continuation

The normal `zor_checks` Cargo entry now runs Rust. The full scenario preserves
binary artifact bytes, immutable replay, task-scoped required artifacts/checks,
sealing, symlink/hardlink/FIFO/path/size refusals, artifact count/byte capacity and
malformed association/generation refusal without journal repair. Check coverage
includes explicit stdout/stderr/exit/signal evidence, UTF-8 output caps, mutable and
concurrently ordered requirements, timeouts/output pressure, exact full-journal error
publication, runner death with a still-live process group, no replay, and late
completion after concurrent cancellation. The capacity fixture waits for its result
writer before restoring the original journal. All three embedded Python workers are
Rust fixture modes exercised by the same real test.

Required verification passed once:
`FUX_REQUIRE_ZOR_BIN=1 ZOR_BIN=/Users/kisaczka/Desktop/code/fux/zor/target/debug/zor cargo test --locked --test zor_integration zor_checks -- --exact --nocapture`.
Evidence: `/tmp/fux-rust-checks-runtime.log` (one passed, 27.09s),
`/tmp/fux-rust-checks-clippy.log`, `/tmp/fux-rust-checks-root-format.log` and
`/tmp/fux-rust-checks-tool-format.log`. Independent full source/worker/dispatch parity
review found no confirmed defects or omitted assertions. No production/companion
source or historical measurement changed, so existing reconstruction evidence remains
valid. The Python scenario is removed.

Current inventory: 70 ported, one ready with callers pending, three pending. Remaining:
`zor_bindings.py`, `zor_service.py`, `zor_sources.py`, and `zor_contention.py`.
Complete intended-diff review and the mandatory final non-R6 gate remain open. The
original objective stays active and R6 remains explicitly deferred.

### Retained-source integration continuation

The normal `zor_sources` Cargo entry now runs Rust. The complete scenario preserves
committed binary bytes/executable modes, independent 0700 check directories, Git
helper and replacement-ref exclusion, source count/file limits and malformed retained
source refusal. Artifact coverage retains explicit missing/unsafe/timeout evidence,
reserved IDs, directory-rename descriptor safety, concurrent capture ordering and
exact journal-capacity publication of a 64 KiB artifact plus maximally escaped command
output. Historical-attempt inputs cannot satisfy the current attempt. SHA-256 Git
sources follow the same contract, and sealed verification retains its exact selected
source/check/artifact records through retries and owned worktree cleanup. All embedded
Python workers are Rust fixture modes exercised by the full real scenario.

Required verification passed once:
`FUX_REQUIRE_ZOR_BIN=1 ZOR_BIN=/Users/kisaczka/Desktop/code/fux/zor/target/debug/zor cargo test --locked --test zor_integration zor_sources -- --exact --nocapture`.
Evidence: `/tmp/fux-rust-sources-runtime.log` (one passed, 24.69s),
`/tmp/fux-rust-sources-clippy.log`, `/tmp/fux-rust-sources-root-format.log` and
`/tmp/fux-rust-sources-tool-format.log`. Independent complete source/worker/dispatch
review found no confirmed defect or omitted original assertion. The Python scenario
is removed. Production/companion sources and historical measurements are unchanged;
valid reconstruction evidence is reused.

Current inventory: 71 ported, one ready with callers pending, two pending. Remaining:
`zor_bindings.py`, `zor_service.py`, and their `zor_contention.py` helper. Complete
intended-diff review and the mandatory final non-R6 gate remain open. The full
objective stays active; R6 remains explicitly deferred.

### Native-binding integration continuation

The normal `zor_bindings` Cargo entry now runs Rust, including its owned Unix
adapter fixture. The complete original scenario retains native message ancestry,
producer/session lifetime and sequence refusal, receipt-only crash recovery,
read-only historical retries with fux unavailable, and human-input uncertainty.
Heartbeat coverage preserves immutable receipt time, monotonic expiry, wall-clock
rollback, unsafe/corrupt sidecars and the exact service replay allowlist. Adapter
coverage retains point-in-time probes, concurrent journal invalidation, bounded
hello faults, acknowledged arming, durable retirement and exact retry identities.
Dashboard assertions keep native blocked evidence above passive idle, surface
expired native evidence as unknown with attention, and expose pending group
retirement without performing it.

Required verification passed once:
`FUX_REQUIRE_ZOR_BIN=1 ZOR_BIN=/Users/kisaczka/Desktop/code/fux/zor/target/debug/zor cargo test --locked --test zor_integration zor_bindings -- --exact --nocapture`.
Evidence: `/tmp/fux-rust-bindings-runtime.log` (one passed, 27.55s),
`/tmp/fux-rust-bindings-clippy.log`, `/tmp/fux-rust-bindings-root-format.log` and
`/tmp/fux-rust-bindings-tool-format.log`. Independent review of the complete
original scenario, Rust scenario, adapter and dispatch found no confirmed defect
or omitted assertion. The Python scenario is removed. Production/companion
sources and retained measurements are unchanged, so reconstruction and measurement
evidence remain valid.

Current inventory: 72 ported, one ready with callers pending, one pending. The
remaining files are `zor_service.py` and its `zor_contention.py` helper. Complete
intended-diff review and the mandatory final non-R6 gate remain open. The full
objective remains active; R6 remains explicitly deferred.

### Shared-service integration and final Python removal

The final service scenario and its embedded descendant-process fixture are Rust.
Its worktree checks retain creation/current/descendant/unlinked directory evidence,
active and uncertain removal refusal, missing discovery, symlink rejection and exact
launch replay. Task APIs retain binary artifacts, source-bound checks/capture,
changes, strict fields, immutable policy and report claims without inferred success.
The full scenario also checks lost replies, explicit Busy replay permission, overload
admission, compact results after large committed mutations, storage failure isolation,
partial-client eviction, atomic rule reload, restart identity, daemon activation and
concurrent startup. The generic service fixture owns its processes and private sockets.

Independent full original-to-Rust review found two accepted-socket mode issues on
macOS; both blocking read sites now explicitly clear inherited nonblocking mode.
The final required `zor_service` Cargo scenario passed in 69.87s after those fixes
and removal of the Python dispatcher and last helper. Evidence:
`/tmp/fux-rust-service-final-runtime.log`, `/tmp/fux-rust-service-final-clippy.log`,
`/tmp/fux-rust-service-root-format.log`, `/tmp/fux-rust-service-tool-format.log`.
The initial pre-fix scenario also passed (73.50s); only the final run establishes
acceptance of the changed source. No benchmark was repeated.

The reviewer also noted that some injected disposable journal/path mutations are
restored on success, but not separately on failure before deleting the private root.
This was rejected with reviewer agreement: failure aborts the scenario, owned child
handles stop the foreground servers before root deletion, and no subsequent test or
retained evidence uses that failed fixture. There is no concrete ownership/cleanup
failure from this implementation difference.

All 74 inventory entries and both separately tracked embedded execution sites are
ported. No first-party `.py` source or executable Python caller remains; upstream
reference sources and explicit nonexecutable provenance archives remain unchanged.
Complete intended-diff review and the mandatory settled-source non-R6 gate still
block final milestone completion. R6 remains explicitly deferred.

### Final acceptance

Every mandatory non-R6 command now has passing evidence. The final gate exposed an
archived-workflow helper lookup, test lint violations and stale fixture teardown
expectations; confirmed issues were fixed and independently reviewed. The continuation
runner's stdin mistake was corrected without a production change. Affected checks and
remaining commands passed, while unchanged successful integrations and measurements
were reused. Exact commands, test counts, logs and dispositions are retained in
[final verification](headless-final-verification.md).

All 74 inventoried scripts and both embedded sites are complete. The original
headless H1–H6 milestone is accepted, with R6 explicitly deferred. No unresolved
non-R6 blocker remains. No repository commit, push or PR was made.
