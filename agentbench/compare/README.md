# Comparing fux builds, and fux with other multiplexers, where it matters to agents

Sections A to C compare any two fux builds on what a terminal tool for agents must get right: telling the program's own **output** from the **echo** of the keys an agent typed, seeing text drawn and overwritten in one burst, giving answers of the right shape, and what mistakes cost an agent. They were written to check PR #161 against master; the results are in that PR's follow-up (bench-161).

Every server these scripts start is private (its own `XDG_RUNTIME_DIR` and `FUX_SOCKET`), and nothing reaches your own fux session. Scratch goes to `$BENCH_DIR` (default `~/.cache/fux-bench`).

## Build the two binaries

Pin both by commit, from clean trees:

```sh
cd ~/code/fux && for c in master my-branch; do d=$HOME/.cache/fux-bench/src-$c; mkdir -p $d && git archive $c | tar -x -C $d && (cd $d && cargo build --release --locked) && cp $d/target/release/fux $HOME/.cache/fux-bench/fux-$c; done
```

## A. Wrong answers, no agent

Programs that log the truth (`tests/oracle/side-log-format.md`: which bytes they wrote as output, which as their own echo, which input each answers) are driven through fux, and fux's answers are checked against the log.

- **The oracle's own cases** (`tests/oracle.rs`), the same seeds on both builds. `FUX_BIN` runs the test suite's servers on another binary; `ORACLE_BENCH=FILE` runs every case (no time limit, nothing shrunk or saved) and writes each case's checks and disagreements:

  ```sh
  cd ~/code/fux && for b in master my-branch; do FUX_BIN=$HOME/.cache/fux-bench/fux-$b ORACLE_BENCH=$HOME/.cache/fux-bench/A/oracle-$b-101.json ORACLE_SEED=101 ORACLE_RUNS=200 ORACLE_SECS=100000 ORACLE_JOBS=4 cargo test --release --test oracle fuzz -- --ignored --exact; done
  ```

  `a_oracle.py SEED...` compares them per class (expects files named `oracle-base-SEED.json` and `oracle-cand-SEED.json`).
- **Fixture programs** in `fixtures/` (FX-01..10 from #161's test, FG-01..12 written blind for bench-161): `fixture_run.py BIN fixtures OUT.json` runs every scenario, asks fux after each step whether each word the program printed as output was seen and whether each word only typed was taken as output (`0 timeout` waits, which send nothing); `a_fixtures.py` pairs the checks across builds (expects `fix-SET-base.json` and `fix-SET-cand.json`).

## B. What mistakes cost an agent

`tasks/` holds 30 very hard tasks (TD-001..030) written blind for bench-161 in PR #160's task format: each hinges on echo, on text overwritten quickly, or on order, and each has fixtures, a strict checker and a tmux reference solution. `agent/` runs Codex on them through fux (PR #160's agent prompt and wrappers: `cxagent.py`, `batch.py`; it needs the `codex` CLI) and writes review prompts for judges (`mkreview.py`, with the frozen `review-prompt.md` and `rulings.md`). Give the builds opaque names (`fux-x`, `fux-y`) so judges are not told which is newer. `cost.py` reads each run's call log (calls, waits, captures, repeated waits, restarts, sleeps, seconds, tokens); `b_analysis.py` compares the builds: clean and first-try rates (Wilson intervals, Fisher, McNemar on per-task majorities) and Wilcoxon signed-rank on per-task means.

## C. Speed and memory

- `speed.py BASE CAND [ROUNDS]`: PR #160's speed table; `speed_extra.py`: actions the echo and burst cases touch (erase and retype, a line editor redrawing its line, floods); medians, builds alternating, a fresh server each round. `mem.py`: a pane with a full 50,000-line history.
- Instructions: `bench/` (`fux-bench --against REF`). Where `perf` is missing, `perf/perf` stands in for the one command fux-bench runs, through `perf_event_open`, one counter per kind of core (`PATH=agentbench/compare/perf:$PATH`).

## D. fux against tmux, zellij and herdr

The same agent does the same very difficult tasks through each multiplexer: `tasks4/` holds 30 (TW-001..030) written blind for bench-4way, each combining at least three hard kinds (several programs at once, long horizons, recovery from crashes, echo traps, overwritten text, deep reading, full-screen programs, layout and size, sessions), with a tmux reference solution making at least 15 calls that act. `writers/caps.json` says which capabilities each tool has (`yes`, only by a `workaround`, or `no`), and each task lists the ones it needs.

- **Writing a new set** (`writers/`): `specgen.py SET` runs the Codex task writers (PR #160's spec-writer prompt and rules, with this set's difficulty and fairness rules); writers check their own tasks with `validate-spec` (the validator, run outside their sandbox). `validate.py SET` checks every spec outside any sandbox, `repair.py` sends failures back to Codex, `seal.py seal SET` hashes and locks the set, `seal.py check SET` unlocks and compares.
- **Running** (`agent/`): `jobs.py SET REPEATS all > SET/jobs.json` lists every task × repeat × tool, shuffled with a fixed seed; `batch.py SET/jobs.json 8` runs them, 8 at once. `cxagent.py TOOL FUX_BIN TASK WORK` is one run: each tool gets a private server and folders of its own; fux is called directly inside Codex's sandbox, the others through a proxy outside it that streams their output back. Every multiplexer call is logged with its exit status and error, and Codex's event stream gives tokens (input, cached, output) and a readable transcript.
- **Judging** (`agent/`): `judges.py SET SIZE` writes review prompts per tool for Opus judges (frozen `review-prompt.md`, `rulings.md` with the four-tool addendum and its clarification: whether a tool "had a documented way" is decided by `writers/caps.json`, and a workaround the tool forces is graded `worked_polling`, not `hack`). The judges read each tool's docs from `$BENCH_DIR/caps/` (`fux-docs.txt`: `fux help-agents` and `fux help`; `tmux-man.txt`; `zellij-docs.txt`: `zellij --help`, `zellij action --help` and each action's help; `herdr-docs.txt`: `herdr --help`, `herdr pane`, `herdr terminal`, `herdr --skill`). Collect them with a private HOME and config folder, and never run `herdr server` to read its help: it starts a server. `normalize.py SET SIZE` then re-reads every run graded hack under that one reading, and `codex_second.py SET 15 6` re-judges the flagged runs blind with Codex.
- **Analysis**: `w_analysis.py BENCH_DIR/SET OUT` writes `runs.csv` (one row per run: done, grade, seconds, tokens, calls, errors, help reads, sleeps, restarts, deciding layer) and `analysis.md`: rates with Wilson intervals, Cochran's Q and Friedman across the four tools, then fux against each other tool with Fisher, McNemar and Wilcoxon, Holm-corrected; on all tasks, on the tasks every tool can do, by kind and by writer, and a per-task table.

One task on all four tools (needs the `codex` CLI, tmux, zellij and herdr):

```sh
cd ~/code/fux && export BENCH_DIR=$HOME/.cache/fux-bench && cargo build --release --locked -j 4 && for t in fux tmux zellij herdr; do python3 -I agentbench/compare/agent/cxagent.py $t target/release/fux agentbench/compare/tasks4/TW-016 $BENCH_DIR/try/$t/TW-016-r1 | grep -E '"(tool|ok|seconds|tool_calls|tool_errors|tokens)"'; done
```

`stats.py` holds the tests every script uses.
