#!/usr/bin/env python3
"""perf.py FUX [STEP…]: how fast fux is, beside tmux, on the same steps.

FUX is the path of the fux binary to measure (a release build). Each multiplexer
runs one pane of 200x60 with `sh` in it, on a session of its own in a temporary
directory that is removed at the end, and is looked at through its viewer (tmux:
its client) in a pty this script owns, as a terminal would own it. The steps
(all of them, or those named):

  flood    `seq 1 2000000` typed into the pane: seconds until what the shell
           prints after it has come out of the viewer.
  key      a key written to the viewer's pty, until its echo (from `cat` in the
           pane) comes out of it: typed slowly, and typed fast.
  idle     CPU of the viewer and of the holder (tmux: client and server) at rest.
           (tmux's server writes to the terminal itself: its client does nothing.)
  busy     a program that repaints all 200x60 cells as fast as it is read: the
           frames it wrote a second, and the CPU of both sides meanwhile.
  slow     the same program at 10 frames a second while the viewer's output is
           read at about 100 KiB a second: a key written to the viewer's pty,
           until the program has read it.

Prints one table: the median and (least to most) of each, for fux and for tmux
(left out when tmux is not installed). Everything runs on two cores where the
system can pin a process (Linux), takes about a minute and a half, and leaves
no process behind. Python's standard library only.
"""
import fcntl, os, pty, random, re, select, shutil, signal, statistics, struct, subprocess, sys, tempfile, termios, time

COLS, ROWS = 200, 60
# The program of the busy and slow steps: SECONDS FRAMES-A-SECOND (0: as fast as read) RESULT.
BUSY = r"""
import os, select, sys, termios, time, tty
seconds, rate, result = float(sys.argv[1]), float(sys.argv[2]), sys.argv[3]
frames = []
for n in range(8):
    rows = []
    for r in range(60):
        runs = ("\x1b[38;5;%dm" % ((n * 37 + r + c) % 256)
                + "".join(chr(0x21 + (r * 3 + c + i + n * 5) % 94) for i in range(8)) for c in range(0, 200, 8))
        rows.append("\x1b[%d;1H" % (r + 1) + "".join(runs))
    frames.append("".join(rows).encode())
old = termios.tcgetattr(0)
tty.setraw(0)
start, n, keys = time.monotonic(), 0, []
while time.monotonic() - start < seconds:
    frame = frames[n % 8]
    while frame:
        frame = frame[os.write(1, frame):]
    n += 1
    left = lambda: max(0, start + n / rate - time.monotonic()) if rate else 0
    while select.select([0], [], [], left())[0]:
        keys += [time.monotonic_ns()] * len(os.read(0, 64))
took = time.monotonic() - start
termios.tcsetattr(0, termios.TCSANOW, old)
os.write(1, b"\x1b[m\x1b[2J\x1b[H")
with open(result + ".part", "w") as file:
    file.write("%d %f\n%s\n" % (n, took, " ".join(map(str, keys))))
os.rename(result + ".part", result)
"""


def cpu(pid):
    """The CPU time a process has used, in seconds."""
    try:
        with open(f"/proc/{pid}/schedstat") as file:
            return int(file.read().split()[0]) / 1e9
    except OSError:
        pass
    # ([[days-]hours:]minutes:seconds, to the hundredth where the system tells it.)
    text = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    days, _, clock = text.rpartition("-")
    parts = [float(part) for part in clock.split(":") if part] or [0.0]
    return int(days or 0) * 86400 + sum(part * 60**i for i, part in enumerate(reversed(parts)))


