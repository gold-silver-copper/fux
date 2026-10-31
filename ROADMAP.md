# fux: the roadmap

Each milestone is a **scenario that agents carry out**, never the owner. A
milestone is done when:
- a script checks its end state in the local gate (D-39);
- the residents (`~/code/notes/projects/fux/vision/SYSTEM.md`) have worked with it for three days with no
  blocking obstacle.

Only one milestone runs at a time. Sizes are rough production-line
estimates from the drafts, not measurements.

**The order's main idea: agents move in before the language and the editor
are designed further.** If fuxth and the editor are designed before agents
have lived in fux, they are designed from audits again, and that is the
churn this roadmap exists to end. (AI researcher, strategist, Lisp expert
and Forth expert agree; the original proposal had agents moving in at M4.)

## M0. Stop and unblock (now; days, not a pass)

- **Adopt** `DECISIONS.md`, the standing limits, the review rule and the
  question defaults in `~/code/notes/projects/fux/vision/SYSTEM.md`. After that, a pass never waits overnight
  on a routine question.
- **The simplification pass** is paused after json-string. It resumes
  wave 4 under the new review rule and skips its docs tasks until after M2
  (D-35). M1 starts when that remaining work merges (D-37).
- **Stop:**
  - new audits;
  - thousand-line pass prompts (a milestone gets one page of tasks plus
    `DECISIONS.md`);
  - optimizing the checker, which is being deleted.
- **Fix the agent guide's name everywhere:** the owner's own instructions
  say `fux help-agents`, which fails (`unknown_word`); the command is
  `fux help agents`.

## M1. Ten tabs, no accidents

**Scenario.** The coordinator gives four agents a workspace each in one fux
server. Each does real work there: a build, tests, a TUI. Agent B runs
`fux str A workspace kill` and `fux %1 kill` on a pane outside its
workspace. Both are refused as `out_of_scope`, and nothing outside B's
workspace changes. A held `%3` from before a server restart is refused as
from another server run. The owner attaches and sees all four at work.

**Why first.** Today any client can act anywhere on its server: from a
shell unrelated to it, `fux str victim workspace kill` succeeded with
status 0 (verified on 0.36.0). The only protection for the owner's panes is
a sentence in each prompt.

**What it needs:**
- **Scope:** a client is given (never chooses) the workspace it acts in;
  acts outside it are refused, reads stay global. About +150 to +250 lines.
  Before/after:

  ```sh
  # Before (0.36.0)
  $ fux str victim workspace kill; echo $?
  0
  # After: this agent's tab was started with its scope.
  $ fux str victim workspace kill
  fux: out_of_scope: argument 3: kill: +2 victim is outside this client's scope +3 agentA
  ```

- **Effect lists:** every failing run prints the actions that already took
  effect, bounded (the last 16, and a count of the rest). About +60 to
  +120.
- **Every id fux prints carries its server run** (today handles print bare
  `%1`; a person may still type `%3`).
- **The resident harness** (`~/code/notes/projects/fux/vision/SYSTEM.md`): headless turns driven from an
  inbox, a call log per agent, the D-31 resource groups (pane programs
  included, the shared server in its own scope), admission 2 → 4. It lives
  outside fux.

**Right after M1, one PR splits the fux crate** into wire, server and
client crates (D-30), so residents can work in parallel lanes.

## M2. Agents write fuxth at a prompt, and nothing is silently half-done

**Scenario.** An agent, given only `fux help agents` and fuxth's help,
writes a library word (for example `deliver`, which types a message into
another agent's input once it shows its prompt, then waits for its report).
It tries the word in the `fuxth` REPL, adds it to its profile with
`require`, and uses it for a day. When one of its programs fails after
acting, the error says what already happened, and the agent repairs that
without a fresh `ls`.

**What it needs:** waves 1–2 of `/home/gsc/code/fux/prompts/fuxth-forth.md`
plus the runner, cut down (D-36):
- **In:** step accounting fixed, runtime input checks, code memory in the
  grant, checker deleted, the Forth dictionary, the outer interpreter and
  `evaluate`, and the `fuxth` runner and REPL.
- **Also in:** a one-page `fuxth/SEMANTICS.md` that reviews check against
  (the semanticist's draft fills the prompt's gaps). Also `while … repeat`:
  models write it, and the AI researcher's first draft failed without it.
  And `char-code` for today's `code`, freeing `code … end` for code values.
- **The REPL prints the stack after each line,** the feedback the checker
  used to give.
- **Out until a resident's finding asks:** immediate words and the
  compiler in the library, maps, `defer`/`is`.

Rough size: fuxth production from about 5,550 to about 3,800 lines; the
runner about +600.

## M3. An agent edits a real file in the editor, and changes the editor while it runs

**Scenario.** An agent opens a file from one of the owner's repos in the
editor, makes a real change through named commands, saves it, and the
change passes that repo's CI. It redefines a command (`kill-line`) in the
running editor, and the next C-k uses the new one, while compiled helpers
keep their versions. An edit sent against an old buffer revision is
refused. A crash of the editor loses no saved work.

**What it needs:**
- **`fuxed` (D-38), version 0:** one buffer kind (a list of lines), named
  commands, `describe-key`, `apropos` and `doc`, undo, save by writing a
  temporary file and renaming it, and keymaps that hold names.
- **A precise interface for agents** (D-29): fuxth units sent to the
  running editor, checked against a buffer revision, answered with records.
  Keys call the same commands.
- **From fuxth, as findings ask:** code values (`code … end`, `call`,
  `find`, `execute`) and maps.
- **Out:** syntax highlighting, language servers, modes and packages.

Rough size: about 1,200 lines of fuxth, plus the editor's host program on
the runner.

## M4. Ten residents, five working days

**Scenario.** Ten residents (`~/code/notes/projects/fux/vision/SYSTEM.md`) do the owner's real work for five
working days: fux PRs, fuxth and editor work, and issues in the owner's
other repos. They use fux for every program, the editor for every edit,
and fuxth for repeated work. A separate checker verifies their artifacts.

**Gates:**
- no OOM kill;
- no damage outside a scope;
- at least 60% of active time on primary work;
- every refusal is either correct or a finding with a reproduction;
- the owner spends under 45 minutes a week on decisions.

## 1.0

Freeze, by decision:
- the things table;
- the fuxth core words and `SEMANTICS.md`;
- the agent interface of fux and the editor (words, record fields, error
  codes);
- fuxix and fux-vt.

Not frozen: the frame protocol (it only connects a client and a server
built from the same source), the libraries, and the internals. 1.0 is a
separate owner decision after M4, not automatic.

## Not on this roadmap

- **A human-facing polish milestone** (themes, reload, mouse). It waits for
  findings or an owner request.
