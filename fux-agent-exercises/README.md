# fux-agent-exercises

An opt-in harness that measures how well a real agent can operate fux through the
BRP API it already has. It is **not** a correctness test suite for fux, and not a
second fuzzer: [`fux-fuzz`](../fux-fuzz/README.md) already asks whether fux behaves
correctly. This asks a different question — whether an agent can *discover* what to
do, do it, verify it, and recover when the world changes underneath it.

It is separate from fux's production dependencies and from `cargo test`. Nothing
here runs in ordinary CI, and a measured campaign spends real money on real model
calls.

The findings from the first campaign are in [FINDINGS.md](FINDINGS.md).

## What the agent gets

1. fux's current `README.md`, verbatim, in its system prompt.
2. A scenario goal.
3. Exactly one tool, `fux_rpc`, which sends one JSON-RPC request to that run's fux
   server and returns the response verbatim.

It has no shell, no filesystem, no editor and no second tool. The tool never
discovers entity ids, never translates intent into request sequences, never repairs
malformed JSON, never decodes ANSI frames and never polls for completion. That
friction is the measurement, so removing it would defeat the point.

The harness itself uses ordinary OS access for setup and for independent
verification. Fixture truth lives in a run-private directory the agent is never
told about.

## Requirements

- Node **>= 23.6**. The sources are TypeScript and Node strips types at runtime;
  there is no build step, no bundler, and no typecheck.
- A global install of `@earendil-works/pi-coding-agent`. The harness resolves it
  through `FUX_PI_PACKAGE_ROOT`, then `npm root -g`, then the real path of `pi`.
  The pi SDK and typebox are imported through that one resolved root so exactly one
  typebox instance exists — pi validates tool arguments with typebox's compiler, and
  a second copy would reject the schema this harness builds.
- A built fux binary (`cargo build --release --locked`).
- A Google Gemini credential, normally `GEMINI_API_KEY`.

There are deliberately no npm dependencies and no lockfile.

## The required model

Every measured run uses the latest **stable** Gemini Flash release: not Flash-Lite,
not a preview, and not the floating `gemini-flash-latest` alias. Nothing is
hard-coded. The id is taken from pi's catalog by version rank and then confirmed
against Google's live model list and published model documentation; the resolved id,
the rejected aliases, the sources and the timestamp are recorded in every artifact.

If verification cannot confirm the choice, preflight fails and the campaign refuses
to start rather than quietly falling back to another model.

## Commands

```sh
cd fux-agent-exercises

# Check fux, pi, credentials, and resolve + verify the model. No model call.
node run.ts preflight

# Run every scenario's setup and verifier with no agent at all. No model call.
# Every check should fail: nobody did the work. This validates fixtures/verifiers.
node run.ts dry-run --artifacts /tmp/fux-ex-dry

# Harness self-tests, including positive controls. No model call.
node --test --test-concurrency=1 "tests/*.test.ts"

# The measured campaign. Spends money.
node run.ts campaign --repetitions 2 --artifacts runs/campaign-02

# Re-render the measurement table from artifacts.
node run.ts report --artifacts runs/campaign-02
```

Useful flags: `--scenarios discovery,launch` · `--repetitions N` · `--thinking LEVEL`
· `--max-calls N` · `--deadline-ms MS` · `--request-timeout-ms MS` · `--keep on-failure`
· `--model ID` · `--provider ID` · `--skip-model-verification` · `--readme PATH`.

Defaults: five scenarios, two fresh sessions each, 40 tool calls and 300 s per run,
15 s per request, thinking level `low`.

## Scenarios

| id | What it demands |
| --- | --- |
| `discovery` | Find a named process among several workspaces, move its pane to another workspace, rename it, focus it, disturb nothing else. |
| `launch` | Start a fixture with an exact argv and cwd, decide whether it succeeded, leave its output visible. Pass and fail variants. |
| `noisy` | Find one diagnostic line that has scrolled out of view but is still in retained history, and report its code. |
| `modal` | An overlay is already open. Complete a rename prompt, or decline a destructive close confirmation, without leaking keystrokes to the child. |
| `recovery` | Close a named pane and focus another while a second viewer stays untouched — the harness closes that pane first, just before the agent's own `close` or `focus` naming it is forwarded. |

