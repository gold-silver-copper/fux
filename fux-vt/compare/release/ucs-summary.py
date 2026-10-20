"""ucs-summary.py JSON [MIN]: ucs-detect's results (--save-json), each
category scored beside the authority that decides it, then the languages:
how many pass (no grapheme measured otherwise than wcwidth measures it),
and for each that fails, its count and the kinds of grapheme it fails on,
with their general categories, so the width rule behind each shows. Exit 1
if fewer than MIN languages (default 81) pass, or if ucs-detect measured
other than its LANGUAGES."""
import json
import sys
import unicodedata

# The languages ucs-detect 2.3.8 measures: fewer means some went unmeasured.
LANGUAGES = 85

# What decides each category, for fux-vt: ucs-detect's expectation is
# wcwidth's, which is not itself an authority (README, "Widths").
AUTHORITY = {
    "unicode_wide": "UAX #11 East_Asian_Width W and F, by fux-vt's width rule",
    "narrow": "UAX #11 and fux-vt's width rule (Cf, Mn and Me take no cell)",
    "emoji_zwj": "UTS #51 ZWJ sequences, one cluster by UAX #29 GB11: fux-vt's policy, 2 cells",
    "emoji_vs16": "UTS #51 emoji presentation (VS16): fux-vt's policy, 2 cells",
    "emoji_vs15": "UTS #51 text presentation (VS15): policy, decided against narrowing (README)",
    "ri": "UAX #29 GB12 and GB13, a flag of two regional indicators: 2 cells",
    "sri": "a lone regional indicator: policy, 1 cell as xterm (README)",
    "sfz": "a lone skin-tone modifier: UAX #29 GB9 joins it to the cell before (README)",
    "tofu": "code points with no glyph: fux-vt's width rule",
}


def cps(text):
    return [ord(c) for c in text.encode("utf-8").decode("unicode_escape")] if "\\" in text else [ord(c) for c in text]


def kind(text):
    return " ".join(f"U+{c:04X} {unicodedata.category(chr(c))}" for c in cps(text))


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    d = json.load(open(sys.argv[1], encoding="utf-8"))
    least = int(sys.argv[2]) if len(sys.argv) == 3 else 81
    t = d["test_results"]
    print(f"ucs-detect {d['ucs_detect_version']}, wcwidth {d['wcwidth_version']} "
          f"(Unicode {d['unicode_version']}), {d['software_name']} at "
          f"{d['width']}x{d['height']}")
    for key, results in sorted(t.items()):
        if key == "language_results" or not key.endswith("_results") or not isinstance(results, dict):
            continue
        name = key[: -len("_results")]
        total = sum(v.get("n_total", 0) for v in results.values() if isinstance(v, dict))
        errors = sum(v.get("n_errors", 0) for v in results.values() if isinstance(v, dict))
        if total:
            print(f"  {name:13} {100 * (total - errors) / total:5.1f}% of {total:5}  "
                  f"{AUTHORITY.get(name, '?')}")
    langs = t["language_results"]
    passed = sorted(k for k, v in langs.items() if v.get("n_errors", 1) == 0)
    print(f"  languages     {len(passed)}/{len(langs)} pass  "
          "UAX #29 clusters, each as wide as fux-vt's width rule makes it")
    for lang, v in sorted(langs.items()):
        if v.get("n_errors", 1) == 0:
            continue
        shapes = {}
        for f in v.get("failed", []):
            s = (f["measured_by_terminal"], f["measured_by_wcwidth"])
            shapes.setdefault(s, f["wchars"])
        how = "; ".join(f"{a} for {b}, as {kind(w)}" for (a, b), w in sorted(shapes.items()))
        print(f"    {lang}: {v['n_errors']} of {v['n_total']} (measured for wcwidth's: {how})")
    if len(langs) != LANGUAGES:
        print(f"  ucs-summary: {len(langs)} languages measured, not {LANGUAGES}")
        return 1
    return 1 if len(passed) < least else 0


if __name__ == "__main__":
    sys.exit(main())
