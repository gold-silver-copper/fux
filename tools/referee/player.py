#!/usr/bin/env python3
"""Runs inside a terminal under test: plays a case's steps to it and asks it where its cursor is.

usage: player.py STEPS OUT [xterm]
STEPS is the file referee.py wrote: `size COLS ROWS`, then one step a line, `feed HEX` or
`resize COLS ROWS`. The feeds are written as they are, with nothing of this program's between
them: it asks the terminal something only around a resize and at the end.
OUT is a directory: `ready-N` appears when step N wants the terminal resized (the referee does
it, or with `xterm` this program asks the terminal itself), `cursor` gets "ROW COL" (from 0),
`done` appears at the end, and the program leaves when `exit` appears. Anything that goes wrong
is written to `failed`.
"""
import fcntl, os, re, select, struct, sys, termios, time, tty

steps, out = sys.argv[1], sys.argv[2]
xterm = len(sys.argv) > 3


def fail(why):
    open(f"{out}/failed", "w").write(why + "\n")
    raise SystemExit(why)


def write(data):
    while data:
        # The terminal may be answering the case's own questions: take them in, or both sides wait.
        if select.select([0], [], [], 0)[0]:
            os.read(0, 4096)
        if select.select([], [1], [], 0.05)[1]:
            data = data[os.write(1, data[:1024]) :]


def taken_in(question=b""):
    """The status reply comes after the case's replies and any question sent here."""
    write(question + b"\x1b[5n")
    heard, deadline = b"", time.monotonic() + 5
    while not heard.endswith(b"\x1b[0n"):
        if not select.select([0], [], [], max(0, deadline - time.monotonic()))[0] or time.monotonic() >= deadline:
            fail(f"no status reply after {question!r}: got {heard!r}")
        heard += os.read(0, 4096)
    return heard[:-4]


def size():
    rows, cols = struct.unpack("HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))[:2]
    return cols, rows


def becomes(cols, rows):
    deadline = time.monotonic() + 5
    while size() != (cols, rows):
        if time.monotonic() > deadline:
            fail(f"the terminal is {size()}, not {cols}x{rows}")
        time.sleep(0.005)


tty.setraw(0)
for number, line in enumerate(open(steps).read().split("\n")):
    kind, _, rest = line.partition(" ")
    if kind == "size":
        becomes(*map(int, rest.split()))
    elif kind == "feed":
        write(bytes.fromhex(rest))
    elif kind == "resize":
        cols, rows = map(int, rest.split())
        taken_in()
        open(f"{out}/ready-{number}", "w").close()
        if xterm:
            write(b"\x1b[8;%d;%dt" % (rows, cols))
        becomes(cols, rows)
        taken_in()
positions = re.findall(rb"\x1b\[(\d+);(\d+)R", taken_in(b"\x1b[6n"))
if not positions:
    fail("no cursor reply before the status reply")
at = positions[-1]
open(f"{out}/cursor.new", "w").write(f"{int(at[0]) - 1} {int(at[1]) - 1}\n")
os.rename(f"{out}/cursor.new", f"{out}/cursor")
if xterm:
    # Print the whole screen to the command the referee gave xterm; `final` tells that command
    # this is the print to keep, whatever the case printed itself.
    open(f"{out}/final", "w").close()
    write(b"\x1b[?19h\x1b[i")
    taken_in()
open(f"{out}/done", "w").close()
deadline = time.monotonic() + 20
while not os.path.exists(f"{out}/exit") and time.monotonic() < deadline:
    time.sleep(0.01)
