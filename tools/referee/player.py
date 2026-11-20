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


def ask(question, answer):
    """Writes a question and gives the terminal's last answer of that shape, once it has said
    all it had to say: a case's own questions are answered before this one."""
    write(question)
    heard, deadline = b"", time.monotonic() + 5
    while True:
        found = list(re.finditer(answer, heard))
        ended = found and found[-1].end() == len(heard)
        if not select.select([0], [], [], 0.03 if ended else 0.5)[0]:
            if ended:
                return found[-1]
            if time.monotonic() > deadline:
                fail(f"no answer to {question!r}: got {heard!r}")
            continue
        heard += os.read(0, 4096)


def taken_in():
    """"Are you well?": the answer comes once everything before it was taken in."""
    ask(b"\x1b[5n", rb"\x1b\[0n")


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
at = ask(b"\x1b[6n", rb"\x1b\[(\d+);(\d+)R")
open(f"{out}/cursor.new", "w").write(f"{int(at[1]) - 1} {int(at[2]) - 1}\n")
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
