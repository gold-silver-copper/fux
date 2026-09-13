# Retention as caller policy under fux-enforced ceilings

Follow-up to the third boundary audit on `main` `742bf1c` of
`https://github.com/gold-silver-copper/fux` (fux 0.9.0, zor 0.4.0, local-ipc 0.2.0, all still
unpublished). The boundary is settled; two retention details remain where fux hard-codes policy
that zor should choose, and one eviction is silent. Governing principle: fux keeps only what
requires owning the PTY, the process, the retained grid or the event log; fux enforces
ceilings, zor supplies policy under them.

Do the two items below as **one pull request** against `main`, branch `retention/caller-policy`,
from a fresh worktree; one commit per item, each building and passing the fast suites on its
own; push when the branch passes verification, open the PR, wait for hosted CI and report it.
Do not merge, do not publish. Keep the runtime of `/Users/kisaczka/Desktop/code/fux` unchanged.

Rules: no compatibility shims (no optional-with-old-default-forever fields kept "for
compatibility"; a changed request shape is changed everywhere in the same PR, and the consumer
fixture `crates/fux/tests/fixtures/control-consumers.json` is updated so `protocol_consumers`
passes); no single build, test run or CI wait over five minutes; no batch campaigns; do not run
the full 45-command headless gate (leave its command in the report); per-crate lints stay;
regenerate the boundary inventory only after confirming fux declaration changes are generic;
the `wrap` feature stays a zor default and fux keeps building zor with `--no-default-features
--features cli`. Use one independent subagent to review the complete diff for semantic drift
in receipt and final-record semantics and for any leftover compatibility path; fix confirmed
findings before pushing.

## 1. Caller-chosen retention under fux ceilings

Today `crates/fux/src/ecs/resources.rs` fixes `INPUT_RETENTION_MS = 60_000` and
`FINAL_RETENTION_MS = 60_000` alongside the caps `MAX_INPUT_OPERATIONS = 128` and
`MAX_FINAL_RECORDS = 128`. The caps stay in fux (they bound server memory against any client).
The durations become caller policy:

- **Input reservations**: `input-reserve` gains a required `retain_ms` (u64). fux clamps it to
  a ceiling constant `MAX_INPUT_RETENTION_MS` (choose a value and justify it in the changelog;
  10 minutes is a reasonable ceiling) and rejects `0`. The receipt's `expires_ms` reflects the
  clamped value. Every fux test and fixture that reserves input passes an explicit value; zor's
  `tasks/submit.rs` chooses its own value (read how long zor actually needs receipts: the
  reconcile/retry windows in `tasks/submit.rs`, `tasks/wait.rs` and `service_tasks.rs`) and
  states the reason in a comment.
- **Final records**: the retention of a pane's final record becomes a property the launcher
  sets on the pane, because the record is created at exit when no client is present. Add a
  required `final_retain_ms` (u64) to `split` (the only way zor creates panes) and to whatever
  fux uses to create its own initial pane on `workspace new`/`resolve`/manager `create` (fux's
  own CLI passes a documented default; decide whether the default belongs in `Config` and say
  why). fux clamps to `MAX_FINAL_RETENTION_MS` (again choose and justify; several hours is
  reasonable, since a supervisor may reconnect later) and rejects `0`. Store it on the pane
  component and use it in `final_records.rs` when the record is created. zor's `tasks/launch.rs`
  and `run.rs` pass values that match their actual recovery windows (read `tasks/wait.rs` and
  `run.rs` to find how long they may poll) and comment the reason.
- Delete the two old constants. Update `docs/local-control-protocol.md` (request fields,
  ceilings, clamping, the meaning of `expires_ms`), `docs/design.md` (bounds table: cap in
  fux, duration from the caller), `docs/security.md` if it lists the bounds, the protocol
  fixtures, the consumer fixture (new fields with their zor consumers), and `CHANGELOG.md`.
- Tests: ECS tests that a reservation and a final record expire at the caller's value, that a
  value above the ceiling is clamped (observable through `expires_ms` and through expiry
  timing), that `0` is rejected as `invalid-request`; a local-CLI scenario step for the new
  `split` field; zor tests that its chosen values are within fux's ceilings (read the ceiling
  from `fux info`'s limits, which must now publish both ceilings; that makes those limits a
  real consumer for the first time, so record zor as their consumer in the fixture).

## 2. Explicit eviction on the `final` reply

`crates/fux/src/ecs/systems/final_records.rs` evicts the oldest-closed record when the cap is
reached, and `read` then answers `expired` ("final evidence unknown, evicted, or expired") for
a pane that may have existed. Make the outcomes distinguishable without retaining more data:
fux keeps, per server instance, a bounded ring of the pane ids whose records were evicted
before their `expires_ms` (cap it, for example 1024 ids; state the cap), and the `final`
request answers with distinct error codes: `expired` when the record existed and its retention
elapsed (fux can know this only while the id is still in the ring or the record is still
present; decide the exact rule and document it), `evicted` when the id is in the eviction ring,
and `unknown` otherwise. Keep `pending` and `conflict` as they are. Add the new code(s) to
`ErrorCode`, the protocol doc, fixtures and the consumer fixture. zor (`tasks/wait.rs`,
`tasks/launch.rs`, `run.rs`) handles `evicted` explicitly: it is a hard failure with a message
that says the server dropped the record under load, not a retry. Tests: an ECS test that fills
the cap, checks the evicted id answers `evicted`, an expired one answers `expired`, and an id
never seen answers `unknown`; the ring bound itself.

## Verification and reporting

Run within the time budget: `cargo fmt --all --check`; strict `cargo clippy --workspace
--all-targets -- -D warnings` and zor with `--no-default-features --features cli`; fux
lib/ECS/fixtures/structure/boundary/protocol_consumers suites; all local CLI scenarios; zor lib
tests; the automation scenarios that touch receipts and final records (`input-receipts`,
`final-records`, `zor-run`, `zor-launch`, `zor-tasks`, `zor-recovery`, `zor-check-workers`;
split into runs under five minutes); tooling tests; the release-package check. Bump fux to
0.10.0 and zor to 0.5.0 (breaking request shapes); local-ipc unchanged. Write
`.verification/retention-policy/REPORT.md` with: the PR URL and CI result; the chosen
ceilings and zor's chosen values with their reasons; per item the commit and lines per crate;
the reviewer's disposition; deviations from this prompt; and the gate command:

```sh
cargo run --locked --manifest-path tools/xtask/Cargo.toml -- dependencies verify --build --headless
```
