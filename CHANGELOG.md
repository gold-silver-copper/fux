# Changelog

## Unreleased — ECS-native source alpha

The workspace manifests identify this source as `1.0.0-alpha.1`. This section is not a
publication, release-tag or binary-availability announcement. See
[verification](docs/verification.md) for actual runtime/platform/provider evidence and
[capability status](docs/capability-status.md) for boundaries; no speedup is claimed here.

### fux

- Persistent server-owned PTYs with independent viewers, shared scene layouts, per-viewer
  focus/zoom/copy/scroll state and exact-pane attachment with optional PID checking.
- Private descriptor-based loopback BRP, scoped capabilities, instance/generation-guarded
  mutations, generated method schemas and allowlisted reflective reads.
- Bounded HTTP requests/connections, cursor-based watches with explicit retention gaps,
  retained input receipts and final pane evidence.
- Allowlisted scene/session persistence with explicit `auto`, `ask` and `none` restore
  policy and per-pane restore/skip decisions.
- Provider-owned surfaces with revision/provider guards, typed last-painted input,
  viewer-local scrolling and control-character-safe surface painting.

### zor

- Journaled task/attempt policy, launch/prompt receipts, uncertainty-aware reconciliation,
  provider-native bindings/reports/resume eligibility, checks and owned worktree lifecycles.
- Task dashboard hosted as a fux surface, exact live task attachment, and local or selected
  machine status/operations without turning UI closure into task cancellation.
- Private machine catalog as the durable configuration authority, separate control and
  attachment bindings, ADMIN-only active endpoint resolution, secret-free read projections
  and durable no-replay remote action intents.
- Plugin installation/linking, actions, panes/surfaces and event hooks; canonical
  `plugin:NAME/ACTION` bindings, activation/run-specific descriptors and workspace-scoped
  fux BRP grants.
- Durable event claims before hook dispatch, explicit crash/uncertainty boundaries rather
  than exactly-once side-effect claims, and adapter-owned process groups with production
  guardian cleanup for plugin/check/provider children.
- Destructive task verbs selected with `--machine` distinguish acknowledged (`0`), failed
  (`1`), accepted/pending (`2`) and uncertain (`3`) CLI outcomes without silent mutation retry.

### Operations

- Source installation into an isolated local root, controlled update/recovery instructions,
  and documented design, ownership, protocol and security contracts.
- Active acceptance is fux/zor only; koh and a future iroh-ssh transport are not active gates.
