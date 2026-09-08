# Native provider selection

Recorded 2026-09-07, before adapter implementation, for N2 of
[the active milestone](native-agent-milestone.md).

Select **Codex app-server over stdio**, implemented as a provider-specific Rust
adapter in zor. fux continues to provide only generic process/pane/lifecycle APIs;
koh has no provider responsibilities.

The installed Codex 0.153.4 exposes an app-server protocol and generates its own
JSON schemas. These contracts provide a direct path from a zor operation identity
to a native client message ID, server user-item ID, thread ID, and turn ID. They
also expose an explicit turn interrupt and thread resume/read operations. This is
a stronger implementation basis for this milestone than inferring turn control
from terminal output or undocumented CLI behavior.

Inspection commands were `codex --version`, `codex app-server --help`, and
`codex app-server generate-json-schema --out ...`, plus `claude --version` and
`claude --help`. Raw output is retained in
`.verification/native-provider-inspection-20260907/`. Relevant schema excerpts
are retained in [native-provider-schema-excerpts.json](native-provider-schema-excerpts.json).

| Contract | Inspected fields and intended use |
|---|---|
| `turn/start` | Required `threadId` and structured `input`; optional `clientUserMessageId`. Use a stable zor operation identity as the client ID and preserve text literally. |
| `TurnStartResponse` | Contains `turn.id`, `items`, and `status`. A user item has native `id`, optional `clientId`, and typed content. Correlation must validate identity and content rather than assume the request was accepted. |
| `turn/interrupt` | Requires both `threadId` and `turnId`. Keep this distinct from coordination cancellation and owned-worker stop. |
| `thread/resume` | Accepts a retained thread ID; validate the returned identity and original storage namespace before claiming session recreation. |
| `thread/read` | Accepts `threadId` and `includeTurns`; supports a bounded recovery query. Missing or partial history cannot justify replay. |
| Input-required evidence | User-input requests carry `threadId`, `turnId`, `itemId`, `isBlocking`, and questions. Surface the structured blocker without requiring an interactive terminal. |
| Response evidence | Agent items carry server `id` and text; turn status is structured. Neither an agent message nor completed turn verifies task artifacts/checks. |

Claude Code 2.1.263 was also inspected. Its CLI advertises print mode, streaming
JSON input/output, replayed user messages, explicit session IDs and resume. The
inspected help does not expose a comparable turn-ID-scoped interrupt contract.
This is a selection based on the inspected surfaces, not a claim that Claude
cannot support one. Existing OpenCode already has the managed registration,
prompt-arm, producer lifetime and session/message ancestry path; it remains the
reference for integration with zor's ownership and uncertainty rules.

The adapter must use native JSON submission, not shell interpolation or terminal
line parsing. Persist submission intent before writing to app-server, preserve
the original deadline, and reconcile a lost response by identity. An optional
client message ID is correlation evidence, **not an assumed idempotency guarantee**.
No retry may send the same prompt again solely because an acknowledgement was lost.
Do not add provider logic to fux's input receipts or koh's byte transport.

This selection proves an installed interface contract only. The adapter, actual
protocol fixtures, unattended workflow, and any separately attributed live model
validation remain unimplemented/unverified at this point. No paid call, credential
inspection, permission change or remote R6 check was performed.

## Provenance (SHA-256)

| Input | Digest |
|---|---|
| Installed `/opt/homebrew/bin/codex` | `b973d440acac501fd2594a43e7ca9ce41e0a65b9dfb28d0d7a7837c99e1261e3` |
| Installed `/Users/kisaczka/.local/bin/claude` | `ef5d2909c8af49f31ab6d5487e90316777bc2fac170adfe8160716caa8aaf4f9` |
| `v2/TurnStartParams.json` | `a3835e8c1e942e4b358e1a670939b89918b16c4d13105a579899892b7ade6dea` |
| `v2/TurnStartResponse.json` | `6fc49c3e5d0ce11a3a109ae194619b04d6e3ff98fa29bac683fdb754608958be` |
| `v2/TurnInterruptParams.json` | `6dff382dae73d1dbc58406ed045605f647e7a49660e2540fbd2c6c24d60c5f2b` |
| `v2/ThreadResumeParams.json` | `8ac68582a81d60940b10b330be8546123f56bfe246b56f8a4f121da00f347cf2` |
| `v2/ThreadReadParams.json` | `dfe040c6ac71d30795b8be3f3ff232e66f362a37f883b491e5d1ea367f470db4` |
| `ToolRequestUserInputParams.json` | `fda15b62e7446105b59c8ed85c908abed2a47db70653e57c92c47a2f204fa77a` |
