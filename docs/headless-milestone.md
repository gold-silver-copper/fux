# Headless architecture, performance and capability milestone

Authority: [execution prompt](../headless-architecture-performance-prompt.md).
This is a new milestone; the earlier herdr-gap objective is complete with R6 deferred.
Fux owns terminal primitives, zor agent policy, and koh opaque authenticated transport.
No live remote acceptance, system permission changes or account-dependent required checks.

| ID | Confirmed starting point | Acceptance and stopping condition |
|---|---|---|
| H1 — gate | The reconstructed gate mixes passing local checks with deferred endpoint/network tests. | One mandatory headless entry point reconstructs companions and runs all existing non-R6 checks plus this milestone's regressions. Exclusions name only deferred R6 runtime cases; missing executables/evidence fail. Existing full remote gate remains separately available. |
| H2 — deterministic boundaries | Fux already separates ECS decisions/effects. Koh framing is generic but its session stores concrete Unix halves. Zor submission and subprocess deadline handling call OS boundaries directly. | Preserve fux's ownership. Introduce only the narrow production seams needed to test relevant I/O, clock and effect decisions; deterministic tests exercise the actual implementation under partial writes, backpressure, interruption, cancellation and stale identities. Document coverage limits. |
| H3 — measured performance | Fux builds frames per dirty viewer; zor reloads and rewrites its bounded journal; koh copies/buffers frames. Existing paired benchmarks used different geometry and do not isolate these costs. | Retain equal-geometry, pinned-build headless baseline/after runs for idle, sustained/burst output, multiple panes/viewers, slow consumers and journal/observation activity. Measure relevant bytes, CPU, latency and high-water marks with unprivileged instrumentation. Implement demonstrated improvements, preserve semantics, disclose regressions and unavailable measurements. Record byte/item/concurrency/deadline bounds for affected resources. |
| H4 — adapter capabilities | OpenCode has native integration and explicit recreation; Claude/Codex have recorded passive coverage but no equivalent native contract. | A small explicit capability CLI/API describes launch/discovery, correlation, state/response, interrupt and native recreation with reasons for unsupported operations. Inspect installed Claude/Codex interfaces and implement supported headless capabilities incrementally; fixtures do not count as live provider proof. All agent-specific behavior stays in zor. |
| H5 — workflow | Existing headless two-worker verified handoff and recovery fixtures pass. | Extend/reuse the real CLI/API workflow to demonstrate the new capabilities with blocker/interruption, safe reconciliation, verification and retained handoff. Operation IDs, structured errors, inspection, retry semantics and bounded event-loss behavior are explicit. Mandatory run is account-independent. |
| H6 — review and delivery | Prior full-diff reviews resolved five findings; their unchanged evidence remains valid. | Review the complete intended diff and new changes independently; validate/fix confirmed in-scope defects. Refresh invalidated evidence and companion patches, run final mandatory non-R6 headless gate on settled source, and provide exact results. |

Initial inspection: no new correctness defect is inferred from the performance cost centers.
Investigate before optimizing. Baseline fux/zor binaries and selected source hashes are preserved
at `/tmp/fux-headless-baseline/provenance.json` before production changes. No benchmark numbers
are claimed from this preservation step. Current source and retained tests are authoritative.

Stop when H1–H6 pass. No new UI, plugin system, general restoration, multiplexer feature parity
or live remote acceptance is part of this milestone. Optional paid calls are authorized but are
not prerequisites. R6 remote outcomes remain unverified and explicitly deferred.

## Final acceptance (2026-09-07)

H1–H6 are accepted within the non-R6 scope. All 74 first-party Python files and both
embedded execution sites are Rust; all mandatory checks passed using the recorded
successful gate prefix and targeted continuations after confirmed test fixes. Complete
root/companion reviews and final-delta reviews are accepted. Companion patches were
exported and byte-for-byte reconstruction verified. No unresolved non-R6 blocker remains.
Exact counts, commands, failures/fixes, review dispositions and limitations are in
[final verification](headless-final-verification.md). This final record supersedes
pending-status statements in the chronological execution history below.

| Criterion | Verified outcome |
|---|---|
| H1 | Mandatory Rust headless entry point retains every non-R6 command and names only deferred remote exclusions. |
| H2 | Production subprocess/identity and generic transport boundaries tested; ownership remains fux multiplexer, zor agent policy, koh opaque transport. |
| H3 | Full paired performance and journal/deterministic transport evidence retained, including regressions and unavailable measurements; no universal speedup claim. |
| H4 | CLI/API capability contract and bounded literal headless launch policy verified; unavailable native operations remain explicit. |
| H5 | Real local two-worker blocker/repair/reconciliation/verified-handoff workflow passed without accounts. |
| H6 | Migration, complete independent review, corrected companion reconstruction, every required non-R6 check and packaging passed. |

## Execution record

- H1 implementation: `cargo run --manifest-path tools/xtask/Cargo.toml --locked -- dependencies verify --build --headless` is the
  mandatory entry point. It preserves the full profile separately, retains every prior local
  command, explicitly names the three deferred network library cases and network integration
  suite, and includes owner checks, root binary tests, startup/native-event validators and
  packaging. Independent review found two missing test groups; both were added and accepted.
  Three gate-policy tests, two startup and eight native-event tests pass. Full settled-source
  run is pending; this is not final gate acceptance.
- H2 koh: the session machine accepts generic local stream halves while production still
  uses owned Unix halves through `into_split`. Bounded in-memory streams now force partial
  application writes, deadline expiry under paused Tokio time, incomplete framing and owned
  cancellation cleanup. Nine session tests and strict all-target clippy pass; independent
  reviewer reran all nine and accepted the change. No real endpoints or R6 tests ran.
