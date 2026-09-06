> Historical record from before the 0.3.0 bevy_ecs rewrite (2026-09-05). The host, popup panes, sidecar supervision, protocol v2/`FUXCTL1` and verification results described here no longer exist. Current architecture: [design.md](design.md); current evidence: [ecs-acceptance.md](ecs-acceptance.md).

# Contextual-help acceptance audit

Audit date: 2026-09-05. Scope: the original requirements in
[contextual-help-prompt.md](../contextual-help-prompt.md), resumed through
[continuation-prompt.md](../continuation-prompt.md). The requested implementation review, acceptance audit, fixes and local verification are complete.
The earlier environment blockers are resolved. Hosted runs on the old checkpoint remain failed;
their diagnosed causes are fixed and verified locally. Publication and new hosted runs are outside
this task’s authorization.

## Review scope and findings

The independent reviewer inspected all 104 changed fux files in `c814a4a..1b9488d`,
including new files, standalone local transport/terminal ownership, contextual interactions,
tests/fixtures, documentation, workflows, manifests and semantic lockfile changes. The earlier
boundary refactor at `c814a4a` is the starting architecture; the subsequent standalone transplant
and contextual work are both in scope. Current unstaged fixes and untracked files are also reviewed.
A delegated independent reviewer covered koh `d6ded15..50a8270` (14 files) and
zor `25cbc46..fb6a1ef` (8 files), matching dependency-patches/manifest.json. Neither reviewer
implemented the fixes. No owner checkout edits, commits, pushes, PRs or comments were made.

| Finding | Validation and disposition |
|---|---|
| P1: terminal filter mistakes UTF-8 continuation bytes for C1 control-string markers | Public `ServerTerminal` reproduction rendered `beforeАafter` as `before`. Fixed streaming UTF-8 continuation tracking, including inside OSC. Regression checks all read splits, Unicode titles and oversized-string containment. |
| P2: application-mode arrows and keypad Enter ignored by viewer modes | Regression selected workspace alpha instead of beta with SS3 Down. Added SS3 arrows to modal navigation/help pagination and SS3 Enter to modal submit. Tests exercise workspace selection, all resize directions, copy movement and command paging. |
| P2: Escape prefix twice cancels instead of forwarding literally | Regression produced no bytes for two Escapes. Literal handling now precedes cancellation and binding dispatch. Test also covers defensive precedence over command keys. Config rejects binding/prefix conflicts. |
| P2: detach/switch could discard input queued by an earlier read | Source confirmed biased connection cancellation could beat queued input because the barrier depended on the current chunk. Drain acknowledgement is now unconditional. A real viewer protocol harness verifies separate-read detach waits for a reply and suppresses trailing commands; the two-viewer scenario verifies preceding input reaches the surviving pane. |
| P1: asynchronous mode cancellation reinterprets pasted bytes as commands | Public controller reproduction dispatched NewTab from pasted `t` after the copy target disappeared. `owns_input` retains canceled decoder ownership through paste/partial sequences; lookup failure clears replay and loading overflow still parses the current byte. Regression covers target removal, copy-read failure, lookup failure and overflow at each identified BEGIN split, then verifies commands resume after END. |

A sixth, P2 verification finding reproduced under installed stable Rust 1.97.1: strict Clippy
rejected an existing byte-character array in a test (`byte_char_slices`). Replacing it with the
equivalent `*b"A2P"` passes stable strict Clippy; the independent reviewer verified equivalence
and the affected terminal-sequence test passes. Downloaded CI job logs confirm this exact error.

A seventh, P2 verification finding explained both hosted macOS fixture failures. The terminal
transcript contained the correct final output, followed by primary-screen restoration; the test
reparsed the transcript and observed only the restored screen. It now waits for viewer exit and
reader completion, then parses the rendered screen immediately before restoration. A deterministic
paint-and-restore regression also rejects text erased before teardown, preserving the assertion’s
strength. No production shutdown delay was introduced.

