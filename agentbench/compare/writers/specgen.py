#!/usr/bin/env python3
"""bench-4way writers: three Codex runs of ten very difficult tasks (seeds:
docs, issues, workflows) for the main set W, and one run of five for the
pilot set P. PR #160's spec-writer prompt and rules, #162's rules 8-11, and
this round's difficulty and fairness rules. No writer is told which
multiplexers will be compared.

  specgen.py SET [WORKERS]      SET is W or P
"""
import concurrent.futures as cf
import os
import shutil
import subprocess
import sys

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
HERE = os.path.dirname(os.path.abspath(__file__))
AGENT = os.path.join(os.path.dirname(HERE), "agent")

CODEX = shutil.which("codex")

ISOLATION = """

**Isolation (part of this set's rules):** write only from your own knowledge and the seed below. Do not read anything on this machine outside this folder: no other projects, notes, home directories or caches. In particular never look at any terminal multiplexer's source, docs or tests on this machine."""

SEED = {
    "docs": """

**Your seed:** the documentation and examples of tmux, Expect (Don Libes's tool and book), pexpect, prompt_toolkit, GNU readline and curses: the situations their docs and example scripts show automation handling (logins, prompts, REPLs, installers, password prompts, line editing, completion menus, long-running programs, full-screen programs). Turn them into tasks of your own; don't copy any example.""",
    "issues": """

**Your seed:** the problems people report in the issue trackers of terminal emulators, terminal multiplexers, Expect/pexpect and AI agent harnesses that drive terminals, and the kind of interactive situations in Terminal-Bench 2.0's public tasks: automation that broke (missed prompts, echo mistaken for output, keys lost or typed too early, timing, wrapped lines, redraws, resizes, programs that hang, sessions lost). Turn them into tasks of your own; don't copy any report or task.""",
    "workflows": """

**Your seed:** real multi-program developer and operator workflows, simulated locally with fixtures: a dev server with a file watcher and a test watcher, debugging in a REPL or a debugger while a service logs, a database shell beside a migration tool, a deploy script asking for confirmations while a log tail runs, a remote-login-like session (simulated: no network) that must survive a disconnect, a build farm of several workers. Turn them into tasks of your own.""",
    "mixed": """

**Your seed:** any of: the docs of tmux, Expect, pexpect, readline and curses; problems reported against terminal tools and agent harnesses; real multi-program developer workflows simulated locally.""",
}

RULES = """

**More rules (from defects judges found in earlier task sets; every task must follow them too):**
8. Every input a fixture accepts is either shown on its screen or stated exactly in the goal: no hidden command words or syntax (if a prompt wants `save <code>`, the screen or the goal says so).
9. Never decide whether a shell is interactive from its command-line arguments; if a task needs an interactive shell, check `isatty` on its terminal or `$-`.
10. Never compare terminal device names between programs that do not run at the same time (a terminal's number is reused once its program exits).
11. A fixture's own checks must not race: never list files that other fixtures are replacing at that moment; use atomic records (write a temporary file, then rename) and read each by name."""

HARD = """

**These tasks must be very difficult.** Earlier task sets were too easy: good agents finished nearly all of them. Each task here must combine **at least three** of these kinds, and its reference solution must make **at least 15 tmux calls that act** (starting, typing, keys, splitting, resizing; reads such as capture-pane and listings don't count; the validator counts them and rejects shorter ones):
- `several-programs`: several panes or programs at once, where the order of events across them matters (a build in one pane, a server in another, a client in a third);
- `long-horizon`: 20 or more interactions, with state carried between them;
- `recovery`: a program that crashes, hangs or asks for something unplanned partway through, which must be noticed and handled;
- `echo-traps`: prompts that echo typed words, line editors that redraw on every key, erase-and-retype, input typed ahead while a program is busy;
- `overwritten-text`: spinners and progress bars hiding the value needed, decoys printed before the input, output landing just as a command ends;
- `reading`: long wrapped lines, values that scrolled far into history, floods past 1 MiB with a needle in them;
- `full-screen`: a TUI with menus, forms and dialogs, driven by keys and the mouse, possibly nested (a TUI inside a shell inside a TUI);
- `layout-size`: resizes mid-task, a program that needs a given terminal size, programs side by side;
- `sessions`: work that must keep running while the agent is away, finding programs again later, cleanup that must leave nothing running.

A wrong or missing answer from the agent's terminal tool must cost real work: a wrong value in the answer, input sent too early (the fixture records it and the checker fails), or a program to restart. An agent with a good tool must be able to finish without sleeps or guesses. Every task is `hard`.

**Fairness.** These tasks will be run through several different terminal multiplexers. Write them so that any capable multiplexer could do them: state everything in terms of programs and outcomes, and never build a task around one tool's special command or quirk. A task may need a capability that some tools lack (setting a pane's exact size, a bell, mouse clicks): that's allowed, but declare it.

**Two more fields in each `spec.json`:**
- `"kinds"`: the list of the kinds above that the task combines (at least three, using exactly these names).
- `"needs"`: the list of multiplexer capabilities the task needs, using only these names: `start-program`, `type-text`, `special-keys`, `paste`, `wait-text`, `wait-quiet`, `wait-exit`, `exit-status`, `read-screen`, `read-scrollback`, `read-wrapped`, `read-colors`, `mouse`, `set-size`, `resize`, `several-panes`, `side-by-side`, `pane-names`, `detach-reattach`, `bell`, `signals`, `kill-pane`, `title`, `cursor`.
Put in `note` which traps the task sets and why a fixed wait or a guess fails."""

