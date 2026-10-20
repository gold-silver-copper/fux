"""alacritty's ref tests and JediTerm's test data, for release/refs.sh.

    refs.py cases ALACRITTY_REF JEDITERM_DATA DIR   DIR/cases.json and each case's bytes
    refs.py list DIR                                each case: NAME ROWS COLS BYTES
    refs.py score DIR EXPECTED                      each engine's screens beside the suite's
                                                    own and real xterm's; exit 1 if fux-vt
                                                    differs from xterm otherwise than
                                                    EXPECTED lists, if fux-vt's or xterm's
                                                    screen of a case is missing, or if the
                                                    suites are not their 45 and 24 cases

A case is a screen size, the bytes a terminal of that size reads, and the
suite's own expected screen:
- alacritty (alacritty_terminal/tests/ref, Apache-2.0 or MIT): each
  directory's alacritty.recording at size.json's size; grid.json is
  alacritty's own grid after it, its visible rows read as alacritty's
  ref test reads them (a wide character's spacer dropped, its zero-width
  characters kept).
- JediTerm (core/tests/resources/testData, Apache-2.0 or LGPL-3.0): each
  NAME.txt read as UTF-8 with LF made CR LF, as its EmulatorTestAbstract
  does, at the size its test gives (80x24 unless EmulatorTest or
  VtEmulatorTest names another); NAME.after.txt is the screen it expects
  (testSetCursorPosition's is written in its test).

Screens are compared as text: each row with its trailing blanks dropped.
An engine's screens are DIR/raw/ENGINE/NAME: fux's `wrapped capture json`,
or xterm's printout of its screen (MC, `CSI i`).
"""
import json
import os
import sys

# The cases each suite has at the commits refs.sh fetches: fewer means a
# case was lost on the way, and is no pass.
CASES = {"alacritty": 45, "jediterm": 24}

# Sizes JediTerm's tests give (columns, rows), and its one inline screen.
JEDITERM_SIZES = {
    "testSystemCommands": (30, 3),
    "testSetCursorPosition": (3, 4),
    "Test2_Screen_3": (132, 24),
    "Test2_Screen_5": (132, 24),
}
JEDITERM_INLINE = {"testSetCursorPosition": ["X00", "0X", "X X", ""]}


def alacritty_rows(grid):
    raw, lines = grid["raw"], grid["lines"]
    inner, zero, n, vis = raw["inner"], raw["zero"], len(raw["inner"]), raw["visible_lines"]
    rows = []
    for line in range(lines):
        if vis == lines - 1:
            row = inner[(zero + (vis - line)) % n]
        else:
            row = inner[(zero + (vis - 1 - line)) % n]
        text = ""
        for cell in row["inner"]:
            flags = cell.get("flags", "")
            if "LEADING_WIDE_CHAR_SPACER" in flags:
                text += " "
                continue
            if "WIDE_CHAR_SPACER" in flags:
                continue
            text += cell["c"] + "".join((cell.get("extra") or {}).get("zerowidth") or [])
        rows.append(text)
    return rows


def norm(rows, height):
    rows = [r.replace("\t", " ").replace("￿", "").rstrip() for r in rows][:height]
    return rows + [""] * (height - len(rows))