class Mux:
    """One multiplexer with one pane, and its viewer in a pty of ours."""

    def __init__(self, kind, fux, root):
        self.kind, self.root = kind, os.path.join(root, kind)
        os.makedirs(self.root)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("FUX", "TMUX")) and k != "ENV"}
        self.env.update(TERM="xterm-256color", PS1="$ ", EDITOR="true", COLUMNS=str(COLS), LINES=str(ROWS))
        self.slow = None  # (bytes, seconds): the most read from the viewer in one go, and how often
        self.next_read = 0.0
        self.kill, self.fd, self.screen_side, self.sock = [], None, None, None
        try:
            self.start(fux)
        except BaseException:
            self.stop()
            raise

    def start(self, fux):
        kind, rows = self.kind, ROWS
        if kind == "fux":
            self.env.update(FUXN_DIR=os.path.join(self.root, "session"), FUXN_KEYS=os.path.join(self.root, "keys"))
            pane = subprocess.run([fux, "start", "main/p", "--", "sh"], env=self.env, capture_output=True, text=True, check=True)
            status = dict(line.split(" ", 1) for line in open(os.path.join(pane.stdout.strip(), "status")).read().splitlines())
            self.pane_side = int(status["holder"])
            self.kill += [self.pane_side, int(status["pid"])]
            argv = [fux, "view", "main"]
        else:
            self.sock = os.path.join(self.root, "sock")
            argv = ["tmux", "-S", self.sock, "-f", os.devnull, "new-session", "sh"]
            rows += 1  # its status line
        self.screen_side, self.fd = pty.fork()
        if self.screen_side == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, COLS, 0, 0))
            os.execvpe(argv[0], argv, self.env)
        self.kill.append(self.screen_side)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, COLS, 0, 0))
        if not self.until(b"$", 10):
            raise SystemExit(f"{kind}: no prompt came out of the viewer")
        if kind == "tmux":
            pid = subprocess.run(["tmux", "-S", self.sock, "display", "-p", "#{pid}"], env=self.env, capture_output=True, text=True)
            self.pane_side = int(pid.stdout.strip())
        self.rest(0.5)

    def read(self, wait):
        """What the viewer wrote within `wait` seconds, if anything, as a terminal reads it."""
        now = time.monotonic()
        if self.slow and now < self.next_read:
            time.sleep(min(wait, self.next_read - now))
            return b""
        if not select.select([self.fd], [], [], wait)[0]:
            return b""
        try:
            data = os.read(self.fd, self.slow[0] if self.slow else 1 << 16)
        except OSError:
            raise SystemExit(f"{self.kind}: the viewer ended")
        if self.slow:
            self.next_read = time.monotonic() + self.slow[1]
        # A terminal says what it is when asked: fux's viewer waits a second for that.
        if b"\x1b[c" in data:
            os.write(self.fd, b"\x1b[?62c")
        return data

    def until(self, mark, limit):
        """Reads until `mark` has come out of the viewer: the time it did, or None after `limit` seconds."""
        end, tail = time.monotonic() + limit, b""
        while time.monotonic() < end:
            tail = tail[-len(mark):] + self.read(0.05)
            if mark in tail:
                return time.monotonic()
        return None

    def rest(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            self.read(end - time.monotonic())

    def type(self, text):
        os.write(self.fd, text.encode())

    def sides(self):
        return cpu(self.screen_side), cpu(self.pane_side)

    def stop(self):
        if self.sock:
            subprocess.run(["tmux", "-S", self.sock, "kill-server"], env=self.env, capture_output=True)
        for pid in self.kill:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        if self.screen_side:
            os.close(self.fd)
            os.waitpid(self.screen_side, 0)


def flood(mux, add):
    for _ in range(3):
        code = "".join(random.choice("abcdefgh") for _ in range(6))
        start = time.monotonic()
        mux.type(f"seq 1 2000000; printf 'END%sFLOOD\\n' {code}\r")
        end = mux.until(f"END{code}FLOOD".encode(), 60)
        if end:
            add("flood: seq 1 2000000, until its end is on the screen", "s", end - start)
        mux.rest(0.3)


def keys(mux, add):
    """A key until its echo: letters that no escape sequence the viewers write ends with."""
    mux.type("cat\r")
    mux.rest(0.5)
    for label, count, gap in [("key to echo, typed slowly (50 to 90 ms apart)", 40, (0.05, 0.09)),
                              ("key to echo, typed fast (10 ms apart)", 120, (0.01, 0.01))]:
        waiting, next_key, typed = [], time.monotonic(), 0
        while typed < count or (waiting and time.monotonic() < next_key + 1):
            now = time.monotonic()
            if typed < count and now >= next_key:
                key = b"wxyzjkvg"[typed % 8:typed % 8 + 1]
                os.write(mux.fd, key)
                waiting.append((key, time.monotonic()))
                typed += 1
                next_key = now + random.uniform(*gap)
            data, at, now = mux.read(max(0, min(next_key - now, 0.05)) if typed < count else 0.05), 0, time.monotonic()
            # (Echoes come in the order typed.)
            while waiting and (found := data.find(waiting[0][0], at)) >= 0:
                add(label, "ms", (now - waiting.pop(0)[1]) * 1000)
                at = found + 1
        mux.type("\r")
        mux.rest(0.5)
    mux.type("\x04")
    mux.rest(0.3)


def idle(mux, add):
    mux.rest(4)  # (fux's viewer looks at the files less often after three seconds of nothing)
    for _ in range(3):
        before = mux.sides()
        mux.rest(2)
        for side, was, now in zip(SIDES, before, mux.sides()):
            add(f"idle: CPU of {side}", "% of a core", (now - was) / 2 * 100)


def busy(mux, seconds, rate):
    """Runs the repainting program in the pane; its frames, its seconds, and when it read each key."""
    result = os.path.join(mux.root, "result")
    mux.type(f"python3 {os.path.join(os.path.dirname(mux.root), 'busy.py')} {seconds} {rate} {result}\r")
    yield None
    end = time.monotonic() + seconds + 30
    while not os.path.exists(result) and time.monotonic() < end:
        mux.read(0.02)
    with open(result) as file:
        (frames, took), read = file.readline().split(), [int(at) for at in file.readline().split()]
    os.remove(result)
    yield int(frames), float(took), read


def repaint(mux, add):
    for _ in range(3):
        before, run = mux.sides(), busy(mux, 2, 0)
        next(run)
        frames, took, _ = next(run)
        add(f"busy: all {COLS}x{ROWS} cells repainted, frames the program wrote", "a second", frames / took)
        for side, was, now in zip(SIDES, before, mux.sides()):
            add(f"busy: CPU of {side}", "% of a core", (now - was) / took * 100)
        mux.rest(0.5)


def slow(mux, add):
    run, typed = busy(mux, 5, 10), []
    next(run)
    mux.slow = (1024, 0.01)
    mux.rest(1.5)
    for _ in range(16):
        typed.append(time.monotonic_ns())
        os.write(mux.fd, b"w")
        mux.rest(0.2)
    _, _, read = next(run)
    mux.slow = None
    for written, got in zip(typed, read):
        add("slow link (100 KiB/s read from the viewer): key until the program reads it", "ms", (got - written) / 1e6)
    mux.rest(1)


SIDES = ("the viewer (tmux: the client)", "the holder (tmux: the server)")


def main():
    steps = {"flood": flood, "key": keys, "idle": idle, "busy": repaint, "slow": slow}
    if len(sys.argv) < 2 or not os.access(sys.argv[1], os.X_OK) or not set(sys.argv[2:]) <= set(steps):
        raise SystemExit(__doc__)
    fux = os.path.abspath(sys.argv[1])
    cores = ""
    if hasattr(os, "sched_setaffinity"):
        os.sched_setaffinity(0, sorted(os.sched_getaffinity(0))[-2:])
        cores = f", on cores {sorted(os.sched_getaffinity(0))}"
    kinds = ["fux"] + (["tmux"] if shutil.which("tmux") else [])
    root = tempfile.mkdtemp(prefix="fux-perf-")
    with open(os.path.join(root, "busy.py"), "w") as file:
        file.write(BUSY)
    rows, values = [], {}
    try:
        for kind in kinds:
            def add(label, unit, value, kind=kind):
                if (label, unit) not in rows:
                    rows.append((label, unit))
                values.setdefault((kind, label), []).append(value)
            mux = Mux(kind, fux, root)
            try:
                for name in sys.argv[2:] or steps:
                    print(f"{kind}: {name}", file=sys.stderr, flush=True)
                    steps[name](mux, add)
            finally:
                mux.stop()
    finally:
        shutil.rmtree(root, ignore_errors=True)

    def cell(kind, label):
        got = values.get((kind, label))
        if not got:
            return "-"
        digits = lambda value: f"{value:.3g}" if value < 100 else f"{value:.0f}"
        return f"{digits(statistics.median(got))} ({digits(min(got))} to {digits(max(got))}), {len(got)} runs"
    version = subprocess.run(["tmux", "-V"], capture_output=True, text=True).stdout.strip() if "tmux" in kinds else "no tmux"
    print(f"{fux} beside {version}{cores}: median (least to most)")
    table = [("", *kinds)] + [(f"{label} [{unit}]", *(cell(kind, label) for kind in kinds)) for label, unit in rows]
    widths = [max(len(line[i]) for line in table) for i in range(len(table[0]))]
    for line in table:
        print("  ".join(text.ljust(width) for text, width in zip(line, widths)).rstrip())


if __name__ == "__main__":
    main()
