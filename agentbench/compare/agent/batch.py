#!/usr/bin/env python3
"""Runs many agent runs, N at once, resuming: a job whose work directory has
a .result.json is skipped. Jobs come from a JSON list of
[tool, fux_bin, task_ref, work]. One retry for a run Codex didn't finish.

  batch.py JOBS.json [WORKERS]
"""
import concurrent.futures as cf
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cxagent  # noqa: E402


def capacity(work):
    try:
        return "at capacity" in open(os.path.join(work, ".codex.log"), errors="replace").read()
    except OSError:
        return False


def one(job):
    """A run; on a capacity error, waits and starts it again (the run never
    began), up to 6 times; on a timeout or crash, one retry."""
    tool, fux_bin, ref, work = job
    for attempt in range(6):
        r = cxagent.run_one(tool, fux_bin, ref, work)
        if r.get("report") is None and capacity(work):
            time.sleep(60 * (attempt + 1))
            continue
        break
    if r.get("timed_out") or (r.get("report") is None and r.get("codex_exit") not in (0, None)):
        r = cxagent.run_one(tool, fux_bin, ref, work)
        r["retried"] = True
        json.dump(r, open(os.path.join(work, ".result.json"), "w"), indent=1)
    if r.get("report") is None and capacity(work):
        os.remove(os.path.join(work, ".result.json"))  # never ran: left for a later batch
    return r


def main():
    jobs = json.load(open(sys.argv[1]))
    workers = int(sys.argv[2]) if len(sys.argv) > 2 else 16
    todo = [j for j in jobs if not os.path.exists(os.path.join(j[3], ".result.json"))]
    print(f"{len(todo)} of {len(jobs)} runs to do, {workers} at once", flush=True)
    done = len(jobs) - len(todo)
    with cf.ThreadPoolExecutor(workers) as ex:
        futs = {ex.submit(one, j): j for j in todo}
        for f in cf.as_completed(futs):
            j = futs[f]
            done += 1
            try:
                r = f.result()
                g = (r.get("report") or {}).get("grade", "-")
                print(f"[{done}/{len(jobs)}] {'ok ' if r['ok'] else 'FAIL'} {r['bin']:<9} {r['id']:<28} "
                      f"{r['seconds']:>6}s calls={r['tool_calls']} sleeps={r['sleeps']} grade={g} "
                      f"{'' if r['ok'] else r['why'][:70]}", flush=True)
            except Exception as e:
                print(f"[{done}/{len(jobs)}] harness error {j[2]}: {type(e).__name__}: {e}", flush=True)


if __name__ == "__main__":
    main()
