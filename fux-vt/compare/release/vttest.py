"""vttest's self-checks, from its own log (vttest -l), for release/vttest.sh.

    vttest.py cmds KEYS LEVEL OUT       KEYS (vttest.keys) as vttest's command file (-c)
    vttest.py results LOG               the self-checks LOG holds, one a line
    vttest.py check LOG EXPECTED NAME   LOG against EXPECTED's lines for NAME: exit 1 on a
                                        failure not listed, a listed one that passes, or a
                                        count other than the listed one

A self-check is a result vttest decides itself, a `Note: result` line:
`failed` (SHOW_FAILURE) when the answer is missing or malformed; DA3's `not
supported` (n/a: vttest asks for it from level 4 only); any other text is
`ok`, an answer vttest could read (SHOW_SUCCESS, or what it read: "Terminal
is in good operating condition"). Under a DECRPM choice the results name
each mode's state, which vttest does not judge: only a `failed` counts
there. Under 11.8, where xterm.c prints its verdicts without show_result, a
`Text:` line `ok`, `failed`, `cursor save/restore ok|failed` or `[N:M] ok`
counts too. Each is named by the menu choice it came under and its place
there: `11.2.5.3.6.3#1`.
"""
import re
import sys

# The command file vttest replays: each input a `Read:` line. vttest skips
# to the next `Wait:` line each time it stops to read the terminal, and to
# its `Done:` line after, so each input goes after enough pairs for every
# read of the terminal between two inputs (DECRQCRA's test asks once for
# each of 95 characters); a Read skips the pairs left over.
PAIRS = 512


def cmds(keys, level, out):
    lines = []
    for raw in open(keys, encoding="utf-8"):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("@<3 "):
            if level >= 3:
                continue
            line = line[4:].strip()
        elif line.startswith("@3 "):
            if level < 3:
                continue
            line = line[3:].strip()
        key = "" if line == "-" else line
        lines += [f"Wait: {n}\nDone: {n}\n" for n in range(PAIRS)]
        lines.append(f"Read: {key}\n")
    with open(out, "w", encoding="utf-8") as f:
        f.writelines(lines)


XTERM = re.compile(r"^(?:cursor save/restore |\[\d+:\d+\] )?(ok|failed)\b")


def parse(log):
    """The self-checks, as (name, outcome, text), the operating level, and
    whether the run ended as the walk does."""
    checks, choice, modes, seen = [], "start", False, {}
    level, ended, bad = None, False, False
    for raw in open(log, encoding="utf-8", errors="replace"):
        line = raw.rstrip("\n")
        if line.startswith("Note: choice "):
            choice, _, name = line[len("Note: choice "):].partition(":")
            modes = "DECRPM" in name
            continue
        if line.startswith("Note: Cur Operating Level: "):
            level = int(line.rsplit(" ", 1)[1])
            continue
        if line.startswith("Note: Cleanup & exit"):
            ended = True
            continue
        if "Bad choice" in line:
            bad = True
            continue
        outcome = None
        if line.startswith("Note: result "):
            text = line[len("Note: result "):].strip()
            if text.startswith("failed"):
                outcome = "failed"
            elif text == "not supported":
                outcome = "n/a"
            elif not modes:
                outcome = "ok"
        elif line.startswith("Text: ") and choice.startswith("11.8."):
            text = line[len("Text: "):].strip()
            m = XTERM.match(text)
            if m:
                outcome = m.group(1)
        if outcome:
            seen[choice] = seen.get(choice, 0) + 1
            checks.append((f"{choice}#{seen[choice]}", outcome, text))
    return checks, level, ended and not bad


def expected(path, name):
    """NAME's lines in EXPECTED: {check: outcome}, and its count line."""
    want, count = {}, None
    for raw in open(path, encoding="utf-8"):
        line = raw.split(" #", 1)[0].strip()
        if not line or line.startswith("#"):
            continue
        f = line.split()
        if f[0] != name:
            continue
        if f[1] == "count":
            count = int(f[2])
        else:
            want[f[1]] = f[2]
    return want, count


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "cmds" and len(sys.argv) == 5:
        cmds(sys.argv[2], int(sys.argv[3]), sys.argv[4])
        return 0
    if cmd == "results" and len(sys.argv) == 3:
        checks, level, whole = parse(sys.argv[2])
        for name, outcome, text in checks:
            print(f"{name}\t{outcome}\t{text}")
        tally = {o: sum(1 for c in checks if c[1] == o) for o in ("ok", "failed", "n/a")}
        print(f"# level {level}, {len(checks)} self-checks: {tally['ok']} ok, "
              f"{tally['failed']} failed, {tally['n/a']} n/a; "
              f"{'the walk ran whole' if whole else 'THE WALK DID NOT RUN WHOLE'}")
        return 0 if whole else 1
    if cmd == "check" and len(sys.argv) == 5:
        checks, level, whole = parse(sys.argv[2])
        want, count = expected(sys.argv[3], sys.argv[4])
        problems = [] if whole else ["the walk did not run whole (a bad choice, or no exit)"]
        got = {name: outcome for name, outcome, _ in checks}
        for name, outcome in got.items():
            if outcome != "ok" and want.get(name) != outcome:
                problems.append(f"{name} {outcome}, not listed so")
        for name, outcome in want.items():
            if got.get(name) != outcome:
                problems.append(f"{name} listed {outcome}, is {got.get(name, 'not run')}")
        if count is not None and count != len(checks):
            problems.append(f"{len(checks)} self-checks, the list says {count}")
        for p in problems:
            print(f"vttest: {p}")
        ok = sum(1 for o in got.values() if o == "ok")
        print(f"vttest {sys.argv[4]}: level {level}, {ok}/{len(checks)} self-checks ok, "
              f"{len(problems)} problems")
        return 1 if problems else 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
