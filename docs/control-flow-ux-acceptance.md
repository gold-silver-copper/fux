# Control-flow UX acceptance evidence

The prompt's implementation and verification work is complete within its UX scope.
The complete suite is not green: four known unrelated zor routing fixtures fail.
See [final verification](control-flow-ux-final-verification.md) for exact commands,
results, review scope, retained evidence and limitations, and the
[mode matrix](control-flow-mode-coverage.md) for per-action assertions and shared
handler coverage dispositions.

| Prompt requirement | Final evidence |
| --- | --- |
| Explicit behavior and ownership contract | `control-flow-transitions.md`; updated bindings/hints/manual steps |
| Independent pane/viewer histories and one-Escape normal input | Controller tests; history-controls and reporting-app PTY, including nested unequal panes |
| Copy/q/copy success/prefix/selection clear | Parser/controller tests; real clipboard and exact next-input reporting-app assertions |
| Mouse focus, Shift override, capture and first fresh gesture | Application byte logs, popup auxiliary handoff and lost-release tests; layout/selection PTY |
| Resize while scrolled/selecting, tiny restoration | Geometry/clamp tests; differently sized viewers and tiny-layout PTY; inspected restored frames |
| Every transient mode | 22-action Escape matrix plus per-mode completion/outside/target-loss/failure dispositions in mode coverage |
| Responsive delayed operations, correlation and bounded queues | Delayed history/control/manager fixtures; fixed timeout under ongoing frames, B progress, stale epochs and buffer reply/state ordering |
| Fair scroll scheduling and clamps | Finite visible-session round robin, one read per session, eight-read capacity, sent/desired offset and burst/clamp tests |
| Input boundaries, paste and exact delivery | Every-byte boundary parser matrix; exact raw app receipts; canceled unfinished paste cannot leak or execute prefix |
| Existing detach, transfer, startup, viewers and mouse regressions | Final local 18/18 and automation 19/23, without exclusions; four baseline failures retained |
| Formatting, lint and standalone harness | 191 library tests; 67 harness tests; both all-target Clippy and formatting checks passed |
| Fresh visual evidence | 373 final integration frames and 39 supplemental frames replayed; all 27 labeled contact sheets inspected |
| Audit dispositions and separate review | Final verification review record; historical findings preserved in audit; no unresolved confirmed in-scope defect |

This is headless protocol/rendering evidence. It does not certify native OS event
translation or behavior beyond the documented contract.
