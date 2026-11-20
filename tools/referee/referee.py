#!/usr/bin/env python3
"""The referee: the same bytes to this emulator and to others, and where their screens differ.

usage: tools/referee/referee.py [--fux PATH] [--old PATH] [--witness NAME,…] [-v] [CASE…]

Each case (tools/referee/corpus/**/NAME.case) is played to fux (the built program, in a pane of
a session of the referee's own) and to each witness:

  old    the emulator of the fux this one replaces (tools/referee/old, built apart)
  tmux   on a socket of its own
  xterm  under Xvfb, never on a display of yours

All three are needed unless --witness names fewer. What is compared is the screen's text, row
by row with blanks at the end cut, and the cursor's row and column; not colours, answers to
questions, modes, the bell or the clipboard. A difference is known if the README's table of
departures has a row for the case and the witness that says where it is (`text`, `cursor`, or
both); any other makes the run fail, and so does a case that a witness could not be played.
Witnesses change between versions, so the table has a row if any version the referee was run
with differs, and a row that does not differ here is noted. A case's lines:

  size COLSxROWS      first
  feed BYTES          as a Python bytes literal's inside: \\x1b[2J, \\r\\n
  text STRING         as a Python string literal's inside, sent as UTF-8: \\u200d
  file NAME [COUNT]   the bytes of a file beside the case, or its first COUNT
  resize COLSxROWS    the terminal is resized here
  # anything          a note

Feeds that follow each other reach the terminal as one stream.
"""
import ast, os, pathlib, re, select, shlex, shutil, signal, subprocess, sys, tempfile, time

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent
PLAYER = HERE / "player.py"
# Never /tmp unless the user's own TMPDIR says so.
ROOM = os.environ.get("TMPDIR") or os.environ.get("XDG_RUNTIME_DIR") or str(pathlib.Path.home() / ".cache")


class Unplayed(Exception):
    """A terminal could not be played a case: the harness's trouble, not a difference."""


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
            if "/" in name:
                raise SystemExit(f"{path}: {name} is not a file beside the case")
            steps.append(("feed", (path.parent / name).read_bytes()[: int(upto) if upto else None]))
        elif kind == "resize":
            steps.append(("resize",) + tuple(map(int, rest.split("x"))))
        elif line and not line.startswith("#"):
            raise SystemExit(f"{path}: not a line of a case: {line!r}")
    if size is None:
        raise SystemExit(f"{path}: no size line")
    return size[0], size[1], steps


def written(case):
    cols, rows, steps = case
    lines = [f"feed {s[1].hex()}" if s[0] == "feed" else f"resize {s[1]} {s[2]}" for s in steps]
    return "\n".join([f"size {cols} {rows}", *lines]) + "\n"


def until(what, ready, seconds=10):
    deadline = time.monotonic() + seconds
    while not ready():
        if time.monotonic() > deadline:
            raise Unplayed(f"timed out waiting for {what}")
        time.sleep(0.005)


def run(command, **more):
    try:
        return subprocess.run(command, check=True, capture_output=True, timeout=20, **more).stdout
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError) as error:
        raise Unplayed(f"{command[0]}: {error}")


def screen(text, rows):
    """A screen's text as its rows, each without the blanks at its end."""
    lines = text.split("\n")
    if len(lines) < rows or any(lines[rows:]):
        raise Unplayed(f"a screen of {rows} rows came as {len(lines)} lines")
    return [line.rstrip(" ") for line in lines[:rows]]


class Played:
    """A terminal with the player in it: the directory they share and the steps to resize at."""

    def __init__(self, case, extra=()):
        self.dir = pathlib.Path(tempfile.mkdtemp(prefix="referee-", dir=ROOM))
        self.steps = case[2]
        self.rows = ([step[2] for step in self.steps if step[0] == "resize"] or [case[1]])[-1]
        (self.dir / "steps").write_text(written(case))
        self.command = [sys.executable, str(PLAYER), str(self.dir / "steps"), str(self.dir), *extra]

    def run(self, resize, ended=lambda: False):
        """Waits for the player to finish, resizing the terminal with `resize` when it asks."""
        asked = set()

        def done():
            if (self.dir / "failed").exists():
                raise Unplayed((self.dir / "failed").read_text().strip())
            if ended():
                raise Unplayed("the terminal ended early")
            for number, step in enumerate(self.steps, 1):
                if step[0] == "resize" and number not in asked and (self.dir / f"ready-{number}").exists():
                    asked.add(number)
                    resize(step[1], step[2])
            return (self.dir / "done").exists()

        until("the player", done, 30)
        row, col = map(int, (self.dir / "cursor").read_text().split())
        return row, col

    def leave(self):
        (self.dir / "exit").touch()


