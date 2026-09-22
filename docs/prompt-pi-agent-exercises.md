# Build a minimal pi-driven fux exercise suite

## Objective

Use real pi agent sessions to discover friction in operating fux through its existing BRP interface.

Test whether an agent can discover state, act correctly, verify results, and recover—not merely whether predetermined RPC sequences succeed.

Build the smallest reusable runner, execute an initial bounded campaign, and report evidence-backed findings. Do not modify fux to make the exercises pass.

## Read first

Inspect:

- fux’s current README and BRP implementation.
- `fux-fuzz/README.md` and relevant existing scenario/fixture code.
- Existing integration-test setup and process cleanup.
- Installed pi SDK documentation and relevant examples.

Resolve pi documentation/examples from the installed package, not this repository. Verify installed API signatures rather than copying assumptions from older examples.

Reuse existing fixtures and verification techniques where practical. Do not couple the suite to private test internals or create another general-purpose testing framework.

## Required model

Use the latest stable Gemini Flash model available from Google's Gemini provider at execution time for every measured agent run. Do not use Flash-Lite, a preview/experimental model, or another model family as a substitute.

Resolve the exact provider/model ID from pi's current model catalog and verify against Google's current model documentation that it is the latest stable Gemini Flash release. Do not assume a hard-coded version or an alias containing `latest` is current. Record the resolved provider/model ID, any alias used, the verification date/source, and the pi version in the campaign report and run metadata. Pin that resolved ID for the entire campaign so repetitions use the same model.

Preflight authentication and model availability before starting the campaign. If the required model is unavailable through the installed pi version or configured credentials, report the blocker rather than silently falling back. Do not switch models to make failing scenarios pass.

## Architecture

Use the pi TypeScript SDK:

- `createAgentSession` for fresh agent sessions.
- Explicit provider/model selection matching the required model above, and explicit thinking-level configuration supported by that model.
- Session events for recording tool execution and outcomes.
- Explicit resource loading so ambient extensions, skills, prompts, and project instructions do not silently change the experiment.
- Abort/dispose and server cleanup on completion, errors, and timeouts.

Keep it opt-in and separate from fux’s production dependencies and ordinary CI. Do not make paid model calls part of `cargo test`.

Start with one runner, a few scenario definitions, and straightforward verifiers. No plugin framework, dashboard, database, or orchestration service.

## Agent interface

Give the exercised agent:

1. The current fux README.
2. A scenario goal and fixture information needed to attempt it.
3. One thin raw BRP tool accepting a method name and JSON parameters.

The tool should forward requests to the isolated test server and return its response. It may enforce request timeouts and record calls, but must not:

- Translate semantic actions into RPC sequences.
- Discover entity IDs on the agent’s behalf.
- Repair malformed requests.
- Interpret ANSI frames.
- Poll for command completion automatically.
- Provide convenience operations such as “run tests,” “find pane,” or “read terminal.”

Expose existing custom fux methods, including attach/frame, alongside stock BRP. Do not add any server methods.

No direct agent filesystem/shell tools in the initial measured suite. The agent may launch processes and interact with programs through fux itself. Provide the README in its context rather than adding a general filesystem tool.

This deliberately measures BRP usability, not pi’s ability to bypass fux.

The harness/controller may use OS tools for setup and independent verification. Keep its private verifier data out of the agent prompt.

## Safety and experimental isolation

Fux BRP permits arbitrary same-user execution. Restricting pi’s tools is not a sandbox.

- Use a disposable server, unique loopback port, temporary workspace, and isolated shell configuration per run.
- Never connect exercises to a user’s existing server.
- Use only trusted fixture programs and bounded workloads.
- Do not expose credentials to fixture processes unnecessarily.
- Never claim temporary directories contain arbitrary agent-launched commands.
- If stronger containment is required, use an actual OS/container boundary and document it.
- Clean up only processes/resources owned by the run.
- Preserve failure artifacts before cleanup.
- Do not promise containment of intentionally disowned descendants.

## Initial scenarios

Implement five scenarios. Each has an outcome-oriented prompt, deterministic setup, explicit success criteria, and an independent verifier.

### 1. Discovery and organization

Provide multiple named workspaces/tabs/panes.

Ask the agent to identify the relevant process, organize its pane into the requested destination, and leave a clear name and focus state.

Verify process identity is preserved, layout relationships are correct, and unrelated panes remain intact.

### 2. Exact launch and completion

Ask the agent to launch a fixture test program with specified argv/cwd, determine whether it succeeded, and leave its final output accessible.

Include passing and failing fixture variants.

Independently verify actual argv/cwd, exit status, and the agent’s reported result. Do not accept shell-prompt recognition as evidence of process completion.

### 3. Noisy-output investigation

A fixture produces bounded output containing a diagnostic followed by enough output to require history inspection.