- H4 inspection: installed Codex0.153.4 exposes a generated app-server JSON schema; Claude
  exposes print/stream-json/session options. Help/version/schema extraction used private
  HOME/XDG directories and no accounts. Raw inspection artifacts are in
  `/tmp/headless-agent-interfaces`; these establish interface availability, not implemented
  zor adapter capabilities or live provider success. Codex protocol reference:
  https://learn.chatgpt.com/docs/app-server (fetched2026-09-07).
- H3 measurement tooling is now Rust in `tools/xtask/src/headless_performance.rs`. It uses equal80x24
  viewer geometry, synthetic workers, multiple panes/viewers and a deliberately slow reader;
  it records visible-output timing, wire bytes/frames and owned-process CPU/RSS when available.
  No new performance conclusion is established until validated baseline/after results exist.
- H2 zor: the bounded subprocess runner moved from Git policy to `platform::process`;
  check execution and Git policy use the same production runner and original caller deadline.
  Target reply identity validation is now pure. Independent review ran all 88 library tests;
  the confirmed unused-import lint issue was fixed and strict all-target/all-feature clippy
  passed. Later capability additions have separate targeted checks pending.
- H3 fux: immutable pane views are shared only within one publication pass. Independent
  review accepted the production change and reran the new ECS regression. Retained 12-case
  before/after matrices and a limitations report are in [headless-performance](headless-performance/README.md).
  Three offline integrity tests pass, including matrix, identity, cleanup, build-command and
  candidate-source checks. Reviewer-found pairing/build validation gaps were fixed. This is
  not acceptance of the still-outstanding journal and deterministic koh cost-center work.
- H4 contract increment: `zor task adapter-capabilities --agent opencode|claude|codex`
  and service task action `adapter-capabilities` report implemented guarantees without
  provider or journal access. Unknown agents fail through the ordinary request error path.
  Review corrected recreation wording: stop intent prevents resume. Claude/Codex native
  correlation/recreation and all native interrupt operations remain explicitly unavailable;
  native adapter enhancements are still outstanding. No paid model calls were made.
- H5 workflow fixture now checks CLI/API capability agreement and read-only behavior before
  its existing two-worker blocked-check/repair/verified-handoff path. Its affected run is
  pending; fixture reports remain attributed synthetic evidence, not live model coverage.

## Current continuation status

- The original performance matrix, journal baseline and deterministic koh framing/window
  measurements now exist in `headless-performance/`. The journal fixture recorded read/replay
  behavior separately from adopted-task replacements. The deterministic koh test exercises
  actual production framing and window bounds without remote endpoints. These are retained
  measurements, not a claim of universal performance improvement.
- Zor implements the explicit capability contract and single-prompt headless launch policy
  for Claude/Codex. The real CLI/API test uses scripted provider executables and proves exact
  noninteractive argv, literal prompts, stable launch replay and no inferred task success.
  Native correlation/recreation remains unavailable for those two agents. No live provider
  success is claimed and no paid model call was made.
- User scope now includes every first-party Python script. See
  [migration inventory](python-rust-migration.md): all 74 files and two embedded execution
  sites are ported. Both standalone tooling owners are mandatory in the Rust gate. Local
  CLI scenarios and the migrated fux/zor integrations passed their targeted Cargo entries
  and independent review. All original required checks remain enabled with Rust callers.

Final disposition: all migration, evidence reconciliation, companion reconstruction,
complete review and mandatory non-R6 verification work is accepted in the final record.
R6 remains explicitly deferred and no remote runtime claim is added.

Latest migration increment (2026-09-07): Rust lifecycle/startup capture tooling and offline
Codex/Claude evidence validation pass on Rust 1.91; three authenticated capture ports pass
against scripted terminals using dummy credentials only. Historical Python capture sources
are archival `.py.txt` for hash provenance. Local recovery now runs from its normal Cargo
entry in Rust and preserves durable stop, uncertainty/loss, history and no-replay coverage.
Independent reviews accepted the ports. See the migration record for commands, limitations
and the remaining inventoried files; this is not final milestone acceptance.

Dashboard and two-worker workflow ports are now accepted by independent review and pass
through their normal Cargo entries with real local fux/zor. Rust fixtures replace the
embedded Python notifier, worker and check. This also closes the earlier pending H5
capability CLI/API/read-only workflow check. Thirty-four inventoried Python files remain;
no full milestone/final gate claim is made.

The offline workflow comparison validator is also Rust. Historical fixture sources are
archived byte-for-byte and the retained evidence is unchanged; new-source provenance has
a separate regression. Both gate profiles retain this requirement.

The detection inventory is now Rust, with exact data/Markdown parity and reviewed PATH/alias
edge cases. It performs no agent execution; historical inventory attribution remains intact.

Traffic/setup/resource comparison validators now run in Rust in both gates and CI, with
all original mutations and exact provenance/accounting checks. Retained measurements are
unchanged. Independent final review accepted the ports; 23 tooling tests and strict lint pass.

Both detection validators are now Rust and independently accepted. Historical screen
evidence main.rs attribution is reconciled to exact archived bytes, without relabeling a
measurement. All 25 tooling tests and strict lint pass; all comparison-validator CI commands
use Rust. The broader remaining migration and final milestone gate are still pending.

Retained native/zor resume validators are now Rust with all original negative mutations
and reviewed missing-field regressions. Eight zor tooling tests pass on Rust 1.91 with
strict lint. No live resume or deferred remote runtime check was run.

The native-event validator is now Rust with exact outcome equality and all 12 original
mutations. Required gate wiring and independent final review pass. Nine zor tooling tests
pass on Rust1.91 with strict lint; no provider/remote runtime acceptance is implied.
