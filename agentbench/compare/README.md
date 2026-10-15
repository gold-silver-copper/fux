# Comparing two fux builds where it matters to agents

These benchmarks compare any two fux builds on what a terminal tool for agents must get right: telling the program's own **output** from the **echo** of the keys an agent typed, seeing text drawn and overwritten in one burst, giving answers of the right shape, and what mistakes cost an agent. They were written to check PR #161 against master; the results are in that PR's follow-up (bench-161).

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

`stats.py` holds the tests every script uses.