Each scenario has deterministic variants, an outcome-oriented prompt, and a verifier
that never trusts the agent's account.

## How verification works

The agent's final answer is a claim. Outcomes come from independent evidence:
fixture-written argv/cwd/stdin records, real pids and exit codes, authoritative ECS
relationships, and `fux.frame` output decoded into rows by a small ANSI screen
decoder. The verifier never repairs state before judging it.

Outcomes are `pass`, `partial` (the end state is right but the interaction under test
was bypassed), `fail`, or `error`. A run whose fux server process died is flagged
separately, because "fux crashed" and "the harness broke" mean very different things.

**Positive controls matter here.** `tests/positive-control.test.ts` drives each
scenario to completion through the real agent tool with a scripted sequence and
asserts the verifier reports `pass`; one test deliberately bypasses the modal prompt
and asserts `partial`. Without these, a verifier bug would be indistinguishable from
an agent failure in every run. They are harness self-tests, not agent runs, and they
contact no model.

## Isolation and safety

Each run gets its own fux server, loopback port, temporary `HOME`, working directory
and shell configuration, so an exercise never touches a server you are already
running. Only the process this harness spawned is signalled during cleanup. The
child environment is rebuilt from a small allowlist, and anything whose name looks
like a credential is dropped, so fixtures cannot read the campaign's provider key.

Ambient pi resources are switched off explicitly — extensions, skills, prompt
templates, themes and context files — and the harness asserts afterwards that none
loaded, rather than trusting the flags.

**This is isolation for repeatability, not a sandbox.** fux's BRP is unrestricted
same-user command execution by design, so anything reachable through it runs with
your privileges. Restricting the agent to one RPC tool does not change that. Use
only trusted fixtures, and use a real OS or container boundary if you need
containment. Deliberately disowned descendants are not claimed to be cleaned up.

## Artifacts

`runs/<campaign>/campaign.json` holds the summary; `runs/<campaign>/<run-id>/run.json`
holds one run and is designed to stand alone as evidence, without the summary beside
it: the exact system and task prompts, the documentation sha256, an `environment` block
with fux's path/version/git revision and pi's version and root, the resolved model with
its rejected aliases and full verification record, the requested and effective thinking
level, the budgets, every tool call with its raw request and response, bounded session
events, the verifier's checks and evidence, usage and cost, and the server log tail when
the server died.

A scenario that changes its task wording records a `baseline.promptRevision`, so runs
are never pooled across different prompts by accident.

Response text is truncated twice, and both are recorded separately: what the model was
allowed to see (8 000 characters) and what was kept on disk (200 000 characters).
Credentials are never recorded.

`runs/` is gitignored: each `run.json` embeds the full system prompt, so three
campaigns came to about 6 MB. The durable, committed record is
[FINDINGS.md](FINDINGS.md), which inlines the decisive evidence for each finding, plus
[`evidence/`](evidence) for reproductions that do not need an agent.

## Known limitations

- Ten runs per campaign on one model is a small sample. Three campaigns is 30 runs. It
  shows where friction is, not how reliable agents are in general.
- Recorded traces help reproduce a failure by hand, but LLM runs are not
  deterministically replayable and entity ids differ between runs.
- Every scenario pre-attaches the agent's viewer and tells it the id, so attaching is
  not part of what is measured; everything else still has to be discovered.
- `noisy` is on prompt revision 2. Revision 1 ended with an answer-shaped placeholder
  (`CODE: E-1234`) that one run echoed verbatim; see F7. Results from the two revisions
  are reported separately and must not be pooled.
- The `recovery` disruption fires from a pre-forward hook, on the first `close` or
  `focus` command that names the target. The agent's own request then meets the
  "target no longer exists" notice. Campaigns 01 to 03 fired it on the first response
  mentioning the target instead, which was always the opening query (F8); their
  `recovery` results are not comparable with later campaigns.
- Token accounting and cost come from pi. Budgets bound tool calls and wall-clock
  time per run; they are not an enforced spending cap.
- Tested on macOS arm64 only.
