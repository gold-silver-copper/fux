Refactor fux into a standalone local terminal multiplexer with optional koh and zor integrations.

## Goal

A user must be able to build and run fux without koh or zor installed, without their source repositories, without any cryptographic keys, and without network access at runtime.

Local sessions must persist after detach or terminal closure. Backwards compatibility and breaking semver are not concerns.

## Project ownership

### fux
Owns:
- PTYs and pane process lifecycle.
- Persistent local sessions, workspaces, tabs, layouts, and scrollback.
- Terminal rendering, input handling, keybindings, and multiplexer commands.
- Local configuration and control interfaces.
- A versioned local attachment protocol.

### koh
Owns:
- All remote connectivity and network transports.
- Cryptographic identities, key storage, encryption, and authentication.
- Remote authorization, discovery, relays, and reconnect behavior.
- An optional gateway connecting authenticated remote clients to fux’s local interface.

### zor
Owns:
- Agent detection and observation.
- Observation events and their schema.
- Optional observation integrations.

Fux may display observation data, but must not depend on zor to create, operate, or restore a local session.

## Required architecture

1. Make the default fux build standalone.
   - No direct or transitive dependency on koh or zor in the default build.
   - A clean fux checkout must build without either sibling repository.
   - Do not disguise coupling by copying networking or agent-detection implementations into fux.
   - Move legitimate multiplexer primitives into fux, preserving applicable licenses.
   - Keep integration code outside the core dependency graph.

2. Retain persistent sessions through a local session server.
   - Prefer one server per OS user, managing multiple workspaces.
   - Start it on demand.
   - Communicate through Unix domain sockets.
   - Local startup, attach, detach, workspace switching, and control commands must never load or create koh keys.
   - Bind no TCP, UDP, discovery, or relay listeners during standalone operation.
   - Preserve existing pane processes when a client disconnects.

3. Secure the local interface.
   - Use private runtime directories and restrictive socket permissions.
   - Validate ownership and OS peer credentials on supported platforms.
   - Protect against unsafe paths, symlinks, stale sockets, and concurrent startup races.
   - Bound messages, clients, queues, and resource consumption.
   - Ensure slow or disconnected clients cannot block the session server indefinitely.
   - Treat the OS user account as the local security boundary.

4. Design a clear integration boundary.
   - Define explicit, versioned attachment and control contracts.
   - Keep transport-specific types and identity concepts out of multiplexer state and commands.
   - Koh must use an intentional gateway interface; fux must not manage remote keys or allowlists.
   - Authentication and authorization must happen before koh grants remote access.
   - Starting or stopping koh must not restart or terminate local fux sessions.
   - Preserve remote functionality through the new boundary rather than leaving it silently broken.

5. Make zor opt-in.
   - Default panes start their commands directly.
   - Enable observation explicitly through configuration or an integration.
   - Handle missing, crashing, malformed, or slow observers without losing pane functionality.
   - Keep observation data bounded and treat it as untrusted metadata.
   - Do not build another agent detector inside fux.

6. Handle upgrades and failures clearly.
   - Detect incompatible local protocol versions before entering terminal raw mode.
   - Explain when an existing session server requires a restart.
   - Never silently kill sessions to resolve a version mismatch.
   - Restore terminal state and close resources on failed attachment and cancellation.
   - Preserve the existing uncommitted koh handshake-cleanup fix.
   - Do not delete, reset, or modify personal keys, or terminate existing personal sessions.

## Implementation workflow

Inspect the current repositories, instructions, and uncommitted changes first. Identify coupling in both runtime code and the build graph.

Write a concrete implementation plan, then carry it through. Preserve existing multiplexer behavior while replacing its transport coupling; avoid creating a reduced second multiplexer alongside the existing one.

Update configuration, CLI help, documentation, packaging, and CI to match the final architecture. Remove obsolete key-management and network options from fux’s local product surface.

Do not claim standalone support merely because passphrase prompts disappeared or optional executables are absent. Verify that the default build and execution genuinely exclude koh and zor.

Do not commit, push, publish, or create PRs unless separately requested.

## Acceptance criteria

Demonstrate with automated tests and isolated real-binary scenarios:

- A clean fux checkout builds and tests without koh or zor source trees.
- Default dependency inspection contains neither koh nor zor.
- Fux starts in an isolated environment with no koh configuration or keys and produces zero key prompts.
- No cryptographic identities are created by local use.
- Standalone operation opens no network listeners.
- Detach and reattach preserve the same pane processes and their state.
- Multiple viewers, resize, input, keybindings, copy mode, and workspace switching work.
- Concurrent first launches produce one valid session server.
- Unauthorized local peers and unsafe socket paths are rejected.
- Malformed or oversized messages and stalled clients are handled within explicit bounds.
- Final-pane exit, explicit workspace termination, signals, and failed attachment clean up correctly.
- Protocol mismatches produce actionable errors without killing sessions.
- Local sessions work when koh and zor are absent.
- Optional remote access works through koh, and gateway failure leaves local sessions intact.
- Optional observation works through zor, and observer failure leaves panes usable.
- Standalone packaging does not require dependency patches or sibling checkouts.

Run appropriate formatting, linting, unit, integration, and real-binary checks. Report platform coverage honestly.

Finish with the implemented ownership boundaries, verification results, migration instructions, and any remaining limitations. Do not present an incomplete integration or a build-only workaround as the completed architecture.
