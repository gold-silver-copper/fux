#!/usr/bin/env python3
"""The referee: the same bytes to this emulator and to others, and where their screens differ.

usage: tools/referee/referee.py [--fux PATH] [--old PATH] [--witness NAME,…] [-v] [CASE…]

Each case (tools/referee/corpus/**/NAME.case) is played to fux (the built program, in a pane of
a session of the referee's own) and to each witness that is installed:

  old    the emulator of the fux this one replaces (tools/referee/old, built apart)
  tmux   on a socket of its own
  xterm  under Xvfb, never on a display of yours

What is compared is the screen's text, row by row with blanks at the end cut, and the cursor's
row and column. A difference that is a row of the README's table of departures is reported as
known; any other makes the run fail. A case's lines:

  size COLSxROWS      first
  feed BYTES          as a Python bytes literal's inside: \\x1b[2J, \\r\\n
  text STRING         as a Python string literal's inside, sent as UTF-8: \\u200d
  file NAME [COUNT]   the bytes of a file beside the case, or its first COUNT
  resize COLSxROWS    the terminal is resized here
  # anything          a note
"""
import ast, os, pathlib, re, shutil, signal, subprocess, sys, tempfile, time

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent
PLAYER = HERE / "player.py"


def parse(path):
    """A case file as (cols, rows, steps); a step is ("feed", bytes) or ("resize", cols, rows)."""
    size, steps = None, []
    for line in path.read_text(encoding="utf-8").split("\n"):
        kind, _, rest = line.partition(" ")
        if kind == "size":
            size = tuple(map(int, rest.split("x")))
        elif kind == "feed":
            steps.append(("feed", ast.literal_eval('b"' + rest.replace('"', '\\"') + '"')))
        elif kind == "text":
            steps.append(("feed", ast.literal_eval('"' + rest.replace('"', '\\"') + '"').encode()))
        elif kind == "file":
            name, _, upto = rest.partition(" ")
            steps.append(("feed", (path.parent / name).read_bytes()[: int(upto) if upto else None]))
        elif kind == "resize":
            steps.append(("resize",) + tuple(map(int, rest.split("x"))))
        elif line and not line.startswith("#"):
            raise SystemExit(f"{path}: not a line of a case: {line!r}")
    if size is None:
        raise SystemExit(f"{path}: no size line")
    return size[0], size[1], steps


def written(steps):
    lines = [f"feed {s[1].hex()}" if s[0] == "feed" else f"resize {s[1]} {s[2]}" for s in steps]
    return "\n".join(lines)


def until(what, ready, seconds=10):
    deadline = time.monotonic() + seconds
    while not ready():
        if time.monotonic() > deadline:
            raise RuntimeError(f"timed out waiting for {what}")
        time.sleep(0.005)


def trimmed(lines, rows):
    lines = [line.rstrip(" ") for line in lines][:rows]
    return lines + [""] * (rows - len(lines))


class Played:
    """A terminal with the player in it: the directory they share and the steps to resize at."""

    def __init__(self, steps, extra=()):
        self.dir = pathlib.Path(tempfile.mkdtemp(prefix="referee-"))
        self.steps = steps
        (self.dir / "steps").write_text(written(steps))
        self.command = [sys.executable, str(PLAYER), str(self.dir / "steps"), str(self.dir), *extra]

    def run(self, resize, failed=lambda: None):
        """Waits for the player to finish, resizing the terminal with `resize` when it asks."""
        asked = set()

        def done():
            why = failed()
            if why:
                raise RuntimeError(why)
            for number, step in enumerate(self.steps):
                if step[0] == "resize" and number not in asked and (self.dir / f"ready-{number}").exists():
                    asked.add(number)
                    resize(step[1], step[2])
            return (self.dir / "done").exists()

        until("the player", done, 30)
        row, col = map(int, (self.dir / "cursor").read_text().split())
        return row, col

    def leave(self):
        (self.dir / "exit").touch()

    def remove(self):
        shutil.rmtree(self.dir, ignore_errors=True)


