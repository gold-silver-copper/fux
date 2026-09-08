# Native milestone final verification

Status: **complete within the required non-R6 scope**, 2026-09-08. N1–N5 are
verified in [the milestone checklist](native-agent-milestone.md). The final fresh
invocation exited 0; `.verification/gate-tbzpPi/manifest.json` records all 45
commands passed on their first attempt, `complete: true`, and a finalized checkpoint.

The final intended change set includes root staged/unstaged/untracked first-party
changes plus zor and koh companion changes at their pinned manifest bases. Existing
user changes are preserved. No commits, pushes or PR operations are authorized or
performed. Independent final reviews are partitioned across root runtime/gate/tooling,
zor runtime/tooling, and koh plus workflow/performance/evidence.

Preflight: `cargo run --manifest-path tools/xtask/Cargo.toml --locked -- dependencies verify`
passed for both refreshed companion patches and exact reconstructed source comparison.
The authoritative mandatory headless plan is `tools/xtask/checks.json`: 45 commands.
The successful final invocation was:

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- dependencies verify --build --headless
```

It used a fresh manifest, not a prior continuation. Its durable gate directory
retains reconstructed source, source inventory/fingerprint, command plan, configuration,
toolchain identity and per-command logs/results. No running gate was found during
preflight. Existing targeted and historical evidence is reused only within its stated
scope; it does not substitute for the required final end-to-end invocation.

R6 remains explicitly deferred: live remote authorization, reconnect, retention-window
acceptance and netmon runtime behavior are not verified by this milestone. The three
listed remote runtime tests remain excluded; all other mandatory headless checks remain
enabled. Deterministic koh results are production-logic coverage, not remote proof.

Provider validation is separately bounded: installed Codex initialization/storage
metadata was checked; live model turns and materialized real-provider resume remain
unverified. Account-free fixtures are labeled as such. No paid model calls were used.

Final review dispositions:

- Koh/evidence reviewer inspected the complete koh diff at manifest base, root native
  and workflow integration/fixtures, mandatory registration, retained artifact handoff
  and all paired performance evidence. Patch identity, all six binary/archive hashes,
  viewer medians and exact handoff extraction match. No confirmed actionable defects.
- Zor reviewer inspected the companion change at pinned base, including new runtime,
  fixtures/tooling and documentation. Detailed native identity, durable send authority,
  recreation deadlines, storage, process cleanup, reservations, freshness/attention and
  capability review found no defect. Cross-checked launch/cancellation/check/source/artifact
  interactions and reused earlier complete reviews for unchanged surfaces. Recreation
  after cancellation, native task-success inference, record forgetting and lost-wrapper
  support were checked and rejected as findings for the documented reasons.
- Root reviewer inspected the complete production diff and new ECS modules, generic
  receipt/event/final-record/lifecycle/terminal changes, N1 actual runner and evidence
  associations, reconstruction, mandatory plan and regressions. Migration inventory,
  dispatch, shared cleanup/socket helpers and provenance routing were checked, reusing
  documented original-to-Rust reviews for unchanged scenario/capture bodies. No confirmed
  actionable findings.

All three final reviews were read-only. They did not rerun tests or benchmarks and do
not replace the required fresh final gate. No in-scope review fix remains outstanding.

First fresh invocation: `.verification/gate-bZdJq6`. Checks 0–5 passed; check 6
failed three tooling regressions and the invocation exited nonzero after retaining
its checkpoint. The failures were:

- A disposable Git reconstruction fixture inherited account commit signing. Its
  commit now explicitly disables signing without changing user configuration.
- The historical pane-sharing validator expected the pre-probe `src/view.rs` hash.
  Exact original source was recovered from the preserved N3 baseline archive and
  pinned under `tools/archive/native-milestone/view.rs.txt`. The original measurement
  hash is unchanged; only that historical path/hash pair selects the verified archive.
- Controller-setup provenance was invalidated by N2 dashboard changes. A fresh Rust
  capture passed all six zor/herdr cases. The previous JSON remains unchanged in
  `docs/comparisons/history/`; the active report and prose now reflect the new run.

The Git fixture regression and six retained/negative evidence tests pass after fixes;
strict all-target tooling Clippy passes. Root review accepted the signing/archive fixes.
Fresh-capture review verified current source/binary hashes, pinned herdr provenance,
all six clean/no-input runs and updated counts, with no confirmed defect.
These source/evidence changes require another fresh
final invocation; the failed run is retained and will not be presented as passing.

Second fresh invocation: `.verification/gate-GATZ40`. Checks 0–26 passed; check 27
failed the structural invariant prohibiting ignored tests because the temporary N3
measurement probe remained in `src/view.rs`. The invocation exited 1 after retaining
its checkpoint. The complete probe is now archived as
`tools/archive/native-milestone/view-cost-probe.rs.txt`; removing only that probe
restores `src/view.rs` byte-for-byte to the preserved baseline. The structural check
is unchanged. Candidate archives, binaries and raw measurements remain unchanged.

All eight structural regressions pass after removal. Root formatting, diff whitespace
and strict library/structure Clippy checks pass. Independent review confirmed the
archived probe exactly matches the candidate archive and the source restoration is
exact; all retained candidate/archive/executable hashes remain valid. No confirmed
finding remains. The third fresh invocation below used this settled source.

## Successful final invocation and completion audit

Authoritative record: `.verification/gate-tbzpPi/manifest.json`, with reconstructed
source and per-command `000-000` through `044-000` diagnostic directories beside it.
Source fingerprint: `c9a36551a5f697d27a1482e132bf57f94836c9b65e5f5693f8cc0006a409627e`.
Final checkpoint: `4d42c1c7108408802d5f0db825d35ec283b182887b27fbafdd5f207ec7320b7d`.
No source changed during this invocation. Only completion documentation was updated
after exit; the retained source snapshot records the exact tested inputs.

Independent evidence review compared all 45 recorded commands and exclusions with
`tools/xtask/checks.json`, verified each single-attempt exit-0 outcome and all 45
diagnostic-log hashes, and confirmed successful package verification. The runner
subsequently finalized its checkpoint, marked the manifest complete and exited 0.

| Requirement | Final evidence |
|---|---|
| N1 reproducible gate and continuation | Check 6: 20 tooling library tests and 27 tooling binary tests pass, including actual stdin isolation, interruption, deadlines/output bounds, cleanup, failed-check retry and invalid-evidence rejection. Durable final manifest retains exact command plan, toolchain/configuration, source identity and logs. |
| N2 native adapter in zor | Recorded installed-interface selection and [coverage map](native-adapter-coverage.md); check 27 passes real-fux native integration, and check 39 passes 131 zor library tests plus CLI suites. The two ignored installed probes are explicitly optional; their separately recorded metadata evidence is not live-turn proof. |
| N3 measured opportunity | [Paired decision](native-performance-decision.md), preserved baseline/candidate source and binaries, raw viewer/journal/koh records, and independently verified measurements. Candidate rejected for mixed results; production validation restored. Check 28 passes the retained performance validator. |
| N4 unattended workflow | Check 27 passes all 21 zor integration tests, including the extended two-native-worker workflow. Earlier unchanged standalone stdout and exact verified-artifact handoff remain in `.verification/native-workflow-20260908/`. |
| N5 full review and final verification | Three independent complete-diff reviews and accepted final fixes above; both companion patches reconstruct; all 45 mandatory headless commands pass in one fresh invocation. |

The gate also passes all required formatting and strict lint checks, root integration
suites (3 boundary, 31 ECS, 11 local CLI, 8 structure), root library/binary tests
(84 and 3), documentation build, fixture checks, zor no-default-features build,
12 deterministic koh gateway tests, all retained-evidence validators and package
creation/verification. No non-R6 check was removed or waived. No new benchmark run
was needed after the final test-only removal.

No unresolved non-R6 blocker remains. Required validation was headless and used
ordinary permissions without accounts or paid calls. Live model turns and materialized
real-provider resume remain unverified; lost-wrapper restart and answering native
blockers remain unsupported. R6 live authorization, reconnect, retention-window and
netmon runtime work remains explicitly deferred, including the network gateway
integration suite. These limits are not claimed as completed capabilities.
No commits, pushes, PRs or CI mutations were performed; remote CI is outside this task.
