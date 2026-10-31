# fux: decisions

**The rules of this file.**
- It is append-only. Every agent's prompt includes it.
- **The owner is never asked a question.** Every open question is settled
  by the ten-agent decision panel (`~/code/notes/projects/fux/vision/SYSTEM.md`, "The decision panel"), and
  the result is recorded here with its vote count. The owner may overrule
  any entry at any time by saying so; nobody waits for that.
- The panel can't change the owner's standing rules (their global
  instructions) or an entry marked "owner"; it decides everything else.
- A decision is reopened only with new evidence from use (a failed
  scenario, or a resident's finding with a reproduction), never because a
  fresh audit prefers otherwise.
- Superseding a decision appends a new entry that names the one it replaces,
  the evidence and the cost.
- "Source" names where it was decided. Drafts:
  `~/code/notes/projects/fux/vision-2026-10-30/`.

## Settled

| Id | Decision | Reason | Source |
|---|---|---|---|
| D-1 | **Agents are the daily users;** about ten live in fux, the editor and fuxth doing real work. The owner sets direction and decides, and is not the tester | Evidence must come from use, and the owner's time is the scarcest resource | owner, 2026-10-30 |
| D-2 | **One server per socket, single-threaded; fuxth runs only in clients,** never in the server | Language growth can't touch server latency or determinism | 10-26 vision; architect, computer scientist |
| D-3 | **fux, fuxth and the editor are independent programs.** No project holds code specific to another tool | Owner's standing rule | owner |
| D-4 | **A config affects only its own client,** and is staged: a failed config publishes nothing. The CLI never loads a personal config | One caller's config must not change another's meaning | 10-26 decision 4; #275, #280, #282 |
| D-5 | **Names are given, never made.** Lookups never guess | No guessing | 10-26 decision 6; #271, #272 |
| D-6 | **Facts, never judgements:** no readiness, idleness or prompt heuristics, ever | Agents and fuxth decide; fux reports | #224; 10-26 |
| D-7 | **Every fux-vt feature stays,** including sixel and the old DEC families | Owner's decision; a line count doesn't license cutting a feature | 10-26 decision 1 |
| D-8 | **No mouse-first UI, no floating panes** | Rejected | 10-26 |
| D-9 | **The whole-program checker is dropped.** fuxth runs words as it reads them; an earlier action can happen before a later error. Each word still checks its own input before acting; config stays staged | One extensible execution model; agents recover from visible errors (95 of 98 tasks passed either way, #183) | owner, 2026-10-30; `prompts/fuxth-forth.md` Part 0 |
| D-10 | **Typed values, resumable machine, hard bounds,** no raw memory or addresses | The constraints of an embedded language that has external effects | owner, 2026-10-30 |
| D-11 | **Calls bind early; late lookup is explicit** (`find`, `execute`; editor keymaps and hooks hold names) | Forth's rule: helpers stay predictable, live extension happens at chosen points | owner, 2026-10-30 |
| D-12 | **`evaluate` never in fux;** the standalone runner and the editor enable it | Text from a screen must never become code in fux | owner, 2026-10-30 |
| D-13 | **Shell-friendly spelling:** `define … end`, no `;` or bare brackets in core words | fux command lines are typed into a shell | `fuxth-forth.md` Part 0 item 9 |
| D-14 | **Behaviours kept from the simplification pass stay with new mechanisms:** library words refuse wrong inputs; a caught failed wait still gets its answer; request kinds stay in fux | The owner said no to dropping them | `simplify/owner-decisions.md`; `fuxth-forth.md` Part 0 item 8 |
| D-15 | **Confirmation is a one-row chooser** (Enter accepts) | Owner's call | `simplify/owner-decisions.md` |
| D-16 | **Pre-release:** no backwards compatibility, aliases, deprecation or migration | Owner's standing rule | owner |
| D-17 | **No paid agentbench runs or benchmark campaigns** by default | Owner's standing rule | owner |
| D-18 | **The simplification pass is paused after json-string** for this vision | Waves 4–5 may be reshaped | owner, 2026-10-30 |
| D-19 | **Never ask the owner.** Every question is settled by a panel of ten agents of different perspectives and models; the owner reads the outcome and may overrule | As agentic as possible | owner, 2026-10-30 |

## Settled by the decision panel, sitting 1 (2026-10-30)

Ten fresh panelists, one per perspective, on Opus, Fable, Sol and Astra.
Ballots: `~/code/notes/projects/fux/vision/panel/ballot-1.md` and `~/code/notes/projects/fux/vision/panel/ballot-2.md`; votes and reasons:
`~/code/notes/projects/fux/vision/panel/r1/`, `~/code/notes/projects/fux/vision/panel/r2/`; counts: `~/code/notes/projects/fux/vision/panel/tally-r1.txt`, `~/code/notes/projects/fux/vision/panel/tally-r2.txt`.

| Id | Decision | Vote | Strongest dissent |
|---|---|---|---|
| D-20 (B-1) | **Standing bands:** an instruction-count change within ±2% on the named workloads, a line count inside the task's stated range, and a cost inside a range already accepted for the same PR are accepted and recorded, never asked | 10–0 | — |
| D-21 (B-2) | **The review rule:** one reviewer of another model, two for interpreter state, resource bounds, the wire or editor undo. A must-fix needs a failing scenario or test, a reproduced bug or a broken decision id; anything else is a note. At most two rounds. Universal properties become structural | 9–1 | power user: scope enforcement and editor save also get two reviewers |
| D-22 (B-3) | **Audits only at a milestone's end,** at most three perspectives, limited to its scenarios and findings, each finding with a reproduction | 10–0 | — |
| D-23 (B-4) | **Agents share one server,** each in its own workspace, once scope (M1) exists and resource containment of their pane programs is verified; private servers until then, and always for fux's own destructive tests | 8–2 (both dissents are this condition, folded in) | computer scientist, power user: only after containment is verified |
| D-24 (B-5) | **At least three residents work on the owner's other repos** | 10–0 | — |
| D-25 (B-6) | **The effect list replaces "refused before anything runs":** a failing run lists the actions that already took effect, and help says so | 10–0 | — |
| D-26 (B-7) | **Every id fux prints carries its server run;** a person may still type a bare `%3` | 10–0 | — |
| D-27 (B-8) | **Names:** paste buffers become **clips**; fuxth's long-lived object is **the dictionary**; today's `code` becomes **`char-code`**, so `code … end` means a code value; the runner's words are **`getenv`** and **`draw-frame`** | 10–0 on each | — |
| D-28 (B-9) | **Where the documents live:** `VISION.md`, `ROADMAP.md`, `DECISIONS.md` at the fux repository root, committed; `~/code/notes/projects/fux/vision/SYSTEM.md`, ledgers and findings in `~/code/notes/projects/fux/` | 10–0 | — |
| D-29 (Q-11) | **Agents drive the editor through a socket that takes fuxth units,** checked against a buffer revision and answered with records; keys call the same commands | 10–0 | — |
| D-30 (Q-12) | **Split the fux crate after M1, in one PR:** wire, server (enforced to have no fuxth interpreter) and client crates | 8–2 | Forth expert, strategist: cut the 14 edges and add a CI layering check instead, without three manifests |
| D-31 (Q-13) | **The population's machine budget:** 10 residents, at most 4 in a turn at once; at most 2 cargo builds at once at `-j 8`; 4 GiB per resident and 48 GiB for the cohort; no new turn below 24 GiB free. The whole cohort, coordinator and steward included, is limited to 16 CPUs. Programs a resident starts in fux panes are placed in that resident's resource group before they run. The shared fux server starts first, in its own capped scope outside every resident's. The owner's terminal stays outside the cohort | round 2: 10–0 | round 1 was 4–6 on the additions; all moved to them |
| D-32 (Q-14) | **Spending in week one:** log turns and tokens per resident, with a cohort ceiling of 400 turns a day; set the ceiling from what week one cost | 7–3 | AI researcher, computer scientist, strategist: also a USD 50 a day ceiling, whichever comes first |
| D-33 (Q-15) | *Superseded by D-38.* **The editor:** program **`fed`**, package **`fux-fed`** (crates.io's `fed` is an unrelated 2017 crate), a crate in this repository beside `fuxth/`, depending only on `fuxth`, the runner, `fux-vt` and `fuxix` | round 2: 6–2–2 | Forth expert, semanticist: `fuxed` for both, one name for one thing; ontologist, power user: package `fed-editor` |
| D-34 (Q-16) | **1.0 freezes** the things table, the fuxth core words and `SEMANTICS.md`, the agent interface (words, record fields, error codes), fuxix and fux-vt; not the frame protocol, the libraries or the internals. 1.0 itself is decided after M4 | 10–0 | — |
| D-35 (Q-17) | **The rest of the simplification pass:** resume wave 4 under D-21 (one-attach, events, tab-creation, confirm-chooser, caps-and-codes, encoders); keep session-tests and client-tests; drop help-and-docs and comments until after M2 | 8–2 | AI researcher, strategist: stop the pass and fold its tasks into the milestones |
| D-36 (Q-18) | **The fuxth prompt is cut to M2's scope:** waves 1–2 and the runner, plus `SEMANTICS.md`, `while … repeat` and `char-code`. Code values, maps, immediate words and `defer` wait for residents' findings | 9–1 | computer scientist: include code values and fux callbacks as code in M2 |
| D-37 (Q-19) | **M1 starts right after the simplification pass's remaining work** under D-35 | 7–3 | AI researcher, Lisp expert, strategist: start M1 now (the Lisp expert: at least its resident harness, which lives outside fux) |
| D-38 | **The editor is `fuxed`,** for both the program and the package (free on crates.io, checked 2026-10-30), a crate in this repository beside `fuxth/`, depending only on `fuxth`, the runner, `fux-vt` and `fuxix`. Supersedes D-33's `fed`/`fux-fed` | owner's choice, the Forth expert's and semanticist's round-2 vote: one name for one thing | owner, 2026-10-30 |
| D-39 | **The local test suite is the merge gate, not CI.** Before merging, run `~/code/notes/projects/fux/live/gate.sh` in the PR's worktree (CI's checks for what the branch changed, parallelism capped); merge when it passes. Nobody waits on CI; it still runs after merging, as a backstop, and a failure there becomes a fix-forward task. No long benchmarks or campaigns as part of the gate | Work fast; CI waits held merges for tens of minutes | owner, 2026-10-30 |
| D-40 | **All code is written by Claude Opus.** Implementers, fix-ups and any agent that edits code are Opus. Sol, Astra and Fable review, research and sit on the decision panel. D-21's "a reviewer of another model" therefore means a non-Opus reviewer | Opus is the stronger coder | owner, 2026-10-30 |
| D-48 | **Every fix is checked agentically, with its own build of fux.** Before a PR merges, a verifier agent builds the PR's binary in its own target dir, starts a private server with it, and exercises the change's behaviour through fux itself (driving panes, programs and the words the PR changed), as an agent would use it. It writes a short transcript of what it ran and saw into the PR's review notes. Tests and the local gate (D-39) still run; this is in addition. The verifier is any model, since it writes no code (D-40) | Fixes must be shown working in the real tool, not only in tests | owner, 2026-10-31 |

## Settled by the decision panel, sitting 2 (2026-10-30): how fux gets finished

Ballot, votes and reasons: `~/code/notes/projects/fux/live/panel/2026-10-30-s2/`.

| Id | Decision | Vote | Strongest dissent |
|---|---|---|---|
| D-41 (S2-1) | **The simplification pass ends after `events`.** `tab-creation` and `confirm-chooser` move into M0c; caps-and-codes' K10 (connection and pane admission caps) moves into M0b and its K11 (one bound, one refusal code) into M0c's error-code list; `caps-and-codes`, `encoders`, `session-tests` and `client-tests` are dropped | round 2: 7–3 (all ten end the pass after `events`; the three plain (a) votes asked for the same carry-over) | — |
| D-42 (S2-2) | **fux-vt and fuxix are frozen:** only bug fixes and reproduced findings change them. M0b bounds a pane at the server's boundary from fux-vt's documented capacity formulas (`fux-vt/THREATS.md` §3); a failed peak test is a reproduced finding that may change fux-vt | round 2: 8–2 | two: let fux-vt change for M0b's bounds work |
| D-43 (S2-3) | **M0b, bounds by construction, runs before M1:** a budgeted region per fuxth run, per request and per pane; growable stores only through bounded types; a peak-memory test per component; delete the per-site caps the structure makes redundant | 10–0 | — |
| D-44 (S2-4) | **M0c, a finite interface, runs before M1:** the crate split (D-30, moved here), the server's requests, fux's words, record fields and error codes written as generated lists that every PR diffs, and the client-follows-server fallback (a client of another protocol runs the server's own binary, so it speaks the server's vocabulary). The interface freezes after M1, because M1 changes requests (scope, run-qualified ids, effect lists) | 7 for this form, 3 for freezing before M1 | three: freeze before M1 |
| D-45 (S2-5) | **M0c first, then M0b:** the split gives M0b clean lanes and smaller PRs | 8–1–1 | strategist: the mechanical split first, then M0b, then the rest of M0c |
| D-46 (S2-6) | **Line count is a gate only for pure cleanup tasks;** correctness and bounds work is judged by its tests and by "nothing slower". Each PR still reports its counts | 10–0 | — |
| D-47 (S2-7) | **fux 1.0:** the panel declares it after M0c, M0b and M1 pass and the residents work a week with nothing blocking. It freezes D-34's list for fux. After it, fux takes only bug fixes and reproduced findings, and new work goes to fuxth and fuxed, which have their own 1.0 later | 7–3 | AI researcher: only after a week of all ten residents |

## Settled by the decision panel, sitting 3 (2026-10-31): fux's display, and how agents see fux

Ballot, votes and reasons: `~/code/notes/projects/fux/live/panel/2026-10-31-s3/`.
Proposals: `~/code/notes/projects/fux/live/findings.md` F-7 to F-14.

| Id | Decision | Vote | Strongest dissent |
|---|---|---|---|
| D-49 (S3-1) | **The bar is hidden by default.** The bar, tab list and notices draw over the panes while the prefix is in use, then the covered rows are repainted. A bell or notification shows a brief corner notice that takes no layout space. Pane sizes never change because of fux's own UI. The default lives in the fuxth library; `bar on` restores today's row | 9–1 | power user: the notice should stay as a corner badge naming the tabs with unseen bells until this client views them, rather than fade |
| D-50 (S3-2) | **Milestone M1b, after M0b and before M1's resident week:** prompt marks (D-51), changed rows (D-52) and joined lines (D-53), so they land before the interface freezes (D-44). Styling marks (D-54) join it. The other representation proposals wait for residents' findings | 8–1–1 | ontologist: include the facts header (F-13) too |
| D-51 (S3-3) | **Shell prompt marks (OSC 133) are recorded as facts.** fux records the marks a program sends and answers from them (the last command's output and exit status). It never infers a prompt itself | 10–0 | — |
| D-52 (S3-4) | **"What changed since version V" answers only the changed rows,** bounded like any capture | 10–0 | — |
| D-53 (S3-5) | **Text captures rejoin soft-wrapped lines** into logical lines by default and state the width once; the raw grid is available on request | 10–0 | — |
| D-54 (S3-6.2) | **Captures mark what only styling carries** (faint, as reverse already is, and what the program drew as a selection), on request through `marks`. A mark says "rendered faint", never "placeholder" | round 2: 9–1 | — |
| D-55 (S3-6) | **Wait for residents' findings:** compact default answers (F-11, 7–3), a facts header on captures (F-13, round 2: 8–2), text stand-ins for pictures (F-14, 7–2–1) | as listed | three each wanted F-11 now; two wanted F-13 now |
| D-56 | **Correction to D-51 and D-53: both already exist on master.** Prompt marks: `last-output` captures the last command's output and `str last_status get` its exit status, from OSC 133. Joined lines: text captures join wrapped rows by default (`wrapped` gives the raw rows). The proposals were written from the installed 26 October binary. M1b verifies and documents them, and builds only D-49, D-52 and D-54. Any proposal now gets checked against master's `fux help` before it goes to a panel | the steward's mistake, found when writing M1b's page | steward, 2026-10-31 |

## Recorded disagreements (for later evidence, not to reopen now)

- **A pre-run lint as a separate tool** (computer scientist, Lisp expert:
  yes, never as a gate; `fuxth-forth.md` Part 4 bans scans). Recorded as:
  build one only when a resident's finding asks, outside the language and
  CLI.
- **`defer`/`is`** (Lisp expert: no, names in maps are enough). Out of M2;
  added only on a finding.
- **Whether the editor's own fuxth may be live-redefined by other agents.**
  Settled direction: each agent has its own dictionary; sharing an
  extension is a reviewed source change (Forth expert, semanticist).
