# Real-agent detection and integration evidence

The startup captures below contain no submitted input. The separate OpenCode
`events.json` fixture exercises two real TUI prompts with a local synthetic provider;
see [integration evidence](../../../INTEGRATIONS.md) for its scope and reproduction.
It does not broaden the bundled passive detection rule or implement task-report binding.

| Agent release | Retained real evidence | Remaining scope |
|---|---|---|
| Codex CLI 0.153.4 | Initial sign-in and authenticated directory trust require input; real working/input-ready response, unanswered command approval and native `/quit` exit 0; 80→39→80-column resize; forced pane release with unknown exit status | Other idle layouts remain unverified; nested-tool negatives are synthetic |
| Claude Code 2.1.263 | Initial theme selection requires input; partial welcome unknown; authenticated working/input-ready response and `/exit` status 0; resize and forced release as above | Tool approval and other idle layouts remain unverified; nested-tool negatives are synthetic |
| OpenCode 1.18.29 | Startup idle variants; native prompt/response, read-tool and permission/question evidence; reload/resume; resize and forced release | Broader passive working/blocker/idle screens, natural exit, other provider/model/session layouts |
| Pi 0.80.10 | Real unauthenticated startup reports no models available; no bundled zor rule | Working, blockers, idle, natural exit, resize and nested tools; capture is not claimed state support |


The captures come from installed binaries, with their SHA-256, the fux binary SHA-256,
UTC capture time, dimensions, revision, and elapsed observation times recorded in JSON. In the
startup captures no input was sent. HOME, XDG directories and agent configuration directories were temporary;
the child environment was constructed from an allowlist. The only redaction replaces the
temporary directory with `<CAPTURE_ROOT>`. Repeated visually identical captures are grouped,
retaining first/last revisions, elapsed times and sample counts. This is viewport evidence,
not an original byte transcript or evidence of application processing.

Reproduce from the fux checkout, selecting a new output directory:

```sh
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- capture-startup --fux target/debug/fux \
  --agent /absolute/path/to/codex --output /tmp/new-codex-capture
cargo test --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask capture_startup
cargo test --manifest-path zor/Cargo.toml --locked --test bundled_rules
```

The capture tool launches no task and sends no keystrokes. It waits six seconds after socket
startup and terminates/reaps its own fux server, which owns the pane process. Capture files
must be reviewed before publication. A failed collection or cleanup leaves `diagnostic.json`
when writable; this is not a validated fixture. `startup.json` requires successful collection
and a zero server exit. Forced server cleanup does not prove its pane was reaped. The tool does not claim to sandbox an arbitrary executable
or prevent it accessing platform credential stores; use only the intended trusted binary.

The bundled rules are deliberately exact and specific to these screens. Other languages, versions,
wrapped widths, edited wording or additional output may return unknown. It proves neither
broad agent support nor authenticated task behavior. Tests derive quoted, shell-prefixed,
stale-text, altered and malformed-control negative screens from the real captures; those
mutations are synthetic adversarial tests, not real-agent coverage. An indistinguishable exact
copy can still spoof passive screen evidence, even in an identified agent process. Such a
verdict cannot authorize an action or establish task success.

These patterns were authored from the recorded local output, not copied from herdr manifests.

Claude’s theme rule intersects an exact prefix, exact suffix, and total Unicode-character count
to retain whole-screen specificity within the existing 512-byte per-regex limit. This does not
recognize alternate themes, terminal widths, later startup screens, or authenticated prompts.

Positive fixture tests reconstruct visible rows through zor’s production terminal model. Its
ScreenView removes trailing row padding and adds a final newline; rules match that normalized
view. Synthetic negative strings test the matcher and do not establish raw-byte OSC handling.
The capture tool preserves a supplied executable symlink name so versioned installations retain
their normal invocation identity. Process-name identification alone never supplies a state.


