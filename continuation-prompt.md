Continue the fux contextual-help project from its committed checkpoint. Complete the remaining review, acceptance audit, fixes, and verification.

Read AGENTS.md, HANDOFF.md, and contextual-help-prompt.md first. The original requirements in contextual-help-prompt.md remain the acceptance criteria. Also read README’s contextual hints section, docs/local-attachment-protocol.md, and docs/contextual-help-plan.md. Treat the plan as chronological history, not proof of completion.

All three repositories were reported pushed and clean; verify their current state and preserve unrelated changes. Use dependency-patches/manifest.json to identify the koh/zor owner snapshots. Do not reapply old patches.

Proceed autonomously:

1. Establish the complete review scope, starting from fux commit c814a4a. Include all new files, relevant koh/zor changes, and any current staged, unstaged, or untracked work. Distinguish the standalone refactor from contextual-help changes while reviewing both.

2. Have an independent subagent review the complete change set. Review input routing, viewer-local state and transitions, ordered input/acknowledgements, timer cancellation, fragmented sequences and paste, stale pane/tab targets, resource bounds, error visibility, rendering, and optional integration contracts. If independent review is unavailable, perform an explicit separate full-diff review pass.

3. Validate findings against current code. Fix confirmed in-scope defects, add meaningful regression coverage where needed, rerun affected checks, and independently review the fixes. Document any findings rejected or deferred and why.

4. Write a requirement-by-requirement contextual-help acceptance audit. Map every original requirement to source and test or runtime evidence. Clearly distinguish verified behavior, defects, and unverified platform behavior. Passing tests alone do not establish acceptance.

5. Run final relevant verification:
   - cargo fmt --all --check
   - cargo clippy --locked --all-targets --all-features -- -D warnings
   - cargo test --locked --all-features
   - cargo doc --no-deps --all-features --locked
   - cargo build --locked --bin fux
   - cargo clippy --manifest-path tests/verify/fixture-child/Cargo.toml --locked -- -D warnings
   - cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked
   - python3 tools/dependencies.py verify
   - git diff --check

   Include relevant isolated real-binary, fixture/oracle, and koh gateway/reconnect scenarios. Ensure integration checks actually execute rather than silently skip. Verify the standalone all-features dependency tree excludes koh and zor.

6. Update the handoff and completion documentation to reflect the actual final state. Report interaction behavior, configuration defaults, review scope and findings, verification commands/results, and remaining limitations. Separate macOS evidence from Linux/Android runtime verification and GitHub CI status.

Use isolated temporary runtime/config directories. Do not restart or modify personal sessions, clear keys, or kill user workspaces. Do not commit, push, create a PR, publish, or comment on GitHub unless separately requested.

Continue until review and acceptance are complete or a concrete blocker prevents further progress. Do not claim completion while requirements remain unverified or confirmed in-scope defects remain unresolved. If blocked, state the exact blocker and remaining work.
