#!/usr/bin/env python3
"""Repair writers: specs that failed mechanical validation, a few per Codex
run, fixed by Codex with `validate-spec` (the validator, run outside its
sandbox) in reach. Fixed specs are copied back into SET/specs; everything is
validated again afterwards.

  repair.py SET PER_JOB WORKERS
"""
import concurrent.futures as cf
import json
import os
import shutil
import subprocess
import sys

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
HERE = os.path.dirname(os.path.abspath(__file__))
AGENT = os.path.join(os.path.dirname(HERE), "agent")
sys.path[:0] = [HERE, AGENT]
import cxagent  # noqa: E402
import speccopy  # noqa: E402

PROMPT = """These test tasks for terminal-multiplexer agents failed mechanical validation. Fix each one so
it is valid, keeping its intent, category and difficulty.

The format and rules are below ("THE RULES"). In short, each task folder has spec.json, fixtures/*.py,
check.py and solve_tmux.sh (a reference solution using `tmux -L ab` only, run as
`bash solve_tmux.sh WORK FIXTURES`). Validation runs the reference solution twice on a fresh private
tmux server, each time on a fresh work directory set up from spec.json, and checks that check.py passes
both times and fails on an empty work directory.

Your sandbox cannot run tmux, but you have a command for this: `validate-spec DIR` runs the full
validation of the task folder DIR outside your sandbox and prints what failed (with the reference
solution's stderr), then PASS or FAIL. Run it on each folder, fix what it reports (a bug may be in the
fixture, the checker, the reference solution or the goal), and repeat until it prints PASS. If a task
cannot be made valid without changing what it tests, say so and leave it.

If a folder passes `validate-spec` for you but failed here, it is flaky: its outcome depends on timing
or machine load. Make the fixture and the reference solution robust (wait for what the program shows,
never for a fixed time; the validator may run with the machine busy). Run `validate-spec` at least twice.

The folders, with what failed:
{failures}

THE RULES
{rules}
"""


def main():
    set_name, per_job, workers = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    val = json.load(open(os.path.join(H, set_name, "validation.json")))
    failing = sorted(k for k, v in val.items() if v["problems"])
    rules = open(os.path.join(HERE, "specgen-prompt.md")).read()
    rules = rules[rules.index("Every task needs"):rules.index("**Test every task")]
    import specgen
    rules += specgen.RULES + specgen.HARD + "\nNo Co-Authored-By or any AI attribution in anything you write.\n"
    base = os.path.join(H, set_name, os.environ.get("ROUND", "repair"))
    jobs = []
    for i in range(0, len(failing), per_job):
        ids = failing[i:i + per_job]
        d = os.path.join(base, f"job{i // per_job:02d}")
        if os.path.exists(os.path.join(d, "report.json")):
            continue
        os.makedirs(d, exist_ok=True)
        for sid in ids:
            dst = os.path.join(d, sid)
            shutil.rmtree(dst, ignore_errors=True)
            speccopy.copy(os.path.join(H, set_name, "specs", sid), dst)
        failures = "\n".join(f"- {os.path.join(d, sid)}: {'; '.join(val[sid]['problems'])[:600]}" for sid in ids)
        open(os.path.join(d, "prompt.md"), "w").write(PROMPT.format(failures=failures, rules=rules))
        jobs.append((d, ids))

    def run(job):
        d, ids = job
        env, helpers = cxagent.writer_env(d, {"validate-spec": os.path.join(HERE, "validate-one")})
        try:
            with open(os.path.join(d, "log.txt"), "w") as log:
                subprocess.run([cxagent.CODEX, "exec", "-m", "gpt-6.1-sol", "-c", 'service_tier="default"',
                                "-c", 'model_reasoning_effort="high"', "-s", "workspace-write", "--ephemeral",
                                "--skip-git-repo-check", "-C", d, "--output-schema",
                                os.path.join(HERE, "specgen.schema.json"), "-o",
                                os.path.join(d, "report.json"), "-"],
                               input=open(os.path.join(d, "prompt.md")).read(), stdout=log,
                               stderr=subprocess.STDOUT, text=True, env=env, cwd=d, timeout=5400)
        except subprocess.TimeoutExpired:
            pass
        finally:
            for h in helpers:
                h.stop.set()
        for sid in ids:
            src = os.path.join(d, sid)
            dst = os.path.join(H, set_name, "specs", sid)
            speccopy.copy(src, dst)
        return d

    with cf.ThreadPoolExecutor(workers) as ex:
        for d in ex.map(run, jobs):
            print("repaired", os.path.basename(d), flush=True)


if __name__ == "__main__":
    main()
