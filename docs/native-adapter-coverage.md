# Codex native adapter acceptance coverage

Scope: production zor adapter paths, deterministic local fixtures and real-fux
managed-process scenarios. This is not live model-provider certification.

| Required case | Concrete coverage |
|---|---|
| Headless launch/discovery and literal correlation | `zor_native` starts the production wrapper in a real fux pane, checks retained worker/thread/storage, literal input receipt and native response ancestry; CLI and service routes are exercised. |
| Structured working, input-required, response | Protocol tests `duplicate_items_do_not_clear_blockers_or_regress_terminal_evidence`, `retained_blockers_must_match_the_native_turn_and_live_input_scope`, and response/history tests exercise the production reducer. Real-fux controls reconcile working state and completed/partial response distinctions. |
| Stale responses and changed identity | `stale_threads_turns_and_operations_cannot_bind_or_refresh`, `conflicting_identity_or_content_invalidates_without_partial_binding`, session identity/cwd/persistence rejection, and journal retirement/replacement tests. |
| Duplicates and partial frames | Duplicate protocol/journal/control regressions; `arbitrary_fragmentation_preserves_literal_text_and_coalesced_frames` and malformed/truncated/flooded frame bounds; actual-pipe transport tests. |
| Missing acknowledgement and event loss | `lost_ack_and_events_reconcile_from_native_identity_without_replay`, absent/partial/duplicate history tests, and real-fux service reconciliation after deliberately lost acknowledgement/events. Unsolicited history cannot refresh evidence. |
| Caller loss and retry | Durable prepare/take/reopen test commits submission before returning send authority, reopens Store and refuses another send. Protocol serialization/recovery preserves uncertainty. CLI caller exits after queueing; the independent managed worker continues. Partial native write timeout prohibits retry. |
| Interruption/recreation | Distinct stable native interrupt controls; acknowledgement is not completion. State and real-fux recreation tests preserve thread/storage, stop the old process before resume, reject replay, and permit controls after coordination cancellation. Expiry and partial-publication failure revoke authority. |
| Cleanup | Actual owned-pipe tests cover EOF, deadline, cancellation, stderr flood, failed handshakes and eventual reap. Real-fux scenarios verify child disappearance, including completion immediately followed by EOF and provider replacement. |
| Freshness and attention | Read-only timestamp expiry/backwards-clock/duplicate tests; dashboard tests cover native blocker, retirement, event gap, EOF and resync. Real-fux overview checks the replacement producer. Native state never verifies tasks. |
| Capability errors/limits | Account-independent capability/service tests, strict unknown-agent error, original-deadline and capacity tests, missing/materialized/redirected storage checks. README explicitly excludes lost-wrapper restart and answering native blockers. |

Current source verification: `cargo test --manifest-path zor/Cargo.toml --lib tasks::codex -- --nocapture`
passed **38 tests**, with **2 explicitly optional installed probes ignored** (8.09 s).
Recent real-fux native/dashboard runs and independent increment reviews are recorded
in [the milestone record](native-agent-milestone.md). These tests exercise production
logic at its relevant boundaries; a separate end-to-end fixture for every combination
is not an additional acceptance requirement.

Installed initialization and storage metadata were validated separately. Empty-thread
resume was rejected because the provider had not materialized a rollout. Live model
turns and materialized real-provider resume remain unverified and are not claimed.
Required checks use no account, paid calls or system permissions. R6 remains deferred.

N2 implementation/targeted verification, N4 native two-worker demonstration and N5
complete intended-diff review/final gate are complete. See
[the final verification record](native-final-verification.md) for the successful
fresh 45-command invocation and separately attributed provider limitations.