def fux(case, binary):
    played = Played(case)
    env = dict(os.environ, FUXN_DIR=str(played.dir / "session"), FUXN_KEYS=str(played.dir / "keys"))
    env.update(COLUMNS=str(case[0]), LINES=str(case[1]))
    for name in ("FUXN_PANE", "FUXN_WS", "FUXN_VIEW"):
        env.pop(name, None)
    try:
        pane = pathlib.Path(run([binary, "start", "r/p", "--", *played.command], env=env, text=True).strip())
        cursor = played.run(lambda c, r: (pane / "ctl").write_text(f"resize {c} {r}\n"))
        # The screen file follows the emulator by a moment. Once the player has left, the pane's
        # keeper has taken in all it wrote and published the last screen: `wait` says when.
        played.leave()
        run([binary, "wait", str(pane)], env=env)
        return screen((pane / "screen").read_text(encoding="utf-8").split("\n", 1)[1], played.rows), cursor
    finally:
        played.leave()
        shutil.rmtree(played.dir, ignore_errors=True)


def old(case, binary):
    out = run([binary], input=written(case), text=True).split("\n")
    row, col = map(int, out[0].split()[1:])
    return [line[1:].rstrip(" ") for line in out[1:] if line.startswith("|")], (row, col)


def tmux(case, _):
    played = Played(case)
    base = ["tmux", "-S", str(played.dir / "sock"), "-f", str(HERE / "tmux.conf")]
    try:
        run([*base, "new-session", "-d", "-s", "r", "-x", str(case[0]), "-y", str(case[1]), shlex.join(played.command)])
        cursor = played.run(lambda c, r: run([*base, "resize-window", "-t", "r", "-x", str(c), "-y", str(r)]))
        text = run([*base, "capture-pane", "-p", "-t", "r"]).decode("utf-8", "replace")
        return screen(text, played.rows), cursor
    finally:
        played.leave()
        subprocess.run([*base, "kill-server"], capture_output=True)
        shutil.rmtree(played.dir, ignore_errors=True)


class Display:
    """An X server that draws nowhere, started once for the xterm witness. Its number is one the
    referee asked for, from 90 to 99 and with no socket or lock of that number there, and is used
    only while this very server lives. `-displayfd` only says when it listens."""

    number = None
    process = None

    @classmethod
    def get(cls):
        for number in range(90, 100) if cls.process is None else ():
            # Never a number an X server may be using or have used, never one it picks itself:
            # a server that takes a desktop's number removes the desktop's socket when it ends.
            if os.path.lexists(f"/tmp/.X11-unix/X{number}") or os.path.lexists(f"/tmp/.X{number}-lock"):
                continue
            read, write = os.pipe()
            command = ["Xvfb", f":{number}", "-displayfd", str(write), "-screen", "0", "4096x4096x24", "-nolisten", "tcp"]
            process = subprocess.Popen(command, pass_fds=[write], stderr=subprocess.DEVNULL)
            os.close(write)
            # It writes its number when it listens, or ends without a word if the number is taken.
            ready = select.select([read], [], [], 10)[0] and os.read(read, 16).decode().strip() == str(number)
            os.close(read)
            if ready and process.poll() is None:
                cls.process, cls.number = process, number
                break
            process.kill()
            process.wait()
        if cls.process is None or cls.process.poll() is not None:
            raise SystemExit("no Xvfb of the referee's own is running (numbers 90 to 99): xterm is started on no other display. --witness old,tmux runs without it")
        return f":{cls.number}"

    @classmethod
    def stop(cls):
        if cls.process:
            cls.process.terminate()
            cls.process.wait()


