# Real-process fixture policy

Use disposable HOME/XDG directories, owned endpoints, bounded calls and cleanup guards.
`tests/zor_integration.rs` runs the zor fixtures against explicitly supplied real binaries;
the combined gate sets `FUX_REQUIRE_ZOR_BIN=1`, so missing binaries fail.

## Journal contention

`tools/xtask/src/support/contention.rs` is shared by the Rust service, group, workflow,
dashboard, binding and producer fixtures. Only exit 1 with the exact Busy diagnostic, or a structured `task-busy`
reply with matching version, request ID and service incarnation, establishes contention.
Transport failures, changed identities, other exit codes and other errors are never retried.
Busy must not satisfy a negative test expecting a semantic rejection.

Each retry path explicitly permits reads or the same retained operation and unchanged plan:
task start/prepare/submit, worktree intent, named source/check/artifact collection, policy,
handoff, or exact producer claim/sequence. Keep the original operation ID, receipt and
deadline. The helper passes remaining time into each bounded call. Freshness checks retain
their shorter outer deadline and use the shared Busy predicates without starting a new loop.

`group-step` is a selection operation: its final inspection can return Busy after admission
commits. Attempt once, inspect admission, and resume only the intended existing prompt if
admitted. Concurrent callers each attempt once; after joining, inspect before proceeding.
Multiple callers may select the same admitted, undelivered operation; exact worker input logs
and retained receipts establish deduplication, not the number of selection responses.

`group-cancel` may advance one adapter-arm retirement per call. The scripted group fixture
permits retries only because its workers have no native adapter arms. The binding fixture's
native cancellation and retirement assertions keep explicit individual calls. Do not copy
that replay permission to groups with native arms or add new commands to allowlists without
checking their commit and retry behavior.

Rust `tools/xtask/src/support/contention.rs` tests inject exact/malformed Busy responses, an effect before a lost
selection result, transport loss and deadline exhaustion. It runs in the required integration
launcher alongside the real-process fixtures.

The Rust `empty-arguments` scenario (`tools/xtask/src/scenarios/mod.rs`) verifies that configured commands, `new` and `split` preserve empty
non-executable argv entries in actual shell arguments and retained final records. Empty
executable names and NUL-containing arguments are rejected. The managed-launch fixture
separately carries an empty argument through zor intent, creation, replay and reconciliation.

All local CLI scenarios now run in Rust, including the UTF-8 and terminal-input fixture
workers. Run them with `cargo run --manifest-path tools/xtask/Cargo.toml --locked -- scenario NAME PATH_TO_FUX`.
The normal Cargo test entry points also run these scenarios. The observer scenario takes
`PATH_TO_ZOR` as an additional argument; `zor-headless` also takes the separately built
`zor-argv-fixture` binary from `zor/tools/xtask`.
