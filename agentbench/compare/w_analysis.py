#!/usr/bin/env python3
"""bench-4way analysis: one row per run (runs.csv), then the frozen tests.

  w_analysis.py BENCH_DIR OUT_DIR

BENCH_DIR holds W/specs (unsealed), W/runs/<tool>/<task>-r<N>/ (each with
.result.json, .calls.log), W/review-<tool>/batch-*.json (the judges), and
W/caps.json ({tool: {need: "yes" | "workaround" | "no"}}). Writes
OUT_DIR/runs.csv and OUT_DIR/analysis.md.
"""
import csv
import glob
import json
import os
import re
import statistics
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import stats  # noqa: E402

TOOLS = ["fux", "tmux", "zellij", "herdr"]
OTHERS = TOOLS[1:]
KINDS = ["several-programs", "long-horizon", "recovery", "echo-traps", "overwritten-text", "reading",
         "full-screen", "layout-size", "sessions"]
WAIT = re.compile(r"(^|\s)(wait|wait-text|wait-exit|wait-bell|wait-quiet|wait-output|wait-for|subscribe)(\s|$)")
HELP = re.compile(r"(^|\s)(--help|-h|help|help-agents|explain|--skill|list-commands|lscm)(\s|$)")


def writer_of(tid):
    """The writer's seed, from the id ranges they were given."""
    m = re.match(r"TW-(\d+)", tid)
    if not m:
        return "pilot"
    return ["docs", "issues", "workflows"][min(2, (int(m.group(1)) - 1) // 10)]


def load_reviews(base, tool):
    """The first judges' verdicts, with the normalisation pass's grade (over
    every run first graded hack) laid over them; the first grade is kept."""
    out = {}
    for f in sorted(glob.glob(os.path.join(base, f"review-{tool}", "batch-*.json"))):
        for k, v in json.load(open(f)).items():
            out[os.path.basename(k.rstrip("/"))] = dict(v, first_grade=v.get("grade"))
    for f in sorted(glob.glob(os.path.join(base, f"normal-{tool}", "batch-*.json"))):
        for k, v in json.load(open(f)).items():
            name = os.path.basename(k.rstrip("/"))
            if name in out and v.get("grade"):
                out[name]["grade"] = v["grade"]
    return out


def calls(work, tool):
    rows = []
    try:
        for line in open(os.path.join(work, ".calls.log"), errors="replace").read().splitlines():
            c = line.split("\t")
            if len(c) > 2:
                rows.append(c)
    except OSError:
        pass
    mine = [c for c in rows if c[1] == tool]
    # A bare subcommand that answers with its usage (herdr exits 2 for that)
    # is a help read, not an error.
    usage = lambda c: (len(c) > 5 and len(c[2].split()) <= 4
                       and re.match(r"\s*(usage|Usage|USAGE|\S+ \S+ commands:)", c[5]) is not None)
    # A call the agent stopped itself (killed: 137, 143), such as a zellij
    # subscribe stream ended by the agent's script once it matched, is
    # neither an error nor a missed wait.
    failed = lambda c: len(c) > 3 and c[3] not in ("0", "", "137", "143")
    # A wait that ends without its condition (a timeout; fux's statuses 3, 4
    # and 7: exited, closed, or waiting for input first) is an answer, counted
    # apart from errors.
    # In the fux these runs recorded, every action waited for its pane to
    # settle, and its statuses 3, 4, 7 and 124 said what the pane did
    # instead (exited, closed, reading, still busy): answers, like another
    # tool's wait timing out. Today's fux answers 4 and 124 alone.
    wait = lambda c: WAIT.search(c[2]) is not None or (tool == "fux" and c[3] in ("3", "4", "7", "124"))
    return {
        "calls": len(mine),
        "help_calls": sum(1 for c in mine if HELP.search(c[2]) or (failed(c) and usage(c))),
        "nonzero": sum(1 for c in mine if len(c) > 3 and c[3] not in ("0", "")),
        "errors": sum(1 for c in mine if failed(c) and not usage(c) and not wait(c)),
        "wait_misses": sum(1 for c in mine if failed(c) and wait(c)),
        "sleeps": sum(1 for c in rows if c[1] == "sleep"),
        "sleep_s": round(sum(float((c[2].split() or ["0"])[0]) for c in rows
                             if c[1] == "sleep" and re.fullmatch(r"[\d.]+", (c[2].split() or ["x"])[0])), 1),
    }


def man_reads(work):
    try:
        return len(re.findall(r"^exec\n.*\bman\s", open(os.path.join(work, ".codex.log"), errors="replace").read(),
                              re.M))
    except OSError:
        return 0


def table(base):
    specs = {}
    for d in glob.glob(os.path.join(base, "specs", "T*")):
        s = json.load(open(os.path.join(d, "spec.json")))
        specs[s["id"]] = s
    rows = []
    for tool in TOOLS:
        rev = load_reviews(base, tool)
        for work in sorted(glob.glob(os.path.join(base, "runs", tool, "*-r*"))):
            name = os.path.basename(work)
            if "." in name or not os.path.exists(os.path.join(work, ".result.json")):
                continue
            r = json.load(open(os.path.join(work, ".result.json")))
            tid, rep = name.rsplit("-r", 1)
            j = rev.get(name) or rev.get(f"{tid}-r{rep}") or {}
            agent_grade = (r.get("report") or {}).get("grade", "")
            grade = j.get("grade") or ("" if not r["ok"] else agent_grade)
            if not r["ok"] and grade in ("worked", "worked_polling", "hack", ""):
                grade = "failed"
            c = calls(work, tool)
            err = j.get("errors") or {}
            u = r.get("usage") or {}
            spec = specs.get(tid, {})
            restarts = j.get("restarts", 0) or 0
            rows.append({
                "task": tid, "tool": tool, "repeat": int(rep), "writer": writer_of(tid),
                "kinds": " ".join(spec.get("kinds", [])), "done": int(bool(r["ok"])), "grade": grade,
                "agent_grade": agent_grade, "first_grade": j.get("first_grade", ""), "valid": int(j.get("valid", True)),
                "first_try": int(bool(r["ok"]) and restarts == 0 and not r.get("retried")),
                "seconds": r.get("seconds"), "timed_out": int(bool(r.get("timed_out"))),
                "tokens": r.get("tokens", 0), "input_tokens": u.get("input", 0),
                "cached_tokens": u.get("cached", 0), "output_tokens": u.get("output", 0),
                "reasoning_tokens": u.get("reasoning", 0),
                "calls": c["calls"], "help_calls": c["help_calls"] + man_reads(work), "nonzero_exits": c["nonzero"], "errors": c["errors"], "wait_misses": c["wait_misses"],
                "errors_agent": err.get("agent", ""), "errors_tool": err.get("tool", ""),
                "errors_wait_timeout": err.get("wait_timeout", ""), "rewaits": j.get("rewaits", ""),
                "sleeps": c["sleeps"], "sleep_s": c["sleep_s"], "restarts": restarts,
                "layer": j.get("layer", ""), "cause": j.get("cause", ""),
            })
    return rows, specs


def fmt_p(p):
    return "<0.0001" if p < 1e-4 else f"{p:.4f}" if p < 0.01 else f"{p:.3f}"


def verdict(p_adj, favours_fux):
    if p_adj >= 0.05:
        return "no difference shown"
    return "**fux better**" if favours_fux else "**fux worse**"


def per_task(rows, tool, key, tasks):
    out = []
    for t in tasks:
        v = [r[key] for r in rows if r["tool"] == tool and r["task"] == t and r[key] not in ("", None)]
        out.append(statistics.mean(v) if v else 0.0)
    return out


def analyse(rows, tasks, title):
    rows = [r for r in rows if r["task"] in tasks]
    md = [f"### {title} ({len(tasks)} tasks, {len(rows)} runs)", ""]
    if not tasks:
        return md + ["(none)", ""]
    md += ["| Measure | " + " | ".join(TOOLS) + " |", "|---|" + "---|" * len(TOOLS)]
    run = {t: [r for r in rows if r["tool"] == t] for t in TOOLS}

    def rate(t, f):
        k, n = sum(f(r) for r in run[t]), len(run[t])
        lo, hi = stats.wilson(k, n)
        return k, n, f"{k}/{n} = {100 * k / n if n else 0:.0f}% ({lo:.0f}–{hi:.0f})"

    done = lambda r: r["done"]
    clean = lambda r: r["grade"] == "worked"
    clean_p = lambda r: r["grade"] in ("worked", "worked_polling")
    first = lambda r: r["first_try"]
    for label, f in (("done", done), ("clean (worked)", clean), ("worked or worked by polling", clean_p),
                     ("first try", first)):
        md.append(f"| {label} | " + " | ".join(rate(t, f)[2] for t in TOOLS) + " |")
    for key, label in (("seconds", "wall seconds"), ("tokens", "tokens"), ("calls", "multiplexer calls"),
                       ("nonzero_exits", "calls that exited non-zero (all)"), ("errors", "errors (failed calls, waits apart)"), ("wait_misses", "waits ended without their condition"), ("help_calls", "help and doc reads"),
                       ("sleeps", "sleeps"), ("restarts", "restarts")):
        cells = []
        for t in TOOLS:
            v = [r[key] for r in run[t] if r[key] not in ("", None)]
            if not v:
                cells.append("-")
            elif key == "tokens":
                cells.append(f"{statistics.median(v) / 1e6:.2f}M / {statistics.mean(v) / 1e6:.2f}M")
            else:
                cells.append(f"{statistics.median(v):g} / {statistics.mean(v):.1f}")
        md.append(f"| {label}, median / mean | " + " | ".join(cells) + " |")
    md.append("")

    # All four at once.
    def majority(t, task, f):
        v = [f(r) for r in run[t] if r["task"] == task]
        return int(sum(v) * 2 > len(v)) if v else 0
    md.append("All four tools at once:")
    for label, f in (("done", done), ("clean", clean)):
        q, p = stats.cochran_q([[majority(t, task, f) for t in TOOLS] for task in tasks])
        md.append(f"- Cochran's Q, per-task majority {label}: Q = {q:.2f}, p = {fmt_p(p)}")
    for key in ("calls", "seconds", "tokens", "errors", "wait_misses"):
        cols = [per_task(rows, t, key, tasks) for t in TOOLS]
        s, p = stats.friedman([list(x) for x in zip(*cols)])
        md.append(f"- Friedman, per-task mean {key}: χ² = {s:.2f}, p = {fmt_p(p)}")
    md.append("")

    # fux against each other tool, Holm across the three.
    md += ["fux against each other tool (Holm-adjusted p across the three comparisons):", "",
           "| Measure | Test | " + " | ".join(f"vs {t}" for t in OTHERS) + " |", "|---|---|" + "---|" * 3]
    for label, f in (("done", done), ("clean", clean), ("worked or worked by polling", clean_p)):
        raw, fav = [], []
        for t in OTHERS:
            a, n1 = sum(f(r) for r in run["fux"]), len(run["fux"])
            b, n2 = sum(f(r) for r in run[t]), len(run[t])
            raw.append(stats.fisher(a, n1 - a, b, n2 - b))
            fav.append((a / n1 if n1 else 0) >= (b / n2 if n2 else 0))
        adj = stats.holm(raw)
        md.append(f"| {label}, runs | Fisher | " + " | ".join(
            f"p = {fmt_p(p)}: {verdict(p, fv)}" for p, fv in zip(adj, fav)) + " |")
        raw, fav = [], []
        for t in OTHERS:
            only_fux = sum(1 for task in tasks if majority("fux", task, f) and not majority(t, task, f))
            only_t = sum(1 for task in tasks if majority(t, task, f) and not majority("fux", task, f))
            raw.append(stats.mcnemar(only_fux, only_t))
            fav.append(only_fux >= only_t)
            fav[-1] = (fav[-1], only_fux, only_t)
        adj = stats.holm(raw)
        md.append(f"| {label}, tasks by majority | McNemar | " + " | ".join(
            f"{fv[1]} only fux, {fv[2]} only {t}; p = {fmt_p(p)}: {verdict(p, fv[0])}"
            for p, fv, t in zip(adj, fav, OTHERS)) + " |")
    for key in ("calls", "seconds", "tokens", "errors", "wait_misses"):
        fx = per_task(rows, "fux", key, tasks)
        raw, info = [], []
        for t in OTHERS:
            p, med, n = stats.wilcoxon(fx, per_task(rows, t, key, tasks))
            raw.append(p)
            info.append(med)
        adj = stats.holm(raw)
        md.append(f"| {key} per task | Wilcoxon | " + " | ".join(
            f"median {t} − fux {(m / 1e6 if key == 'tokens' else m):+.{2 if key == 'tokens' else 1}f}{'M' if key == 'tokens' else ''}; p = {fmt_p(p)}: {verdict(p, m > 0)}"
            for p, m, t in zip(adj, info, OTHERS)) + " |")
    md.append("")
    return md


def main():
    base, out = sys.argv[1], sys.argv[2]
    os.makedirs(out, exist_ok=True)
    rows, specs = table(base)
    rows = [r for r in rows if r["task"] in specs]
    invalid = sorted({r["task"] for r in rows if r["grade"] == "invalid"})
    with open(os.path.join(out, "runs.csv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0]) if rows else ["task"])
        w.writeheader()
        w.writerows(rows)
    # A sensitivity check: DROP=TW-010,TW-021 leaves those tasks out for
    # every tool (for example, tasks a second judge called invalid).
    dropped = [t for t in os.environ.get("DROP", "").split(",") if t]
    kept = [r for r in rows if r["task"] not in invalid and r["task"] not in dropped]
    tasks = sorted({r["task"] for r in kept})
    def doable(name):
        path = os.path.join(base, name)
        caps = json.load(open(path)) if os.path.exists(path) else {}
        return [t for t in tasks if all(caps.get(tool, {}).get(n, "yes") != "no"
                                        for tool in TOOLS for n in specs[t].get("needs", []))]
    md = ["# bench-4way analysis", "",
          f"Runs: {len(rows)}; invalid tasks dropped for every tool: {', '.join(invalid) or 'none'}"
          + (f"; also left out (sensitivity check): {', '.join(dropped)}" if dropped else "") + ".", ""]
    md += analyse(kept, tasks, "All tasks")
    md += analyse(kept, doable("caps.json"), "Tasks every tool can do (capability table as corrected)")
    if os.path.exists(os.path.join(base, "caps-frozen.json")):
        md += analyse(kept, doable("caps-frozen.json"), "Tasks every tool can do (capability table as frozen)")
    for kind in KINDS:
        ks = [t for t in tasks if kind in specs[t].get("kinds", [])]
        md += analyse(kept, ks, f"Kind: {kind}")
    for writer in sorted({writer_of(t) for t in tasks}):
        md += analyse(kept, [t for t in tasks if writer_of(t) == writer], f"Writer: {writer}")
    # Per task: medians per tool, done out of 3.
    md += ["## Per task", "", "Per tool: done/runs · median seconds · median tokens (k) · median calls · median errors", "",
           "| Task | " + " | ".join(TOOLS) + " |", "|---|" + "---|" * len(TOOLS)]
    for t in tasks:
        cells = []
        for tool in TOOLS:
            rs = [r for r in kept if r["task"] == t and r["tool"] == tool]
            if not rs:
                cells.append("-")
                continue
            med = lambda k: statistics.median([r[k] for r in rs])
            cells.append(f"{sum(r['done'] for r in rs)}/{len(rs)} · {med('seconds'):.0f}s · "
                         f"{med('tokens') / 1000:.0f}k · {med('calls'):g} · {med('errors'):g}")
        md.append(f"| {t} | " + " | ".join(cells) + " |")
    md.append("")
    # Grades and deciding layers.
    md += ["## Grades and deciding layers", ""]
    for tool in TOOLS:
        rs = [r for r in kept if r["tool"] == tool]
        g = {}
        for r in rs:
            g[r["grade"]] = g.get(r["grade"], 0) + 1
        layers = {}
        for r in rs:
            if r["grade"] != "worked" and r["layer"]:
                layers[r["layer"]] = layers.get(r["layer"], 0) + 1
        md.append(f"- **{tool}**: grades {dict(sorted(g.items()))}; deciding layers {dict(sorted(layers.items()))}")
    md.append("")
    open(os.path.join(out, "analysis.md"), "w").write("\n".join(md) + "\n")
    print("\n".join(md[:60]))


if __name__ == "__main__":
    main()