def xterm(case, _):
    played = Played(case, extra=["xterm"])
    # A display of the referee's own and no other, and none of the user's settings for xterm.
    env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "XAUTHORITY", "XAPPLRESDIR")}
    env.update(DISPLAY=Display.get(), LC_ALL="C.UTF-8", HOME=str(played.dir), XENVIRONMENT="/dev/null")
    printer = f"sh {shlex.quote(str(HERE / 'printer.sh'))} {shlex.quote(str(played.dir))}"
    options = ["allowWindowOps: true", "printerCommand: " + printer, "printerAutoClose: true", "printAttributes: 0"]
    options += ["printerFormFeed: false", "printerNewLine: true", "utf8: 2", "precompose: false"]
    command = ["xterm", "-u8", "+sb", "-ut", "-geometry", f"{case[0]}x{case[1]}"]
    for option in options:
        command += ["-xrm", "XTerm*" + option]
    process = None
    try:
        process = subprocess.Popen([*command, "-e", *played.command], env=env, cwd=played.dir, stderr=subprocess.DEVNULL)
        cursor = played.run(lambda c, r: None, lambda: process.poll() is not None or Display.process.poll() is not None)
        until("xterm's print of its screen", (played.dir / "printed").exists)
        text = (played.dir / "printed").read_text(encoding="utf-8", errors="replace")
        return screen(text, played.rows), cursor
    finally:
        played.leave()
        if process:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        shutil.rmtree(played.dir, ignore_errors=True)


def departures():
    """The rows of the README's tables between their marks: {(case, witness): where it differs}."""
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
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))
    args, names = sys.argv[1:], []
    options = {"--fux": str(REPO / "target/release/fux"), "--old": str(HERE / "old/target/release/referee-old"), "--witness": "old,tmux,xterm"}
    verbose = "-v" in args
    args = [a for a in args if a != "-v"]
    while args:
        arg = args.pop(0)
        if arg in options:
            options[arg] = args.pop(0)
        elif arg.startswith("-"):
            raise SystemExit(__doc__)
        else:
            names.append(arg)
    every = {"old": (old, options["--old"]), "tmux": (tmux, shutil.which("tmux")), "xterm": (xterm, shutil.which("xterm") and shutil.which("Xvfb"))}
    wanted = [name for name in options["--witness"].split(",") if name]
    for name in wanted:
        if name not in every or not every[name][1] or not os.path.exists(every[name][1]):
            raise SystemExit(f"witness {name}: not installed (or not built). --witness names the ones to use")
    witnesses = {name: every[name] for name in wanted}
    for name, asked in (("tmux", ["tmux", "-V"]), ("xterm", ["xterm", "-version"])):
        if name in witnesses:
            print(f"witness {name}: " + subprocess.run(asked, capture_output=True, text=True).stdout.strip())
    cases = sorted(HERE.glob("corpus/**/*.case"))
    named = lambda path: str(path.relative_to(HERE / "corpus").with_suffix(""))
    cases = [c for c in cases if not names or c.stem in names or named(c) in names]
    if not cases or not witnesses or not os.path.exists(options["--fux"]):
        raise SystemExit("nothing to compare: no case of that name, no witness, or no fux at --fux")
    known, new, stale, agree = departures(), [], [], {name: 0 for name in witnesses}
    try:
        for path in cases:
            name, case = named(path), parse(path)
            try:
                ours = fux(case, options["--fux"])
            except Unplayed as error:
                new.append(f"{name} could not be played to fux: {error}")
                continue
            differing = []
            for witness, (play, binary) in witnesses.items():
                try:
                    theirs = play(case, binary)
                except Unplayed as error:
                    new.append(f"{name} could not be played to {witness}: {error}")
                    continue
                where = ", ".join(what for what, i in (("text", 0), ("cursor", 1)) if ours[i] != theirs[i])
                expected = known.get((name, witness))
                if not where:
                    agree[witness] += 1
                    if expected:
                        stale.append((name, witness))
                    continue
                differing.append(witness + ("" if where == expected else " (NEW)"))
                if where != expected:
                    row = f"says {expected}" if expected else "has no row for it"
                    new.append(f"{name} differs from {witness} in {where}, and the README's table {row}")
                if verbose or where != expected:
                    print(f"--- {name}: fux, cursor {ours[1]}")
                    print("\n".join("  |" + row for row in ours[0]))
                    print(f"--- {name}: {witness}, cursor {theirs[1]}")
                    print("\n".join("  |" + row for row in theirs[0]))
            print(f"{name}: " + ("same" if not differing else "differs from " + ", ".join(differing)))
    finally:
        Display.stop()
    print()
    for witness in witnesses:
        listed = len({case for (case, w) in known if w == witness} & {named(path) for path in cases})
        print(f"{witness}: same on {agree[witness]} of {len(cases)} cases; {listed} departures in the README's table")
    for case, witness in stale:
        print(f"note: the README lists {case} as differing from {witness}, and from this {witness} it does not")
    for line in new:
        print("NEW: " + line)
    sys.exit(1 if new else 0)


if __name__ == "__main__":
    main()
