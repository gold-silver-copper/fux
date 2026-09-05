# Fux contextual-help handoff

The user asked to wrap up, commit/push all work, and leave a resumable checkpoint.
The original goal is `execute contextual-help-prompt.md`. It is **not yet declared complete**.
Do not continue implementation merely to finish this session; resume when requested.

## What is implemented

- Fux builds and runs as a standalone local multiplexer without koh/zor dependencies or keys.
  Persistent PTYs remain in a local session process. Optional networking belongs to koh;
  optional observation runs through zor's separate process/protocol.
- Configured prefix enters viewer-local command mode; hints appear after 200 ms without pane
  output. Fast shortcuts execute immediately. Explicit help, unknown-key discovery, Escape,
  literal prefix, grouping, availability and custom bindings use the shared command registry.
- Integrated workspace/tab pickers, grapheme-aware rename, target-specific close confirmation,
  repeatable resize with a thin hint bar, private copy/scrollback/selection and OSC52 clipboard.
- Hints can be immediate or automatic hints hidden. Popup hints preserve application input.
  Private modes do not hijack another viewer. Small screens paginate; terminal resize repaints.
- Attachment protocol v2 distinguishes raw pane bytes, typed commands, external bindings, mouse
  and private copy requests. Old servers are rejected before raw terminal mode, without killing
  sessions. Stable pane/tab IDs protect delayed actions from targeting replacement objects.
- Review fixes include exact literal-prefix forwarding, bounded fragmented CSI/SS3/paste handling,
  copy-limit feedback with retry, selection highlight clipping, long rename input visibility,
  buffer invalidation on resize, and stopping at detach while draining preceding ordinary input.

## Remaining work, in order

1. Finish an explicit separate review of the complete intended diff and all new files. Review has
   covered the input bridge, interaction controller, hints, command registry and local framing/
   dispatch, but a complete final pass across the whole checkpoint is not finished. No subagents
   were used. Distinguish the earlier standalone refactor from contextual-help changes without
   dropping either. Starting fux commit was `c814a4a`.
2. Validate and fix confirmed findings, then rerun affected tests and review the fixes. Pay attention
   to per-viewer mode transitions, ordered input/acknowledgements, canceled loading/paste state,
   resource bounds, stale targets, error visibility, optional integration contracts and documentation.
3. Create a requirement-by-requirement acceptance audit against the original prompt. Use actual
   source and runtime evidence for every requirement; do not infer completion from green tests.
4. Run final relevant checks after any edits and report remaining platform/CI limits accurately.
   No PR was requested or opened; GitHub CI is not an acceptance claim in this checkpoint.
5. Only mark the contextual-help goal complete after the audit proves it. Then explain the final
   flow, defaults, tests and limitations to the user.

## Verification evidence

Commands run locally on macOS; `/tmp` logs are convenient local evidence, not portable artifacts.

- `cargo test --locked --test client --test local_cli`: latest UI pass has 41 client tests and five
  isolated CLI scenarios (`/tmp/fux-contextual-review-ui-tests.log`).
- Latest detach regression passes, including preceding input delivery and suppression of trailing
  commands (`/tmp/fux-review-detach-drain-test.log`).
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed for latest production
  changes (`/tmp/fux-handoff-clippy.log`). Formatting and `git diff --check` pass.
- `cargo test --locked --all-features`: passed on the final production checkpoint
  (`/tmp/fux-handoff-full-tests.log`).
- Fixture-child full suite passed after migrating binary copy observation and workspace navigation
  (`/tmp/fux-private-copy-fixture-tests.log`). Root corpus/structure oracle tests passed
  (`/tmp/fux-private-copy-oracles.log`). Fixture-child strict Clippy passed.
- Earlier optional koh gateway tests and five-loss reconnect test passed with explicit FUX_BIN and
  KOH_REQUIRE_FUX_BIN=1 (`/tmp/fux-protocol2-final-gateway.log`,
  `/tmp/fux-protocol2-final-reconnect.log`); these were not skipped.
- Rustdoc passed (`/tmp/fux-contextual-edge-doc.log`). Dependency reconstruction was reverified
  after pinning the newly committed owner snapshots. Cargo tree previously confirmed no koh/zor
  packages in fux's all-features build (`/tmp/fux-protocol2-tree.log`).

Useful repeat commands:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo doc --no-deps --all-features --locked
cargo build --locked --bin fux
cargo clippy --manifest-path tests/verify/fixture-child/Cargo.toml --locked -- -D warnings
cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked
python3 tools/dependencies.py verify
```

## Repository and documentation notes

All three owner repositories are checkpointed on main. `dependency-patches/manifest.json` is the
source of truth for koh/zor commit IDs; CI pins match it. Patches are now empty because their
contents are committed in the owner repositories. The reconstruction tool remains available for
future local integration edits. Do not blindly reapply old patches or reset personal sessions.

Read README's contextual hints section, `docs/local-attachment-protocol.md`, and
`docs/contextual-help-plan.md`. The plan is chronological: early incomplete statements describe
older stages, not the final implementation. Standalone audit/release documents cover a broader
refactor and must not be mistaken for a completed contextual-help acceptance audit.

No personal sessions were restarted, keys cleared, or user workspaces killed. Tests own temporary
runtime/config directories and clean up their own processes. No release, PR, or review comment
was published. This is a checkpoint, not a release-ready certification.
