# Initial-agent state coverage audit

This audit checks original §4 against the retained fixtures and current bundled-rule
tests. The required categories are working, approval/**input required**, idle,
startup, exit, resizing and unknown screens. It also requires negative fixtures for
stale scrollback, quoted prompts, echoed names, shell output, nested tools and
malformed control strings. It does not require every approval mode, natural exit
for every agent, or real nested-tool execution. Those distinctions preserve the
scope; they do not turn unverified variants into supported behavior.

All paths in the matrix are under `zor/tests/fixtures/agents/<release>/`.
`docs/agent-state-coverage.json` pins the inspected source files by SHA-256 and
records native busy-event pointers, resize widths and exit evidence.

| Required category | Codex 0.153.4 | Claude 2.1.263 | OpenCode 1.18.29 |
|---|---|---|---|
| Startup | `startup.json`: real sign-in | `startup.json`: real theme selection | `startup*.json`: real home screen and placeholder variants |
| Working | `authenticated.json`: real Working/interrupt rows; bundled rule | `authenticated.json`: real active spinner/interrupt footer; bundled rule | `events.json`: real native `session.status` busy events under synthetic provider; native activity evidence, not passive working-screen coverage |
| Approval/input required | `approval.json`: unanswered command approval; sign-in and trust also Blocked | `startup.json`: unanswered theme choice is Blocked | `integration.json` and `integration-storage.json`: native permission and question dialogs; passive Blocked rules |
| Idle/input ready | `authenticated.json`: response composer; narrow Idle rule | `authenticated.json`: pre-prompt and post-response manual-mode composer; narrow Idle rule | `startup*.json` plus retained live observation: home composer; Idle rule |
| Exit | `authenticated.json`: native `/quit`, status 0; forced-loss trace separately | `authenticated.json`: native `/exit`, status 0; forced-loss trace separately | `lifecycle.json`: explicit fux kill, retained final record with unknown exit status; natural exit not established |
| Resizing | `lifecycle.json`: 80→39→80 columns, no input | Same widths and no-input evidence | Same widths and no-input evidence |
| Unknown screens | Real blank/loading captures and narrow resized screen | Real partial welcome captures and narrow resized screen | Real narrow resized screen; separate blank diagnostic is not positive support evidence |

The original prompt explicitly prefers integrations where they establish stronger
evidence, while retaining passive detection for unmodified tools. OpenCode's native
busy events satisfy real working-activity fixture coverage; they do not establish a
bundled passive Working rule. Zor's OpenCode integration uses message ancestry and
incomplete assistant-message events for activity, distinct from response completion.
This capability difference remains documented. A synthetic provider drives the real
OpenCode application and native events; no cloud-model behavior is inferred.

Claude theme selection satisfies the original “approval/input required” category.
Its tool-specific approval UI remains unverified. OpenCode's forced-loss fixture
satisfies exit/lifecycle observation with explicit unknown status; it does not prove
natural success or verified task completion. Broader modes, versions and layouts
remain unsupported or Unknown unless separately evidenced. These are coverage limits,
not additional requirements silently added to the completion path.

## Negative evidence and default behavior

`tools/comparisons/detection_screens.py` derives all six required negative classes
from each of eleven real screens (66 cases). Stale transcripts include current
unrelated shell output after prior content; nested-tool cases wrap prior agent text
inside unrelated tool output. These are intentionally synthetic negative fixtures.
They test specificity, not real-agent execution of those tools. Zor returns Unknown
on all 66 with current rules. An exact indistinguishable copied viewport can still
spoof passive evidence; no passive verdict authorizes verification.

`zor/tests/bundled_rules.rs` also reconstructs real captured rows through the
production terminal model and checks flags, rules, partial screens, resizing and
adversarial mutations. All eight tests pass on current source. The separately
retained live default OpenCode observation and six real Codex freshness runs verify
normal discovery without a custom rule directory. Prior required service/event/
binding/producer fixtures establish default discovery, bounded observation,
reconnection, reload and diagnostics; they remain subject to the final full gate.

## Verification reviewed for this audit

- Current bundled-rule tests: 8 passed (preceding rule slice).
- `python3 zor/tools/test_lifecycle.py`: 4 passed.
- `python3 zor/tools/test_opencode_events.py`: 8 passed.
- `python3 zor/tools/test_opencode_integration.py`: 88 passed, including retained native integration variants and evidence mutations.
- `python3 zor/tools/test_capture_startup.py`: 2 passed.
- Current Codex authenticated/approval evidence: 8 passed; Claude evidence: 4 passed (unchanged fixture results reused).
- Current paired screen and live freshness evidence: 4 tests each passed (preceding rule slice).

This closes R4's recorded initial-agent coverage audit under its original scope.
It does not establish universal agent support or erase the explicit limitations
above. R6 remote runtime verification and R7 final whole-change review/gates remain.
