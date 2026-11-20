#!/usr/bin/env python3
"""Runs inside a terminal under test: plays a case's steps to it and asks it where its cursor is.

usage: player.py STEPS OUT [xterm]
STEPS is the file referee.py wrote: one step a line, `feed HEX` or `resize COLS ROWS`.
OUT is a directory: `ready-N` appears when step N wants the terminal resized (the referee does
it, or with `xterm` this program asks the terminal itself), `cursor` gets "ROW COL" (from 0),
`done` appears at the end, and the program leaves when `exit` appears.
"""
import fcntl, os, select, struct, sys, termios, time, tty

steps, out = sys.argv[1], sys.argv[2]
xterm = len(sys.argv) > 3


def reply(question, ends):
    """Writes a question and reads the terminal's answer, up to the byte that ends it."""
    os.write(1, question)
    answer = b""
    while not answer.endswith(ends):
        if not select.select([0], [], [], 5)[0]:
            raise SystemExit(f"no answer to {question!r}: got {answer!r}")
        answer += os.read(0, 1)
    return answer


def size():
    rows, cols = struct.unpack("HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))[:2]
    return cols, rows


tty.setraw(0)
for number, line in enumerate(open(steps).read().split("\n")):
    kind, _, rest = line.partition(" ")
    if kind == "feed":
        os.write(1, bytes.fromhex(rest))
        # "Are you well?": the answer comes once everything before it was taken in.
        reply(b"\x1b[5n", b"n")
    elif kind == "resize":
        cols, rows = map(int, rest.split())
        open(f"{out}/ready-{number}", "w").close()
        if xterm:
            os.write(1, b"\x1b[8;%d;%dt" % (rows, cols))
        deadline = time.monotonic() + 5
        while size() != (cols, rows):
            if time.monotonic() > deadline:
                raise SystemExit(f"the terminal did not become {cols}x{rows}: it is {size()}")
            time.sleep(0.005)
        reply(b"\x1b[5n", b"n")
row, col = reply(b"\x1b[6n", b"R")[2:-1].split(b";")
open(f"{out}/cursor.new", "w").write(f"{int(row) - 1} {int(col) - 1}\n")
os.rename(f"{out}/cursor.new", f"{out}/cursor")
if xterm:
    # Print the screen (to the command the referee gave xterm), and wait until it has.
    reply(b"\x1b[?19h\x1b[i\x1b[5n", b"n")
open(f"{out}/done", "w").close()
deadline = time.monotonic() + 20
while not os.path.exists(f"{out}/exit") and time.monotonic() < deadline:
    time.sleep(0.01)