def cases(ref, data, out):
    os.makedirs(os.path.join(out, "bytes"), exist_ok=True)
    found = []
    for name in sorted(os.listdir(ref)):
        d = os.path.join(ref, name)
        size = json.load(open(os.path.join(d, "size.json"), encoding="utf-8"))
        with open(os.path.join(d, "alacritty.recording"), "rb") as f:
            body = f.read()
        grid = json.load(open(os.path.join(d, "grid.json"), encoding="utf-8"))
        found.append(("alacritty/" + name, size["screen_lines"], size["columns"], body,
                      alacritty_rows(grid)))
    for top, _, files in sorted(os.walk(data)):
        for file in sorted(files):
            if not file.endswith(".txt") or file.endswith(".after.txt"):
                continue
            stem = file[: -len(".txt")]
            rel = os.path.relpath(os.path.join(top, stem), data)
            # VtEmulatorTest's names: vttest/Test2_Screen/3 is Test2_Screen_3.
            key = rel.replace("vttest/", "").replace("/", "_")
            after = os.path.join(top, stem + ".after.txt")
            if key in JEDITERM_INLINE:
                want = JEDITERM_INLINE[key]
            elif os.path.exists(after):
                text = open(after, encoding="utf-8").read()
                want = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
            else:
                continue
            cols, rows = JEDITERM_SIZES.get(key, (80, 24))
            text = open(os.path.join(top, file), encoding="utf-8").read()
            body = text.replace("\r\n", "\n").replace("\n", "\r\n").encode("utf-8")
            found.append(("jediterm/" + key, rows, cols, body, want))
    listed = []
    for name, rows, cols, body, want in found:
        path = os.path.join(out, "bytes", name.replace("/", "--"))
        with open(path, "wb") as f:
            f.write(body)
        listed.append({"name": name, "rows": rows, "cols": cols, "bytes": path,
                       "expected": norm(want, rows)})
    json.dump(listed, open(os.path.join(out, "cases.json"), "w", encoding="utf-8"), indent=1)


def screen(path, height):
    """An engine's screen from its raw file, or None if it has none."""
    if not os.path.exists(path):
        return None
    text = open(path, encoding="utf-8", errors="replace").read()
    if text.startswith("{"):
        return norm(json.loads(text)["lines"], height)
    return norm(text.split("\n"), height)


def score(out, expected):
    listed = json.load(open(os.path.join(out, "cases.json"), encoding="utf-8"))
    raw = os.path.join(out, "raw")
    engines = sorted(os.listdir(raw))
    known = {}
    for line in open(expected, encoding="utf-8"):
        line = line.strip()
        if line and not line.startswith("#"):
            name, _, why = line.partition(" ")
            known[name] = why.strip()
    table, problems, result = {}, [], {}
    for case in listed:
        name, rows = case["name"], case["rows"]
        suite = name.split("/", 1)[0]
        got = {e: screen(os.path.join(raw, e, name.replace("/", "--")), rows) for e in engines}
        xterm = got.get("xterm")
        result[name] = {e: {"suite": s == case["expected"], "xterm": xterm is not None and s == xterm}
                        for e, s in got.items() if s is not None}
        for e, s in got.items():
            t = table.setdefault((suite, e), [0, 0, 0, 0])
            if s is None:
                continue
            t[0] += s == case["expected"]
            t[1] += 1
            if xterm is not None:
                t[2] += s == xterm
                t[3] += 1
        fux = got.get("fux")
        for engine, s in (("fux", fux), ("xterm", xterm)):
            if s is None:
                problems.append(f"{name}: no screen from {engine}")
        if fux is not None and xterm is not None:
            if fux != xterm and name not in known:
                problems.append(f"{name}: fux-vt's screen is not xterm's, and not listed")
            if fux == xterm and name in known:
                problems.append(f"{name}: listed, but fux-vt's screen is xterm's")
    for suite, want in CASES.items():
        n = sum(c["name"].startswith(suite + "/") for c in listed)
        if n != want:
            problems.append(f"{suite}: {n} cases, not {want}")
    print(f"{'suite':10} {'engine':8} {'= its own':>10} {'= xterm':>9}")
    for (suite, e), (a, n, x, m) in sorted(table.items()):
        print(f"{suite:10} {e:8} {a:>5}/{n:<4} {x:>5}/{m}")
    for p in problems:
        print(f"refs: {p}")
    json.dump(result, open(os.path.join(out, "scores.json"), "w", encoding="utf-8"), indent=1)
    return 1 if problems else 0


def main():
    a = sys.argv[1:]
    if a[:1] == ["cases"] and len(a) == 4:
        cases(a[1], a[2], a[3])
        return 0
    if a[:1] == ["list"] and len(a) == 2:
        for c in json.load(open(os.path.join(a[1], "cases.json"), encoding="utf-8")):
            print(c["name"].replace("/", "--"), c["rows"], c["cols"], c["bytes"])
        return 0
    if a[:1] == ["score"] and len(a) == 3:
        return score(a[1], a[2])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