Ask the agent to identify the failure and leave the relevant evidence accessible.

Keep the diagnostic within configured retained history. Do not accidentally make the task impossible through eviction.

Measure frame parsing, scrolling, discovery, and response-volume friction. The verifier knows the diagnostic independently.

### 4. Modal interaction

Begin with a real rename prompt, chooser, or confirmation already open.

Ask the agent to inspect what is pending and complete or cancel the specified interaction without sending unintended input to the child application.

Verify resulting names/layout and the child’s recorded input. Exercise the actual overlay/input path rather than accepting a direct rename that sidesteps the scenario.

### 5. Recovery and coexistence

Run a second viewer and introduce one controlled disruption, such as removing a target after the agent has observed it.

Ask the agent to recover and complete a bounded task while preserving an unrelated process and the second viewer’s private navigation state.

Trigger the disruption from an observable milestone, not a fragile sleep. Record exactly when it occurred.

Distinguish expected shared-process effects from actual viewer-isolation failures. Do not require impossible independent PTY geometry.

## Verification principles

The agent’s final response is a claim, not the oracle.

Use independent evidence as appropriate:

- Fixture-written argv/cwd/input/result records.
- Native process observations.
- Authoritative ECS relationships and process IDs.
- Decoded final terminal frames.
- Explicit checks that unrelated processes survive.

Do not let the verifier repair state before evaluating it.

Keep fixture truth and verifier logic outside the normal agent context. This is experimental separation, not a security guarantee against arbitrary same-user execution.

Report partial success and uncertainty honestly. A polished explanation must not turn a failed task into a pass.

## Bounds and configuration

Support explicit configuration for:

- Provider/model ID, constrained to the verified Gemini Flash release above for the measured campaign.
- Thinking level.
- Scenario and repetition count.
- Per-run wall-clock deadline.
- Maximum tool calls.
- Artifact directory.

Default to a small campaign: five scenarios, two fresh-session attempts each, with conservative finite budgets.

Run a preflight first. Confirm credentials/model availability without printing secrets. Do not silently substitute models.

Abort when limits are reached, record the reason, and clean up. Avoid unbounded retries or autonomous “keep trying until green” loops.

Track token usage and cost where pi provides them. Do not claim an exact hard spending cap unless it is actually enforced.

## Evidence

For each run, retain:

- Exact scenario prompt and supplied documentation revision.
- Fux commit/build identity.
- Pi version, resolved provider/model ID, any model alias, model-verification date/source, and thinking level.
- Scenario seed/variant and configured budgets.
- Timestamped tool calls, raw BRP requests/responses, errors, and final agent answer.
- Verifier findings and supporting artifacts.
- Wall time, tool-call count, retries, and available usage/cost data.
- Timeout, abort, cleanup, and manual-intervention information.

Keep artifacts bounded. If any output is truncated, mark it explicitly. Distinguish truncation in the model-visible tool response from truncation in saved artifacts.

Never record credentials or provider authorization headers.

Recorded RPC traces should help reproduce failures, but do not promise deterministic LLM replay or replay across different entity IDs.

## Friction report

Classify each finding:

1. Fux bug.
2. Documentation/discovery gap.
3. API ergonomic gap.
4. Missing capability.
5. Agent reasoning mistake.
6. Harness/verifier defect.
7. Provider/budget failure.

For each finding include:

- Scenario/run IDs.
- Minimal relevant transcript excerpt.
- Expected versus observed behavior.
- Independent evidence.
- Whether it recurred.
- Smallest plausible next step.

Do not assume every agent failure requires a fux feature. Prefer documentation fixes when the capability already exists.

Report successful but expensive workflows too: excessive schema queries, repeated ANSI decoding, cumbersome entity creation, or uncertainty about completion.

Do not change prompts between repetitions without recording a new experiment variant. Do not feed one run’s discovered solution into another fresh baseline session.

## Scope exclusions

Do not add:

- A fux SDK, MCP server, or convenience action layer.
- New fux RPC methods or production components.
- Terminal-text extraction or receipts just to simplify testing.
- A second fuzzing framework.
- An evaluation dashboard or generic benchmark platform.
- Unbounded autonomous campaigns.
- Claims about general agent reliability from a small sample.

## Deliverables

1. Minimal opt-in pi SDK runner and five scenarios.
2. Deterministic fixture setup and independent verification.
3. Tests for runner bookkeeping, deadlines, verification failures, and cleanup that do not require paid model calls.
4. A documented dry-run/preflight path.
5. A bounded real-agent campaign with preserved evidence.
6. A concise findings report ranking demonstrated friction.
7. Reproduction commands and known limitations.

If real model execution is blocked by credentials, quota, or provider availability, complete what can be verified locally and report the exact blocker. Do not substitute scripted RPC sequences and call them agent runs.

Success means we learn where a real agent struggles with today’s fux—not that every scenario passes.
