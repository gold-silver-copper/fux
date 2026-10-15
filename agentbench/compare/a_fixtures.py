#!/usr/bin/env python3
"""Part A: fixture sets through both builds. A check is keyed (fixture,
scenario, step, word, want); paired across builds where both asked it.
Per class (H1 echo-taken-as-output: a word that should not be met was met;
H2 missed-output: a word that should be met was not)."""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stats import wilson, mcnemar
A = os.path.join(os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench")), "A")


def checks(path):
    out = {}
    for sc in json.load(open(path)):
        for st in sc["steps"]:
            for c in st.get("checks", []):
                out[(sc["fixture"], sc["scenario"], st["step"], c["word"], c["want"])] = c["met"] == c["want"]
    return out


for set_name in sys.argv[1:] or ["F", "G"]:
    b, c = checks(f"{A}/fix-{set_name}-base.json"), checks(f"{A}/fix-{set_name}-cand.json")
    keys = sorted(set(b) & set(c))
    print(f"## set {set_name}: {len(b)} checks on master, {len(c)} on #161, {len(keys)} paired")
    for label, sel in (("H1 echo met (want not met)", lambda k: not k[4]),
                       ("H2 output missed (want met)", lambda k: k[4]), ("all", lambda k: True)):
        ks = [k for k in keys if sel(k)]
        wb = sum(not b[k] for k in ks); wc = sum(not c[k] for k in ks)
        only_b = sum((not b[k]) and c[k] for k in ks); only_c = sum(b[k] and (not c[k]) for k in ks)
        lo, hi = wilson(wb, len(ks)); lo2, hi2 = wilson(wc, len(ks))
        print(f"  {label:30} n={len(ks):4}  master wrong {wb:3} ({lo:4.1f}-{hi:4.1f}%)  #161 wrong {wc:3} ({lo2:4.1f}-{hi2:4.1f}%)  only master {only_b}, only #161 {only_c}, McNemar p={mcnemar(only_b, only_c):.2g}")