OpenCode's rule conjoins three fixed line-offset patterns covering its complete normalized 80×23
startup layout. It recognizes the Build/Big Pickle screen with the recorded connection tip;
only the bounded cwd footer and the three placeholders enumerated by the tagged upstream
[home route](https://github.com/anomalyco/opencode/blob/v1.18.29/packages/tui/src/routes/home.tsx)
vary. All three placeholders are now retained in real captures; the final variant also passed a
live default observer check. Model/provider changes, entered text, additional messages and
other layouts remain unknown. Idle denotes input readiness, never response or task completion.

The binary came from the official [v1.18.29 release](https://github.com/anomalyco/opencode/releases/tag/v1.18.29);
`release.json` retains the asset URL, size, archive SHA-256 and executable SHA-256. The downloaded
archive size and SHA-256 were verified before extracting the executable. The first six-second
capture reached the home screen; a second remained blank and is recorded as a diagnostic, not
validated positive evidence. A 20-second capture reached the same layout with another placeholder.
The capture tool now accepts `--duration SECONDS` from 1 through 30 (default 6), recording the chosen
window. Its window is an observation budget, not proof of application startup time or a native OS
call deadline. No credentials or input were supplied. A fresh live default `zor watch --once`
recognized OpenCode as idle with this rule, input_sequence0 and no observation problem; fux exited0.


`opencode-1.18.29/integration.json` records retirement of an unsent arm after an explicitly
injected durable crash point, followed by three managed prompts through zor's installed
OpenCode adapter: plain text, a real read-tool continuation with an intermediate stop,
and an unanswered root shell permission. Each has an acknowledged arm, delivered input
and matching native binding/report ancestry. The task stays open. Binary, adapter and
harness hashes identify this capture. This uses a synthetic local provider and private
configuration, not an authenticated cloud model. See [INTEGRATIONS.md](../../../INTEGRATIONS.md)
for reproduction and limitations. `cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- verify-opencode-integration` validates
the trace and rejects misleading mutations; the separate event probe remains unchanged.


`opencode-1.18.29/resume.json` records the application-level resume contract across an actual
owned fux server restart. OpenCode 1.18.29 was launched with `--session ID` in the same private
cwd/HOME/XDG storage namespace. Its new process displayed the prior response before any new
terminal input, consumed one fresh prompt under the same native session, and sent prior user
and assistant history to the local fixture provider. Each server lifetime received exactly one
input operation; both fux processes exited cleanly. This is a real OpenCode TUI/persistence test
with a synthetic provider, not zor task resume policy or model-quality evidence.

Reproduce with installed binaries and a new output directory:

```sh
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- capture-opencode-resume --fux /path/to/fux --opencode /path/to/opencode --output /tmp/new-resume-capture
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- verify-opencode-resume
```

Fresh captures use the Rust harness and record its executable hash. The retained
Python capture remains historical evidence; its original hashes are unchanged.

The validator checks native session continuity, distinct messages/processes/server incarnations,
input boundaries, correlated completions, retained provider history, cleanup and fixture
provenance. Misleading mutations are rejected. The combined dependency gate runs the retained
validator without requiring OpenCode to be installed; the capture command requires the real
agent binary. A preserved native session ID alone does not prove that its corresponding local
storage still exists or grant zor permission to recreate a command.


`opencode-1.18.29/integration-storage.json` is a real OpenCode integration capture with a
synthetic provider and clean plugin reload. It retains exact registered HOME/XDG metadata,
unchanged across the reload, plus native message bindings, fresh responses and an unanswered
question blocker. Reproduce with `cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- capture-opencode-integration --reload-adapter --blocker question` and its required fux/zor/opencode/output paths. The existing integration validator
now runs 88 evidence/mutation tests, including missing, foreign and changed namespace cases.
The retained trace records its exact historical executable and Python-source hashes. Fresh Rust captures identify and hash the actual tooling executable, including the reload launcher.
This is resume metadata evidence, not a zor task resume implementation.


`opencode-1.18.29/zor-resume.json` exercises zor's explicit resume policy using real OpenCode
and an owned fux restart. The same task retains its policy and history while receiving a new
process/session/attempt; native conversation persists. A reserved old prompt is never replayed.
The harness drops the accepted creation reply, observes uncertainty and proves one creation
across retry. It covers missing-metadata/cancelled-task refusals, fresh native response, source-bound
check execution and explicit verification of the resumed attempt without altering old input.
Provider responses are synthetic; this is workflow/correlation evidence, not model quality.

```sh
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- capture-zor-resume --fux /path/to/fux --zor /path/to/zor --opencode /path/to/opencode --output /tmp/new-zor-resume-capture
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- verify-zor-resume
```

The combined gate runs the nine retained evidence/mutation checks. Capture requires all real
binaries and uses disposable HOME/XDG, repository/worktree, provider and sockets with bounded
waits and owned cleanup. Prompt bearer tokens and disposable paths are redacted in retained output.


The three initial agents' `lifecycle.json` files were captured with the Python source now
preserved in `tools/archive/capture_lifecycle.py.txt`; their hashes still identify those bytes.
The Rust replacement, run from the fux checkout, is
`cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- capture-lifecycle --fux /path/to/fux --agent /path/to/agent --output /tmp/new-lifecycle`.
It creates a temporary split to narrow the viewport, removes that split to restore width, then
explicitly kills its owned agent pane. No input or authentication is supplied. The records
contain 15 bounded captures per stage, invocation/version/binary/harness hashes and retained
final output. All final exit statuses are null: this establishes pane release and server cleanup,
not a natural agent exit or successful task. Four evidence/mutation tests run via
`cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- verify-lifecycle`; the bundled-rule tests feed the real narrow captures through
the production terminal model and require Unknown, without expanding the 80-column rules.

`pi-0.80.10/startup.json` records the locally installed Pi executable's no-model configuration
screen. The herdr source inventory contains 21 agent manifests. Its declarations, source hashes,
regions and local executable lookup are in the combined repository's
`docs/comparisons/detection-inventory.{md,json}`; reproduce with
`tools/comparisons/detection_inventory.py --output PATH.json` and explicit `--binary AGENT=PATH`
for downloaded fixture executables. Rule counts are not measured accuracy. A PATH miss is not
proof that an agent cannot be installed or configured. This inventory does not close R4 or R5.


`codex-0.153.4/authenticated.json` uses the user's explicitly authorized login cache and
real OpenAI model `gpt-6-astra`, with fresh HOME/XDG/CODEX_HOME and a disposable work directory.
Only `auth.json` is copied, with mode 0600; personal config, history and exported keys are not
inherited. The CLI uses read-only sandboxing and approval `never`; the one prompt requests an
exact short response without tools. Codex's built-in app MCP startup is visible; no tool result
is claimed. Authentication follows the official [login caching documentation](https://learn.chatgpt.com/docs/auth).

The trace retains the real trust screen, working indicator, response, and final record with
exit status 0 after `/quit`. Trust is accepted only for the disposable directory. The exit
command and Enter are separate inputs: this release treated a combined write as pasted text.
Three generic fux input submissions are recorded; the prompt itself is a CLI argument. This is
an application exit, not task verification or native prompt binding. Account and model behavior
can vary; the capture is not a latency benchmark. Credentials are checked for accidental inclusion
before publication; temporary paths and any email addresses are redacted. The temporary auth copy
and logs are removed with the isolated home.

Reproduce only with authorized account usage (not as an unattended CI test):

```sh
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- capture-codex-authenticated --fux target/debug/fux \
  --agent /absolute/path/to/codex --auth-file /absolute/path/to/auth.json \
  --model gpt-6-astra --output /tmp/new-codex-authenticated-capture
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- verify-codex-authenticated
```

The four offline evidence tests run in the mandatory combined gate without credentials.
Production rule tests recognize the captured directory-trust and narrow working layouts and
reject derived quoted/stale/shell-prefixed/altered/malformed-control screens. The visible response
remains Unknown: no broad idle or tool-approval rule is inferred from one successful response.
The working rule currently requires this release's visible header, 80-column English layout,
model footer and default composer placeholder. Other layouts remain unverified.


`codex-0.153.4/approval.json` separately captures a real model requesting permission to run
`/usr/bin/true`. The CLI uses read-only sandboxing with approval `on-request`. The harness
accepts only the initial disposable-directory trust; it never answers the command approval.
The visible “Running” line above the dialog is not a tool result. After observing the dialog,
the harness explicitly releases its fux pane: final exit status is unknown, unlike the `/quit`
trace. No account usage-limit reset is performed. An initial exploratory run did not progress
past trust after an early Enter; the retained harness waits for the rendered trust screen to
settle before its single Enter rather than replaying input after an ambiguous result.

```sh
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- capture-codex-approval --fux target/debug/fux \
  --agent /absolute/path/to/codex --auth-file /absolute/path/to/auth.json \
  --model gpt-6-astra --output /tmp/new-codex-approval-capture
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- verify-codex-authenticated
```

The shared offline test entry now runs eight response/approval evidence tests. The new blocker
rule requires the recorded complete local-command confirmation dialog at the end of a 23-row
viewport. It takes precedence over working rules. Reason, command and prefix text may vary
within fixed single-line bounds; multiline commands, remote environment labels, other widths
and other approval dialogs remain unverified. As with every passive rule, an exact copied live
viewport can spoof it; it cannot authorize an action. Derived quoted/shell-prefixed/stale and
altered-dialog negatives test specificity, not a proof that arbitrary nested output is safe.


The existing OpenCode native permission and question captures now also exercise passive blocker
rules through the production terminal model at their recorded 40×23 size. These require the
complete recorded dialog footer and 22 normalized non-trailing rows; the question additionally
requires its native “Asked 1 question” line. They do not change native adapter report precedence
or prove support for wider layouts, multiline commands or other question widgets. The provider
is synthetic, and exact copied viewport spoofing remains possible. The paired screen comparison
in the fux `docs/comparisons/detection-screens.md` records this narrow evidence and its limits.


`claude-2.1.263/authenticated.json` records the real application using the authorized exported
`ANTHROPIC_API_KEY`, a fresh HOME/XDG/CLAUDE_CONFIG_DIR and disposable work directory. The CLI
runs `--bare --model sonnet --tools ""`; the recorded footer identifies Sonnet 5. Bare mode avoids
personal hooks/configuration/keychain loading, and the empty tool list disables tools. This mode
and model/layout are explicit scope limits, not default-mode or nested-tool coverage.

The harness selects the displayed theme, accepts the already authorized exported key, acknowledges
security notes, and explicitly selects trust for its disposable directory. It sends one short
prompt as a CLI argument, observes the working interrupt footer and actual assistant response,
then types `/exit` and Enter separately. Eight fux inputs are retained with final input_sequence8,
matching command/pane, natural exit status0 and server cleanup0. One earlier exploratory probe
selected the directory dialog's default No/exit; that run is not retained as response evidence.

The entire API-key preview line is replaced with `<API_KEY_REDACTED>`; recursive path/email
redaction and a check for residual full key/key suffixes run before publication. No key is passed
in argv or copied into the fixture. Four offline provenance/negative/redaction tests run in the
mandatory combined gate. The test never needs a key. Captures are viewport observations, not
native prompt correlation or task verification.

```sh
# Requires an already authorized ANTHROPIC_API_KEY in the environment.
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- capture-claude-authenticated --fux target/debug/fux \
  --agent /absolute/path/to/claude --model sonnet --output /tmp/new-claude-authenticated
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- verify-claude-authenticated
```

The narrow working rule requires the recorded CLI/model header, empty composer and manual-mode
interrupt footer. It does not depend on the randomly chosen spinner verb. The subsequent shortcut
footer/response remains Unknown; a “done” message does not verify a task. API-key/security/directory
onboarding screens in this trace remain unsupported by passive rules beyond the existing theme
screen. Capturing `--tools ""` also exposed and verified a generic empty-argument launch fix in
fux and zor: only the executable name must be non-empty, and later empty arguments are preserved.

Claude’s authenticated manual-mode captures now supply narrow Idle evidence before
and after the response: the version/model header, complete composer borders and
shortcut footer must match. Active spinner lines and interrupt hints exclude Idle.
This denotes input readiness only; it cannot satisfy verification requirements or
replace a prompt-scoped response report. Other modes/layouts remain Unknown.

Codex’s recorded response composer now supplies narrow Idle evidence with the
version header and gpt-6-astra footer. Interrupt hints, Working rows and MCP startup
rows exclude Idle. The loading composer remains Unknown, and trust/command approval
remain Blocked. Input readiness never verifies a task or substitutes for an attributed
response report; other versions, layouts and model footers remain unverified.

The retained authenticated traces were produced by the original Python harnesses,
archived verbatim under `zor/tools/archive/*.py.txt` for provenance. Current capture
commands use Rust; their binary hash and `harness_kind` identify new captures.

Offline native-event integrity and adversarial cases run with:
`cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- verify-opencode-events`.
This reads retained events only; it does not launch an agent or contact a provider.