def fux(case, binary):
    cols, rows, steps = case
    played = Played(steps)
    env = dict(os.environ, FUXN_DIR=str(played.dir / "session"), FUXN_KEYS=str(played.dir / "keys"), COLUMNS=str(cols), LINES=str(rows))
    for name in ("FUXN_PANE", "FUXN_WS", "FUXN_VIEW"):
        env.pop(name, None)
    try:
        pane = pathlib.Path(subprocess.run([binary, "start", "r/p", "--", *played.command], env=env, check=True, capture_output=True, text=True).stdout.strip())
        cursor = played.run(lambda c, r: (pane / "ctl").write_text(f"resize {c} {r}\n"))
        # The screen file follows the emulator by a moment: read it when it has stopped changing.
        seen = [None, time.monotonic()]

        def settled():
            text = (pane / "screen").read_text(encoding="utf-8", errors="replace")
            if text != seen[0]:
                seen[:] = [text, time.monotonic()]
            return time.monotonic() - seen[1] > 0.08 and f"cursor={cursor[0]},{cursor[1]} " in text.split("\n")[0]

        until("the screen file", settled)
        last = seen[0].split("\n")[0]
        now_rows = int(re.search(r"size=\d+x(\d+)", last).group(1))
        return trimmed(seen[0].split("\n")[1:], now_rows), cursor
    finally:
        played.leave()
        shutil.rmtree(played.dir / "session", ignore_errors=True)
        played.remove()


def old(case, binary):
    cols, rows, steps = case
    out = subprocess.run([binary], input=f"size {cols} {rows}\n{written(steps)}\n", capture_output=True, text=True, check=True).stdout.split("\n")
    row, col = map(int, out[0].split()[1:])
    return [line[1:] for line in out[1:] if line.startswith("|")], (row, col)


def tmux(case, _):
    cols, rows, steps = case
    played = Played(steps)
    base = ["tmux", "-S", str(played.dir / "sock"), "-f", str(HERE / "tmux.conf")]
    try:
        subprocess.run([*base, "new-session", "-d", "-s", "r", "-x", str(cols), "-y", str(rows), " ".join(played.command)], check=True, capture_output=True)
        size = [rows]

        def resize(c, r):
            size[0] = r
            subprocess.run([*base, "resize-window", "-t", "r", "-x", str(c), "-y", str(r)], check=True, capture_output=True)

        cursor = played.run(resize)
        text = subprocess.run([*base, "capture-pane", "-p", "-t", "r"], check=True, capture_output=True).stdout.decode("utf-8", "replace")
        return trimmed(text.split("\n"), size[0]), cursor
    finally:
        played.leave()
        subprocess.run([*base, "kill-server"], capture_output=True)
        played.remove()


class Display:
    """An X server that draws nowhere, started once for the xterm witness."""

    number = None
    process = None

    @classmethod
    def get(cls):
        if cls.number is None:
            read, write = os.pipe()
            cls.process = subprocess.Popen(["Xvfb", "-displayfd", str(write), "-screen", "0", "4096x4096x24", "-nolisten", "tcp"], pass_fds=[write], stderr=subprocess.DEVNULL)
            os.close(write)
            cls.number = os.read(read, 16).decode().strip()
            os.close(read)
        return ":" + cls.number

    @classmethod
    def stop(cls):
        if cls.process:
            cls.process.terminate()
            cls.process.wait()


def xterm(case, _):
    cols, rows, steps = case
    played = Played(steps, extra=["xterm"])
    # A display of the referee's own and no other: xterm must never reach the user's.
    env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "XAUTHORITY")}
    env.update(DISPLAY=Display.get(), LC_ALL="C.UTF-8")
    printed = played.dir / "printed"
    options = ["allowWindowOps: true", f"printerCommand: cat > {printed}.new && mv {printed}.new {printed}", "printAttributes: 0", "printerFormFeed: false", "printerNewLine: true", "utf8: 2"]
    command = ["xterm", "-u8", "+sb", "-geometry", f"{cols}x{rows}"]
    for option in options:
        command += ["-xrm", "XTerm*" + option]
    process = subprocess.Popen([*command, "-e", *played.command], env=env, stderr=subprocess.DEVNULL)
    try:
        cursor = played.run(lambda c, r: None, lambda: "xterm ended early" if process.poll() is not None else None)
        until("xterm's print of its screen", printed.exists)
        now_rows = [s[2] for s in steps if s[0] == "resize"][-1:] or [rows]
        return trimmed(printed.read_text(encoding="utf-8", errors="replace").split("\n"), now_rows[0]), cursor
    finally:
        played.leave()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.send_signal(signal.SIGTERM)
        played.remove()