No finding was waived. Independent production/test review confirmed all behavioral fixes, the
lint fix and final-frame fixture fix; the reviewer also replayed the failing CI transcript to
prove the observation race. Final review found no remaining confirmed defect.

## Requirement-by-requirement evidence

Paths and test names below refer to this checkout. Source inspection establishes the routing and
state invariants; the named tests exercise the corresponding effects. `client` means
`tests/client.rs`; runtime scenarios are run through `tests/local_cli.rs` against the actual fux binary.

| Original requirement | Source and acceptance evidence |
|---|---|
| Standalone local multiplexer; no koh/zor dependencies or keys | Cargo.toml/lockfile, local client/server, daemon and observation boundary reviewed. `project_dependencies_and_application_imports_respect_ownership` and dependency-tree check exclude owner crates. `local_attachment.py` checks no keys/network sockets and preserved shell PID after reattach. |
| Discover actions without sacrificing ordinary input | `DetachFilter`, viewer bridge and shared registry separate commands from `PaneInput`; contextual-help and two-viewer real-TTY scenarios exercise both discovery and continued pane input. |
| Configured prefix enters command mode | `DetachFilter::process` enters immediately on a complete prefix; Escape requires terminal-sequence disambiguation (35 ms). Client live-binding test and real custom C-b scenario verify configuration. |
| Approximately 200 ms delayed compact bottom panel | Config default is 200 ms. Bridge deadline independently triggers hint watch repaint; `HintPanel::paint` paints at bottom with at most ten body rows. Real silent-pane test rejects early hints and requires later repaint. |
| Commands execute before hints; fast sequences execute once without flash | Bridge processes bytes and ordered acknowledgements before publishing the next panel. Real fast-command check rejects flashes; one-read new-tab/rename verifies exactly two tabs and correct target; resize burst proves ordered progress. |
| Simple action completes to pane input | Filter clears pending state before typed dispatch; real new-tab/rename, split/close and popup input scenarios exercise continuation. |
| Esc cancels command mode without leaking | `resolve_escape` consumes cancellation; `contextual_prefix_holds_unknown_keys_and_cancels_without_leaking_escape_tails` checks plain input afterward. Real help/dialog cancellation exercises terminal path. |
| Double prefix preserves literal bytes | Literal precedence fix; custom-prefix/literal client tests and real popup raw-byte probe require exactly one prefix plus Q. Local frame-boundary test prevents cross-viewer interpretation. |
| Unknown key does nothing, stays in command mode, reveals immediately | Filter retains pending state and sets reveal flag; client unknown-key assertions require empty pane output and pending state. Real default/hidden/immediate scenarios require prompt hints. |
| No inactivity cancellation | Bridge has only hint and Escape deadlines; no command inactivity transition. Silent-pane help remains active after the hint deadline. |
| Grouped relevant commands; tiny screens keep commands reachable | Registry groups and availability predicate drive panel and dispatch. Grouped-page test collects every binding and checks dim unavailable actions; two-viewer runtime pages 3×18 help and survives 1×1 then restores context at 12×48. Physical 1×1 cannot display full labels. |
| Workspace picker choices/navigation/Enter/Esc | `Interaction::{loading_workspaces,workspaces_loaded,feed,key}` and startup picker share panel/state. Loading replay/cancel test, SS3 selection regression and real workspace switch/cancel/startup scenarios verify choices and queued suffix preservation. |
| Tab picker choices/navigation/Enter/Esc | Mode::Tabs stores stable IDs, validates selected target and emits SelectId. Real tab-picker scenario selects main after creating another tab; cancellation uses shared Esc path and panel labels. |
| Copy hints depend on selection; completion returns; Esc backs out | CopySession and Mode::Copy own selection and distinct hint bars. Client private-copy test checks select/clear/back/completion/clipboard; real keyboard and Shift-drag scenarios verify OSC52 output without shared copy state. |
| Rename focused input, submit/cancel controls, original preserved on cancel | Mode::Rename stores a local bounded buffer and target ID. Client fragmented Unicode submit test; real canceled rename keeps main; Unicode rename succeeds; narrow-input test retains grapheme and insertion marker. |
| Destructive confirmation identifies target/consequences | Mode::Close panel names pane ID and terminated process/unsaved work. Real scenario proves no close before y, n preserves pane, y closes exactly one. Delayed paste test prevents pasted y/Enter confirmation. |
| Help/popup navigation and dismissal controls | Command panel footer includes Esc/paging/literal prefix; popup bar resolves actual prefix and close binding from registry. Popup-footer test and real popup-input/literal/help/close scenario verify it does not consume application input. |
| Repeatable mode exposes controls until finished | Mode::Resize emits repeated bounded deltas, thin bar names directions/Enter/Esc and says changes kept. Real resize and 256-adjustment burst verify applied changes and completion; SS3 regression checks physical application-mode arrows. |
| Compact panels, dialog footers, thin persistent bars; avoid popup stacking | One hint watch value is chosen with modal panel precedence, then command panel, then popup footer. Copy/resize use thin bars; hint and compositor tests verify rendering and clipping. |
| Completion returns to appropriate context; Esc backs out one level | Mode::Pane follows submit/q/Enter; cancel sets back and bridge reveals command help; next Esc returns to pane. Copy first clears selection. README documents immediate resize changes are kept and other edits submit explicitly. |
| No application-mode inference/input hijacking | Modes enter only through configured fux actions or explicit copy mouse gestures. Applications' terminal flags are mirrored, with CSI/SS3 handling in owned modes. Two-viewer runtime and application-mouse tests verify ordinary input isolation. |
| Inspect routing/compositor/config/dispatch/copy/pickers before design | Chronological contextual-help-plan.md inventories these surfaces and records state/presentation decisions; complete review inspected their current integrated implementations. |
| Shared command descriptions cover execution, binding, labels, grouping, availability, CLI/help/hints | `DEFAULT_BINDINGS`, BuiltinAction, Command::request and ClientBindings in commands.rs provide the common registry; main bindings output and hint panel iterate it; bridge uses the same availability predicate and typed host requests. Registry/group consistency and custom-binding tests verify agreement. |
| No separate command lists that drift; reuse command implementations | Registry maps action/command once; host handles shared typed control operations. Modal navigation keys describe fux interaction controls, rather than duplicating dispatch entries. Structure and registry tests inspect these boundaries. |
| Explicit focused interaction states, no generic UI framework | Mode enum names Pane/Copy/LoadingWorkspaces/Workspaces/Tabs/Rename/Close/Resize; prefix filter owns command discovery. Current design is scoped to these interactions. |
| Transient state belongs to initiating viewer; shared operations coordinated | Each connection creates its own filter/Interaction/hint and CopyUi watches; requests receive state before acknowledgement. Private copy uses correlated viewport requests and stable target IDs. Same-workspace real viewers independently type, edit, select, copy, display help and detach. |
| Hint delay repaints without output; timers canceled on transition/detach/shutdown | Bridge computes deadlines from current prefix epoch/pending state and owns no detached timer task; connection exit aborts/joins bridge. Real silent-pane, fast command, resize and detach tests cover lifecycle; source establishes shutdown cancellation and stale timer exclusion. |
| Preserve configurable bindings/literal semantics | Config aliases validate byte encoding and prevent prefix collisions; live policy refresh preserves partial input. Custom C-b and external-binding real scenarios plus byte-exact literal regression. |
| Split reads and several sequences in one read | Fragmented CSI/SS3/paste/mouse and Unicode client tests; real one-read command bursts, workspace switch suffix and separate-read detach handshake. |
| Consumed commands/navigation/cancel bytes never leak | Filter and Interaction consume owned input; normal bytes use distinct raw messages. Canceled-interaction regression verifies paste remains consumed across asynchronous target/loading failures. Client assertions inspect emitted bytes/requests; real popup literal probe and cross-viewer frame-boundary regression verify transport separation. |
| Terminal modes, paste, Unicode and resize preserved | Workspace input-mode/terminal restoration tests; CSI/SS3 regressions; delayed paste tests; UTF-8 terminal-filter regression; Unicode rename/copy tests; actual terminal resize scenario. |
| Focused content remains visible where practical | HintPanel pages around selection and clips rename by grapheme/display width; copy override pins original target/focus. Narrow-input, copy-target and real tiny-screen tests. |
| Bound dimensions/lists/rendering work | Panel ten-row cap; ClientBindings fixed bitsets; state max dimensions 512, aggregate 262144 cells, 64 tabs, 256 panes; manager workspace cap, 4096-byte loading input, 64-byte sequence buffers, 128-byte rename buffer, 1 MiB encoded clipboard. Geometry/copy-limit/protocol and structure tests verify bounds. |
| Immediate hints and hidden automatic hints preserve commands/explicit help | HintPreferences zero-delay/automatic flags, validated 0–5000 ms range; real default/immediate/hidden runs; preference validation test. Unknown keys still reveal help. |
| Existing configuration conventions/defaults documented | Kebab-case serde config, standard XDG/platform discovery, README [hints] defaults and clipboard behavior; config default/round-trip/unknown-key tests. |
| Document deliberate interaction and protocol changes | README covers private modes, Esc levels, kept resize changes, explicit confirmations, protocol-v2 migration and old-server rejection before raw mode; local attachment contract documents ordered replies and private copy. |
| Read instructions/current work; preserve unrelated edits | Initial statuses checked all three owners; only continuation-prompt.md was untracked. Original files retained; modifications confined to confirmed defects, regressions and review/audit documentation. |
| Inventory and concrete implementation plan | docs/contextual-help-plan.md records inventory, modes, hint presentations, transitions and subsequent integration evidence. It is explicitly historical, not acceptance by itself. |
| Complete flow beyond static popup | Pickers, rename, confirmation, copy/scrollback/mouse/clipboard, repeatable resize, registry, preferences and protocol are exercised by the evidence above. |
| Do not commit/push/publish/modify personal sessions | No external mutations performed. Tests use temporary HOME/XDG directories and owned processes. Owner repositories remain clean; fux changes are left for user review. |
| Meaningful tests and isolated real binaries | Root client/host/protocol suites plus contextual_help.py, contextual_viewers.py, detach_drain.py, fixture binary/model/in-process corpus and oracle checks; optional integrations require exact binaries explicitly. |
| Run formatting, strict lint/checks; review and fix confirmed defects | Commands/status ledger below plus independent full review and fix review. Initial regression failures were retained as evidence, not presented as final green results. |
| Final report explains flow/defaults/verification/limitations | README describes final interaction; this audit and HANDOFF.md record findings/checks/limits; user handoff reports final state. |