KINDS_SPLIT = {
    10: "Across your ten tasks, use every kind at least three times.",
    5: "Across your five tasks, use every kind at least once.",
}

CATS = {10: {"panes": 2, "agents": 1, "tui": 2, "shell": 1, "wait": 1, "read": 1, "sessions": 1, "scripting": 1},
        5: {"panes": 1, "tui": 1, "shell": 1, "wait": 1, "sessions": 1}}

SETS = {  # set -> [(seed, count, first id)]
    "W": [("docs", 10, 1), ("issues", 10, 11), ("workflows", 10, 21)],
    "P": [("mixed", 5, 1)],
}
PREFIX = {"W": "TW-{:03d}", "P": "TP-{:02d}"}


def main():
    set_name = sys.argv[1]
    workers = int(sys.argv[2]) if len(sys.argv) > 2 else 4
    tmpl = open(os.path.join(HERE, "specgen-prompt.md")).read()
    base = os.path.join(H, set_name, "specgen")
    os.makedirs(base, exist_ok=True)
    work = []
    for j, (seed, count, first) in enumerate(SETS[set_name]):
        d = os.path.join(base, f"job{j:02d}")
        if os.path.exists(os.path.join(d, "report.json")):
            continue
        os.makedirs(d, exist_ok=True)
        fmt = PREFIX[set_name]
        prompt = tmpl.format(count=count, first=fmt.format(first), last=fmt.format(first + count - 1),
                             categories="\n".join(f"- {c}: {k}" for c, k in CATS[count].items()))
        prompt += ISOLATION + SEED[seed] + RULES + HARD + "\n" + KINDS_SPLIT[count] + "\n"
        prompt += "\nNo Co-Authored-By or any AI attribution in anything you write.\n"
        open(os.path.join(d, "prompt.md"), "w").write(prompt)
        open(os.path.join(d, "meta.txt"), "w").write(f"seed={seed}\n")
        work.append(d)
    sys.path.insert(0, AGENT)
    import cxagent

    def run(d):
        env, helpers = cxagent.writer_env(d, {"validate-spec": os.path.join(HERE, "validate-one")})
        with open(os.path.join(d, "log.txt"), "w") as log:
            try:
                p = subprocess.run([CODEX, "exec", "-m", "gpt-6.1-sol", "-c", 'service_tier="default"',
                                    "-c", 'model_reasoning_effort="high"', "-s", "workspace-write", "--ephemeral",
                                    "--skip-git-repo-check", "-C", d, "--output-schema",
                                    os.path.join(HERE, "specgen.schema.json"), "-o",
                                    os.path.join(d, "report.json"), "-"],
                                   input=open(os.path.join(d, "prompt.md")).read(), stdout=log,
                                   stderr=subprocess.STDOUT, text=True, env=env, cwd=d, timeout=3 * 3600)
                return d, p.returncode
            except subprocess.TimeoutExpired:
                return d, "timeout"
            finally:
                for h in helpers:
                    h.stop.set()

    with cf.ThreadPoolExecutor(workers) as ex:
        for d, code in ex.map(run, work):
            print(os.path.basename(d), code, flush=True)


if __name__ == "__main__":
    main()