def departures():
    """The rows of the README's table between its two marks: {(case, witness): why}."""
    known, inside = {}, False
    for line in (REPO / "README.md").read_text(encoding="utf-8").split("\n"):
        if "referee:departures" in line:
            inside = True
        elif "referee:end" in line:
            inside = False
        elif inside and line.startswith("| `"):
            cells = [cell.strip() for cell in line.strip("|").split("|")]
            for case in re.findall("`([^`]+)`", cells[0]):
                for witness in cells[1].split(","):
                    known[(case, witness.strip())] = cells[2]
    return known


def main():
    args, names, options = sys.argv[1:], [], {"--fux": str(REPO / "target/release/fux"), "--old": str(HERE / "old/target/release/referee-old"), "--witness": "old,tmux,xterm"}
    verbose = "-v" in args
    args = [a for a in args if a != "-v"]
    while args:
        arg = args.pop(0)
        if arg in options:
            options[arg] = args.pop(0)
        elif arg in ("-h", "--help"):
            raise SystemExit(__doc__)
        else:
            names.append(arg)
    witnesses = {"old": (old, options["--old"]), "tmux": (tmux, shutil.which("tmux")), "xterm": (xterm, shutil.which("xterm") and shutil.which("Xvfb"))}
    witnesses = {name: witnesses[name] for name in options["--witness"].split(",") if name}
    absent = [name for name, (_, binary) in witnesses.items() if not binary or not os.path.exists(binary)]
    for name in absent:
        print(f"witness {name}: not installed (or not built), left out")
        del witnesses[name]
    cases = sorted(HERE.glob("corpus/**/*.case"))
    cases = [c for c in cases if not names or c.stem in names or str(c.relative_to(HERE / "corpus").with_suffix("")) in names]
    known, new, stale, agree = departures(), [], [], {name: 0 for name in witnesses}
    try:
        for path in cases:
            name = str(path.relative_to(HERE / "corpus").with_suffix(""))
            case = parse(path)
            ours = fux(case, options["--fux"])
            differing = []
            for witness, (play, binary) in witnesses.items():
                try:
                    theirs = play(case, binary)
                except (RuntimeError, subprocess.CalledProcessError, ValueError) as error:
                    theirs = ([f"(could not be played: {error})"], None)
                if theirs == ours:
                    agree[witness] += 1
                    if (name, witness) in known:
                        stale.append((name, witness))
                    continue
                expected = (name, witness) in known
                differing.append(witness + ("" if expected else " (NEW)"))
                if not expected:
                    new.append((name, witness))
                if verbose or not expected:
                    print(f"--- {name}: fux, cursor {ours[1]}")
                    print("\n".join("  |" + row for row in ours[0]))
                    print(f"--- {name}: {witness}, cursor {theirs[1]}")
                    print("\n".join("  |" + row for row in theirs[0]))
            print(f"{name}: " + ("same" if not differing else "differs from " + ", ".join(differing)))
    finally:
        Display.stop()
    print()
    for witness in witnesses:
        listed = sum(1 for (case, w) in known if w == witness and any(str(p.relative_to(HERE / "corpus").with_suffix("")) == case for p in cases))
        print(f"{witness}: same on {agree[witness]} of {len(cases)} cases; {listed} departures in the README's table")
    for case, witness in stale:
        print(f"note: the README lists {case} as differing from {witness}, and it no longer does")
    for case, witness in new:
        print(f"NEW: {case} differs from {witness} and the README's table has no row for it")
    sys.exit(1 if new else 0)


if __name__ == "__main__":
    main()