## Final verification

All requested root and fixture commands were rerun after the final production fixes on macOS
with the active Rust 1.96.0-nightly toolchain. Local logs use `/tmp/fux-acceptance-*`; these are
local evidence, not portable artifacts. Initial failing regressions and a test-only Clippy slicing
warning were fixed before this final pass.

| Command | Result / log suffix |
|---|---|
| `cargo fmt --all --check` | Passed; `fmt-final.log` |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | Passed; `clippy-final.log` |
| `cargo test --locked --all-features` | 258 passed, including 44 client tests, six isolated CLI scenarios and corpus/structure/loom checks; `full-final.log` |
| `cargo doc --no-deps --all-features --locked` | Passed; `doc-final.log` |
| `cargo build --locked --bin fux` | Passed; `build-final.log` |
| `cargo clippy --manifest-path tests/verify/fixture-child/Cargo.toml --locked -- -D warnings` | Passed after final fixture fix; `/tmp/fux-resume-fixture-clippy.log` |
| `cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked` | 15 passed: eight real-binary scenarios, the deterministic final-frame regression, and six other fixture tests; `/tmp/fux-resume-fixture.log` |
| `python3 tools/dependencies.py verify` | Passed; `dependencies.log` |
| `cargo tree --locked --all-features --prefix none` | Passed; no koh or zor packages; `tree.log` |
| `git diff --check` | Passed |
| `cargo build --manifest-path zor/Cargo.toml --locked` | Passed; `zor-build.log` |
| `cargo test --test zor_integration --locked` with exact `ZOR_BIN` and `FUX_REQUIRE_ZOR_BIN=1` | Passed, real observer required; `zor-required.log` |
| `cargo test --manifest-path zor/Cargo.toml --all-features --locked` | 56 passed; `zor-full.log` |
| `cargo check --manifest-path zor/Cargo.toml --no-default-features --all-targets --locked` | Passed; `zor-minimal.log` |
| `cargo test --manifest-path references/koh/Cargo.toml --test gateway --locked` with exact `FUX_BIN` and `KOH_REQUIRE_FUX_BIN=1` | Both passed with real fux required; `/tmp/fux-resume-gateway.log` |
| `cargo test --manifest-path references/koh/Cargo.toml --lib gateway:: --locked` with the same required fux binary | All ten passed, including forced loss and real-fux five-loss reconnect; `/tmp/fux-resume-reconnect.log` |

