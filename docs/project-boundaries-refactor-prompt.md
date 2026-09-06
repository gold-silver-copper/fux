> Historical record from before the 0.3.0 bevy_ecs rewrite (2026-09-05). The host, popup panes, sidecar supervision, protocol v2/`FUXCTL1` and verification results described here no longer exist. Current architecture: [design.md](design.md); current evidence: [ecs-acceptance.md](ecs-acceptance.md).

# koh, fux, and zor ownership refactor

Refactor koh, fux, and zor into independently useful projects with clear ownership and reliable integration. Implement the changes—not just an architectural proposal.

Target ownership:

- koh is the sole owner of networking and remote connections: transports, endpoint identities, credential storage and prompting, authentication, admission, connection setup, reconnects, and transport-session lifecycle.
- fux is only the terminal multiplexer: workspaces, panes, tabs, layouts, focus, pane PTYs and child processes, multiplexed terminal state, rendering, and workspace commands.
- zor is only the agent-observation layer: observing agent activity, interpreting observations, and publishing documented agent-state reports.

Backwards compatibility and breaking semver are not concerns. Favor clear boundaries over preserving existing APIs, but preserve useful behavior unless there is a concrete reason to change it.

Begin by inspecting all three repositories, their instructions, dependency relationships, and current integration paths. Establish a concise ownership map based on the actual code, then implement the refactor incrementally.

## Architectural requirements

1. koh must expose a complete embedding API.
   - fux must not import iroh or implement network endpoint setup, admission, reconnect logic, network key management, or credential prompting.
   - Application consumers provide their protocol/state/input integration through transport-independent interfaces.
   - koh must support fux without requiring fux to reproduce koh CLI internals.
   - koh owns connection cleanup; fux owns application and terminal cleanup. Make their cancellation contract explicit.

2. Keep transport sessions separate from workspaces.
   - koh owns connected participants and resumable transport sessions.
   - fux owns workspace lifetime, including persistence after viewers disconnect.
   - fux expresses application authorization requirements through koh’s API; koh authenticates peers and enforces admission.
   - A network disconnect must not implicitly destroy a workspace or its panes.

3. Put process and terminal responsibilities in the correct layer.
   - fux owns pane PTYs, spawning, resizing, closing, and reaping pane processes.
   - Establish one owner for each terminal mode, input reader, signal handler, and background task.
   - Generic terminal support may be shared through a deliberate interface, but networking code must not take over application terminal I/O implicitly.
   - zor wrappers must preserve terminal behavior, signals, exit status, and process cleanup without taking over multiplexer policy.

4. Make zor integration a small, explicit contract.
   - Publish versioned, bounded observation reports with documented semantics.
   - fux may display reports but must not contain agent-specific detection or interpretation logic.
   - Distinguish observed or self-reported state from authenticated facts.
   - Malformed reports, unavailable zor, and observer failures must not disrupt normal pane operation.
   - zor must remain useful without fux or koh.

5. Give fux one command model.
   - Route keyboard, mouse, CLI, and control-socket operations through a common typed application-command layer where appropriate.
   - Define keybindings and their help text in one authoritative registry.
   - Avoid duplicated defaults and client-side shortcuts that silently bypass configurable bindings.
   - Keep workspace mutations ordered and resource ownership explicit; reduce shared mutable state where this materially improves correctness.

6. Preserve and correctly relocate the recent key-management fixes.
   - Unlock each required identity at most once per invocation/session scope.
   - Reuse unlocked identities across attachment, reconnects, and workspace switching.
   - Complete credential prompting before competing terminal input readers start.
   - Failed or cancelled credential loading must restore terminal settings and leave no orphaned startup processes.
   - Preserve encrypted storage and separate identities where required.
   - Do not introduce plaintext keys, persistent passphrase environment settings, or secret-bearing logs and command arguments.
   - Credential inspection, passphrase changes, and identity reset belong to koh. If fux exposes convenience commands, they must be thin delegates.
   - Reset must have explicit scope, explain identity/allowlist consequences, and handle active users safely.

7. Keep every project independently buildable and useful.
   - koh must not depend on fux or zor.
   - zor must not depend on fux or koh.
   - fux consumes narrow public interfaces rather than implementation details.
   - Avoid circular dependencies, unnecessary shared-framework projects, and abstractions without concrete consumers.
   - Separate repositories do not require separate processes for every interaction.

8. Make builds reproducible.
   - Inspect ignored local dependencies such as fux’s references/koh and zor directories.
   - Do not leave required changes hidden in ignored checkouts.
   - Make source changes visible in their owning repositories and provide a checked-in, reproducible way to assemble and test the exact combined development state.
   - Update relevant manifests, lockfiles, CI setup, and development instructions.
   - Do not invent unpublished release versions or nonexistent commit references.
   - Do not publish packages to make the local refactor work.

Prefer the smallest coherent refactor that achieves these ownership boundaries. Retain working state, layout, rendering, and observation code where possible. Do not rewrite functioning subsystems merely for stylistic uniformity.

## Validation

- Add focused architectural checks that prevent forbidden dependencies and ownership regressions.
- Test public contracts across projects, not only private implementations.
- Cover cold startup, attachment to an existing workspace, simultaneous startup, workspace switching, remote admission and rejection, connection loss and reconnect, cancellation, failed startup, and shutdown with running panes.
- Cover absent zor, malformed reports, observer failure, and terminal/process transparency.
- Preserve coverage for prompt counts, distinct passphrases, terminal restoration, passphrase changes, and guarded identity reset.
- Use isolated HOME/XDG directories, disposable credentials, local networking where practical, and PTYs for actual interactive behavior.
- Never read, reset, or modify my real keys; never terminate my real sessions.
- Run each affected repository’s required formatting, linting, tests, documentation, and relevant integration checks.
- Review the complete changes across all repositories as a separate pass. Validate findings, fix confirmed in-scope issues, and rerun affected checks.
- Do not broaden the work to repair unrelated failures; report exact blockers instead.

Proceed autonomously through implementation and verification. Ask only when a consequential decision cannot reasonably be resolved from repository evidence and this ownership model.

Do not commit, push, publish packages, create PRs, or comment on GitHub.

## Final handoff

- Explain the resulting ownership boundaries and public integration contracts.
- Identify what moved between projects and what remains shared.
- List affected repositories and how to reproduce the combined build.
- Report verification commands and results, review findings addressed, and remaining limitations or blockers.
- Clearly distinguish completed work from any deferred work.
