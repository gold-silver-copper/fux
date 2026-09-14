# Finish integrated multi-machine supervision

Finish the existing multi-machine milestone in this workspace and its koh companion.
The intended workflow is: select a saved machine, inspect an agent or task, attach to
its exact pane, return to the same supervision context, and recover safely from
connection loss or process restart.

Execute the work, including implementation, verification and documentation. Do not
stop at an audit or another progress checkpoint. Backward compatibility and breaking
semver are not concerns. This prompt does not authorize commits, pushes or PRs.

## Establish the actual starting state

Read applicable AGENTS.md instructions and:

- `multi-machine-navigation-and-supervision-prompt.md` — the full acceptance contract;
  this continuation does not reduce its requirements.
- `docs/multi-machine-supervision-implementation.md` and retained verification evidence.
- `docs/service-ownership-contract.md` and `docs/strict-boundary-implementation.md`.
- `docs/multi-machine-supervision.md` and `docs/multi-machine-manual-acceptance.md`.
- The current machine clients, dashboard/handoff, task resume implementation,
  companion manifest and composition harness/CI.

Inspect staged, unstaged and untracked changes in fux/zor and any koh development
checkout. Preserve existing work. Identify live verification processes before starting
duplicates or rebuilding binaries they use. Treat checkpoint claims as evidence to
verify against source revisions, not proof that today's tree passes.

Create a compact requirement matrix with implemented behavior, retained evidence,
missing verification and concrete remaining changes. Prioritize the gaps below; also
finish any other unmet requirement in the full acceptance contract.

## Preserve ownership

- **fux:** local panes, PTYs/processes, terminal state/history, layouts, viewers and
  generic exact-process attachment/control.
- **koh:** remote identity, independent endpoint authorization, encrypted opaque
  forwarding and bounded transport reconnection/session resumption.
- **zor:** machine catalog, agent/task interpretation, supervision, application
  recovery policy and provider/session eligibility.
- **local-ipc:** generic local authentication, framing and deadlines.

Do not add task/provider policy to fux or koh, or networking/PTY/emulation ownership
to zor's default CLI. Keep koh's existing standalone shell separate from its gateway
dependency closure. Controller cleanup must affect only resources it demonstrably owns.

## Complete application resume

Verify the current guarded remote resume implementation, then exercise a successful
eligible OpenCode resume through a real koh gateway. Reuse and extend the existing
producer/resume fixture where appropriate. Test exact task/attempt/process guards,
service and fux incarnations, native session binding and retained operation identity.

Add dashboard resume controls with discoverable help, clear eligibility/refusal
messages and an explicit user action. Select an authoritative target incarnation;
never silently choose a replacement process or replay a prompt. Keep remote I/O off
the event loop and preserve cancellation and selection guards.

Verify that success creates precisely one intended attempt, preserves history and
unsent input, and reconciles the same operation without duplicate launch. Verify
ineligible providers, adopted tasks, stale selections and changed incarnations fail
without mutation. Distinguish synthetic provider fixtures from actual provider tests;
OpenCode resume does not prove Codex recreation or universal agent restoration.

## Prove recovery and ambiguous outcomes

Add real-process acceptance for controller restart, independently of remote zor and
fux restart. Check catalog persistence, fresh service observations, terminal recovery,
owned-helper cleanup and remote process survival. Do not revive stale selection
authority or automatically replay user input/actions after restart.

Inject a lost mutation reply after the server can have committed the operation. Show
that the UI reports an unknown outcome, retains enough operation identity to reconcile,
and queries authoritative state before any further action. Never blindly resend a
mutation. Prove one effect under explicit reconciliation of the same operation,
including restart where the contract requires durable identity.

Keep transport reconnection, retained-session expiry, controller restart, zor restart,
fux replacement and application resume distinct in code, messages and tests. Exercise
moved and replaced panes, stale observed agents and authorization failures. One failed
machine must not freeze or misattribute another machine's rows, actions or notifications.

## Finish the harness and integration

Use actual Local plus two remote service stacks through koh with independent control
and attachment grants. Run the relevant task and observed-agent workflows, including
exact attachment, return to the same selection, input isolation, cancellation races,
viewer failure, narrow layouts, reload, notifications, link loss and expiry.

Import actual PTY output into Betamax, replay and render it, and inspect the resulting
wide and narrow images. Fix clipping, misleading state, missing controls and terminal
restoration problems. Record binary/source provenance, assertions and reviewed frames.
Do not count screenshots alone as proof of process ownership or input isolation.

Integrate deterministic acceptance into ordinary CI. Identify and run the repository's
required formatting, lint, boundary/dependency, unit, CLI, composition and rendering
checks. Start with targeted checks and broaden as required. Report unrelated baseline
failures separately; do not expand into unrelated fixes.

The koh development checkout is not a published integration baseline. Preserve its
complete changes as an applicable patch with exact base and checksums. Do not pin an
unpublished commit or claim published composition is complete. Complete everything
locally possible, then state the exact publication step that still needs authorization.

## Review and handoff

Perform a separate full-diff review covering all intended tracked and untracked changes
across repositories. Validate findings, fix confirmed in-scope defects and rerun affected
checks. Inspect current hosted CI when accessible, distinguishing old published checks
from verification of local changes.

Update the user guide, manual two-host commands and requirement matrix to match the
final behavior. Clearly separate verified loopback results, synthetic provider evidence,
physical-host/WAN checks not performed, and externally blocked publication requirements.

Finish with concrete behavior delivered, exact verification commands/results, remaining
gaps and publication status. Do not claim this milestone achieves universal Herdr parity;
that requires a fresh capability-by-capability comparison with evidence.