The initial sandbox denied netmon initialization. After the environment permissions changed,
both commands were rerun successfully with the same pinned owners and exact required fux binary.
No network code or test requirement was weakened. These are fresh non-skipped runtime results.

Additional CI reproduction with installed stable Rust 1.97.1:

- `cargo +stable clippy --locked --all-targets --all-features -- -D warnings`: passed after the
  equivalent test-expression fix (`stable-clippy.log`).
- `cargo +stable test --manifest-path tests/verify/fixture-child/Cargo.toml --locked`: all 15
  passed after the final-frame fix (`/tmp/fux-resume-stable-fixture.log`). Fixture strict Clippy
  also passed under stable (`/tmp/fux-resume-stable-fixture-clippy.log`).
- Root and fixture formatting checks and `git diff --check` passed after that fixture-only fix.
- `cargo test --locked --test client terminal_escape_parameters`: passed after the test-expression
  change (`lint-regression.log`); formatting and diff checks passed again. No production changes
  followed the 258-test final pass.

Final independent review covered all behavioral fixes, the lint fix, new regression harness,
requirement audit and updated checkpoint documents. No confirmed defect or required local check
remains outstanding. There were no production changes after the 258-test root pass; later edits
were the verified test-expression and fixture-observation fixes and documentation.

## Platform and external-state limits

