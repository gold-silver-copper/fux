You are writing test tasks for AI agents that drive terminal programs through a terminal multiplexer (a program like tmux or screen that runs programs in panes and lets you send keys, read the screen and wait for output). The tasks will measure how well agents manage with different multiplexers, so every task must be stated in terms of **programs and outcomes**, never in terms of any multiplexer's commands. Do not name any multiplexer in the goal.

Write {count} tasks, ids {first} to {last} (use exactly these ids), in these categories (how many of each):
{categories}

Category meanings:
- start: starting and supervising programs: terminal size, environment variables, working directory, exit status, crashes during startup.
- wait: waiting for output, for a prompt, for the output to go quiet, for exit, for whichever of several outcomes comes first, with a deadline.
- read: reading the screen, scrollback far back, wide or wrapped lines, text without colours, the last N lines, a value that changes.
- input: keys, text, pasting multi-line text, control characters, answering prompts, special keys (arrows, function keys, alt, ctrl).
- tui: menus, forms, modals, editors, pagers, full-screen redraws, the alternate screen, mouse clicks.
- panes: several programs together: a build, a test run and a server; output of one feeding another; ordering by readiness; layouts.
- shell: commands in an interactive shell running in a pane, their output and exit status, long-running commands, shell prompts.
- signals: interrupting with Ctrl-C, programs that ignore it, clean shutdown, programs that die, panes whose program has exited.
- sessions: programs that must keep running while nobody watches, finding a pane again by its name or what it runs, renaming, persistence across the agent's own disconnects.
- scripting: tasks that need a loop or condition across many panes or many outputs (for each of 8 workers, wait until each reports done, then collect results in order).
- agents: supervising another agent-like program that asks for confirmations, plans, permissions, or gets stuck.
- many: 10 to 30 programs at once, fan out and collect.

Every task needs its own small **fixture programs**: fake terminal programs written in Python 3 (standard library only, no network, no randomness except seeded) that behave the same on every run: slow printers, REPLs with prompts, TUIs that redraw (use curses or raw ANSI), programs that crash on cue, password prompts, progress bars, programs that ignore SIGINT, programs that print a lot, programs that wait for exact input. Make them realistic: real programs have prompts, colours, redraws and delays. Timing must not decide the outcome: if a fixture prints something after a delay, the task must be solvable by waiting for the output, and must fail if the agent acts too early or guesses. Include delays where real programs have them (startup, compiling), up to about 20 seconds, so that a fixed short sleep would not work.

For each task, make a folder `<id>/` here with:
1. `spec.json`:
   `{{"id": "<id>", "title": "...", "category": "...", "difficulty": "easy|moderate|hard", "goal": "...", "setup": {{"files": {{"name": "content"}}}}, "note": "..."}}`
   - `goal` is exactly what the agent is told. Refer to a fixture as `python3 -I {{fixtures}}/NAME.py ARGS` and to the run's work directory as `{{work}}` (both are replaced with real paths at run time). Say what must happen and what must be written where (usually `{{work}}/answer.txt`, or files the fixtures write themselves). The goal must be unambiguous: one correct final state.
   - `setup.files` (optional): files placed in the work directory before the run.
   - `note`: why the checker is right, and why a fixed wait or a guess would fail.
2. `fixtures/NAME.py`: the fixture programs. Each takes the work directory as an argument when it needs to record anything. A fixture records what the checker needs (exit codes it was asked to report, what it received, its pid in `{{work}}/.fx/NAME.pid` so leftovers can be killed) only under `{{work}}/.fx/`. Fixtures write nothing outside the work directory.
3. `check.py`: a function `def check(work: str) -> tuple[bool, str]` that reads the final state of the work directory (answer.txt, files, `.fx/` records) and says whether the task was done, with a short reason. Strict: the exact expected value, computed from the fixture's deterministic behaviour.
4. `solve_tmux.sh`: a reference solution as a shell script using tmux only, run as `bash solve_tmux.sh WORK FIXTURES` with a private server: always `tmux -L ab`. It may use sleeps and polling (it only proves the task can be done). It proves the task is solvable and the checker is right.

**Rules every task must follow** (from a review of earlier tasks):
1. If the checker allows each program to be launched only once, the goal says so ("run each program exactly once"). Otherwise the checker accepts earlier failed attempts.
2. No hidden deadlines. Every time limit is stated in the goal and is at least 60 seconds, or the program simply waits until its input arrives.
3. Every fixture refuses to run without a terminal (stdin and stdout must be ttys), and the checker confirms (from the fixture's own record) that it ran in one. Redirecting output to a file must not solve a task.
4. The goal never gives away a value the agent is meant to observe (an id, a code, an exit status, a count): it uses placeholders (`status=<observed exit status>`), and the values come from the fixture (derived from its inputs or a seed), not from the goal.
5. The answer format is exact: say how many lines, that each ends with a newline, and whether marker lines are included.
6. Fixtures accept every common encoding of a key or input: Enter as CR or LF, pasted text with CR or LF line ends (with or without bracketed-paste markers), F-keys and arrows in both their ESC O and ESC [ forms, mouse clicks in SGR and legacy encodings. They check meaning, not raw bytes.
7. Records ignore line-editing details (backspace, Ctrl-U, retyping): what counts is the final submitted input.

**Test every task yourself before you finish.** Your sandbox cannot run tmux, but you have a command for this: `validate-spec DIR` runs the full validation of the task folder DIR outside your sandbox: it sets up a fresh work directory from spec.json, runs your reference solution on a fresh private tmux server (twice, to catch timing luck), runs check.py (it must pass both times), and runs check.py on an empty work directory (it must fail). It prints what failed, with your reference solution's stderr, then PASS or FAIL. Fix anything that fails and run it again until every task prints PASS. Do not use the network. Do not write outside this folder.

Variety matters more than anything: different programs, different kinds of output, different traps. Avoid trivial tasks; about a third should be hard (several programs, several steps, real traps). Do not copy well-known benchmark tasks.

Report, in the output schema, each task's id and whether its own test passed.