Runtime verification is local macOS. No Linux or Android runtime claim is made. Linux/MSRV and
Android cross-check jobs in .github/workflows/ci.yml describe configured verification, not execution.
The full-hash GitHub query found failed [CI run 33963062875](https://github.com/gold-silver-copper/fux/actions/runs/33963062875)
and [nightly run 33964364576](https://github.com/gold-silver-copper/fux/actions/runs/33964364576)
on checkpoint `1b9488d364002525e6edd85e063e9b1f3ba39c2a`. Individual job logs were retrieved
successfully after the environment change. Linux/macOS stable Clippy failed only on the now-fixed
byte-character array. CI and nightly macOS fixtures failed on the same final-frame observation
race, now deterministically reproduced and fixed. Logs: `/tmp/fux-resume-clippy-job.log`,
`/tmp/fux-resume-macos-clippy-job.log`, `/tmp/fux-resume-fixture-job.log`, and
`/tmp/fux-resume-nightly-job.log`.

Those old hosted results remain failed; local fixes are uncommitted and have no hosted execution.
No PR was requested or created; no review-thread state is claimed. CI reruns/publication are not
authorized by this task. This completion establishes the requested review and local acceptance,
not a release or passing hosted checks for an unpublished revision.
Optional integration loopback tests do not establish relay/NAT, mobile suspend/resume or every
terminal emulator's clipboard behavior. A 1×1 terminal stays safe but cannot show meaningful controls.
