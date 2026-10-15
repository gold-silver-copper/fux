"""The oracle's fixture: a terminal program that does what it is told,
deterministically, and writes a side log of everything it does in the
frozen format of side-log-format.md to $FUX_ORACLE_LOG.

It reads lines and answers each one. How a line is typed depends on
--mode:

  line      line mode, the terminal echoes (as Python's input())
  password  line mode, no echo: the reply lands on the prompt's row
  readline  raw mode, its own line editor that redraws in place
  ptk       raw mode, redraws the whole input line on every key
  full      raw mode on the alternate screen (a full-screen form)

A line is `[COMMAND] INnnnn`; nnnn names what it prints (OUTnnnn,
TKnnnnA..). Commands:

  (none)   reply: OUTnnnn, or one that repeats the input (starts with it,
           ends with it, is exactly it), chosen from the seed
  sleep    sleep, then reply           work     compute, then reply
  bell     a bell, alone or with the reply in one write, either order
  over     print the reply and draw over it in the same write
  wrap     the reply inside a line longer than two rows
  scroll   many lines, the reply among the first
  ticker   another thread prints TKnnnnA, B, ... on a timer
  timed    reads with the terminal's own time limit (VMIN 0, VTIME),
           replies after two empty reads (logged as `block on poll`:
           it returns alone)
  poll     polls with a timeout, replies after three empty polls
  alt      switches between the main and the alternate screen
  quit     exits

--chunk N writes everything N bytes at a time, each its own write.
The fixture starts no other process.
"""

import argparse
import base64
import json
import os
import random
import re
import select
import shutil
import sys
import termios
import threading
import time

LOCK = threading.Lock()
SEQ = [0]
LOG = None
CHUNK = [0]


def record(**fields):
    """Appends one record under the lock (the caller holds it)."""
    SEQ[0] += 1
    if LOG is None:
        return
    line = json.dumps(dict(seq=SEQ[0], **fields), separators=(",", ":"))
    LOG.write(line + "\n")
    LOG.flush()


def log(**fields):
    with LOCK:
        record(**fields)


def write_all(data):
    while data:
        n = os.write(1, data)
        data = data[n:]


def out(data, role, answers, thread="main"):
    """Writes `data` to the terminal, each write logged before it is made,
    under the lock, so record order is write order."""
    if isinstance(data, str):
        data = data.encode()
    size = CHUNK[0] or len(data) or 1
    # A long write (a burst) in pieces of at least a sixteenth of it, so
    # that it stays a burst.
    if len(data) > 256:
        size = max(size, len(data) // 16)
    pieces = [data[i:i + size] for i in range(0, len(data), size)]
    for n, piece in enumerate(pieces):
        if n and CHUNK[0]:
            time.sleep(0.002)
        with LOCK:
            record(ev="write", b64=base64.b64encode(piece).decode(), role=role,
                   answers=answers, thread=thread)
            if b"\x07" in piece:
                record(ev="bell", answers=answers, thread=thread)
            write_all(piece)


class Term:
    def __init__(self, mode):
        self.saved = termios.tcgetattr(0)
        self.raw = mode in ("readline", "ptk", "full")
        self.echo = mode == "line"
        self.alt = False
        self.timed = None
        self.apply()

    def apply(self):
        attrs = termios.tcgetattr(0)
        lflag = attrs[3]
        lflag = lflag | termios.ICANON if not self.raw else lflag & ~termios.ICANON
        lflag = lflag | termios.ECHO if self.echo else lflag & ~termios.ECHO
        if self.raw:
            lflag &= ~termios.IEXTEN
            attrs[0] &= ~termios.ICRNL
        attrs[3] = lflag
        cc = attrs[6]
        if self.timed is not None:
            cc[termios.VMIN], cc[termios.VTIME] = 0, self.timed
        elif self.raw:
            cc[termios.VMIN], cc[termios.VTIME] = 1, 0
        termios.tcsetattr(0, termios.TCSANOW, attrs)

    def modes(self):
        log(ev="mode", raw=self.raw, echo=self.echo, alt=self.alt)

    def restore(self):
        termios.tcsetattr(0, termios.TCSANOW, self.saved)


class Fixture:
    def __init__(self, args):
        self.args = args
        self.mode = args.mode
        self.term = Term(args.mode)
        self.consumed = 0
        self.pending = b""
        self.line = []
        self.cursor = 0
        self.replies = []
        self.tickers = []

    # Reading input.

    def read(self):
        """One blocking read of the terminal; its bytes."""
        log(ev="block", on="read")
        data = os.read(0, 4096)
        self.consumed += len(data)
        log(ev="read", b64=base64.b64encode(data).decode(), consumed=self.consumed)
        return data

    def byte(self):
        """The next byte typed, and the input consumed once it was read."""
        while not self.pending:
            self.pending = self.read()
        b, self.pending = self.pending[:1], self.pending[1:]
        return b, self.consumed - len(self.pending)

    def key(self):
        """The next key: its bytes, and `consumed` once it was read."""
        b, at = self.byte()
        if b != b"\x1b":
            return b, at
        seq = b
        nxt, at = self.byte()
        seq += nxt
        if nxt in (b"[", b"O"):
            while True:
                c, at = self.byte()
                seq += c
                if c.isalpha() or c == b"~":
                    break
        return seq, at

    def canonical_line(self):
        """A line as the terminal's line editing hands it over."""
        while b"\n" not in self.pending:
            self.pending += self.read()
        line, self.pending = self.pending.split(b"\n", 1)
        return line.decode(errors="replace"), self.consumed - len(self.pending)

    # Drawing.

    def prompt(self, answers):
        if self.term.alt:
            self.draw_screen(answers)
        elif self.mode == "password":
            out("Password: ", "output", answers)
        else:
            out("> ", "output", answers)

    def input_row(self):
        return "\x1b[6;1H" if self.term.alt else "\r"

    def redraw(self, answers):
        """The whole input line drawn again, the cursor where it is."""
        text = "".join(self.line)
        back = len(self.line) - self.cursor
        out(self.input_row() + "\x1b[K> " + text + (f"\x1b[{back}D" if back else ""),
            "echo", answers)

    def draw_screen(self, answers):
        rows = ["\x1b[H\x1b[2JFORM " + self.mode.upper()]
        for n, reply in enumerate(self.replies[-8:]):
            rows.append(f"\x1b[{8 + n};1H{reply}")
        out("".join(rows), "output", answers)
        self.redraw(answers)

    def edit(self, key, at):
        """A key edits the line; it draws its echo."""
        ptk = self.mode in ("ptk", "full")
        line, cur = self.line, self.cursor
        if key in (b"\x7f", b"\x08"):
            if cur == 0:
                return
            del line[cur - 1]
            self.cursor -= 1
            if ptk:
                return self.redraw(at)
            rest = "".join(line[self.cursor:])
            out("\b" + rest + " " + f"\x1b[{len(rest) + 1}D", "echo", at)
        elif key == b"\x15":
            del line[:cur]
            self.cursor = 0
            self.redraw(at)
        elif key == b"\x17":
            start = cur
            while start > 0 and line[start - 1] == " ":
                start -= 1
            while start > 0 and line[start - 1] != " ":
                start -= 1
            del line[start:cur]
            self.cursor = start
            self.redraw(at)
        elif key in (b"\x1b[D", b"\x1bOD", b"\x02"):
            if cur > 0:
                self.cursor -= 1
                self.redraw(at) if ptk else out("\x1b[D", "echo", at)
        elif key in (b"\x1b[C", b"\x1bOC", b"\x06"):
            if cur < len(line):
                self.cursor += 1
                self.redraw(at) if ptk else out("\x1b[C", "echo", at)
        elif key in (b"\x1b[H", b"\x1bOH", b"\x1b[1~", b"\x01"):
            self.cursor = 0
            self.redraw(at) if ptk else out("\r\x1b[2C", "echo", at)
        elif key in (b"\x1b[F", b"\x1bOF", b"\x1b[4~", b"\x05"):
            n = len(line) - cur
            self.cursor = len(line)
            if n:
                self.redraw(at) if ptk else out(f"\x1b[{n}C", "echo", at)
        elif key == b"\x1b[3~":
            if cur < len(line):
                del line[cur]
                if ptk:
                    return self.redraw(at)
                rest = "".join(line[cur:])
                out(rest + " " + f"\x1b[{len(rest) + 1}D", "echo", at)
        elif len(key) == 1 and 32 <= key[0] < 127:
            ch = key.decode()
            line.insert(cur, ch)
            self.cursor += 1
            if ptk:
                return self.redraw(at)
            rest = "".join(line[self.cursor:])
            out(ch + rest + (f"\x1b[{len(rest)}D" if rest else ""), "echo", at)

    def raw_line(self):
        """A line typed into the fixture's own editor."""
        while True:
            key, at = self.key()
            if key in (b"\r", b"\n"):
                text = "".join(self.line)
                self.line, self.cursor = [], 0
                # Off the line typed, as a form moves on from its field.
                out("\r\n", "echo", at)
                return text, at
            self.edit(key, at)

    # Answering.

    def rng(self, token, what):
        return random.Random(f"{self.args.seed}:{token}:{what}")

    def reply(self, text, answers):
        if self.term.alt:
            self.replies.append(text)
            self.draw_screen(answers)
        else:
            out(text + "\r\n", "output", answers)

    def answer(self, line, at):
        words = line.split()
        token = next((w for w in words if re.fullmatch(r"IN\d{4}", w)), None)
        if token is None:
            return
        n = token[2:]
        reply = "OUT" + n
        command = words[0] if words[0] != token else ""
        rng = self.rng(token, command)
        if command == "quit":
            return "quit"
        if command == "sleep":
            log(ev="block", on="sleep")
            time.sleep(rng.uniform(0.2, 0.6))
            self.reply(reply, at)
        elif command == "work":
            log(ev="block", on="work")
            end = time.monotonic() + rng.uniform(0.3, 0.8)
            while time.monotonic() < end:
                sum(range(1000))
            self.reply(reply, at)
        elif command == "bell":
            kind = rng.randrange(4)
            if kind == 0:
                out("\x07", "output", at)
                self.reply(reply, at)
            elif kind == 1:
                out(reply + "\x07\r\n", "output", at)
            elif kind == 2:
                out("\x07" + reply + "\r\n", "output", at)
            else:
                self.reply(reply, at)
                log(ev="block", on="sleep")
                time.sleep(0.2)
                out("\x07", "output", at)
        elif command == "over":
            blank = "_" * (len(reply) + 2)
            how = rng.randrange(2)
            draw = "\r" + blank if how == 0 else "\r\x1b[2K" + blank
            out(reply + draw + "\r\n", "output", at)
        elif command == "wrap":
            cols = shutil.get_terminal_size().columns
            out("x" * (cols + 7) + " " + reply + " " + "y" * cols + "\r\n", "output", at)
        elif command == "scroll":
            count = rng.randrange(150, 250)
            at_line = rng.randrange(3, 10)
            lines = [reply if i == at_line else f"LINE{n}-{i:03d}" for i in range(count)]
            out("\r\n".join(lines) + "\r\n", "output", at)
        elif command == "ticker":
            gap = rng.uniform(0.1, 0.2)
            thread = threading.Thread(target=self.tick, args=(n, gap), daemon=True)
            self.tickers.append(thread)
            self.reply(reply, at)
            thread.start()
        elif command == "timed":
            self.reply("timed " + n, at)
            if self.term.raw:
                self.term.timed = 2
                self.term.apply()
                empty = 0
                while empty < 2:
                    log(ev="block", on="poll")
                    data = os.read(0, 4096)
                    self.consumed += len(data)
                    log(ev="read", b64=base64.b64encode(data).decode(), consumed=self.consumed)
                    self.pending += data
                    empty = empty + 1 if not data else empty
                self.term.timed = None
                self.term.apply()
                self.reply(reply, at)
            else:
                self.poll(reply, at)
        elif command == "poll":
            self.poll(reply, at)
        elif command == "alt":
            self.term.alt = not self.term.alt
            out("\x1b[?1049h" if self.term.alt else "\x1b[?1049l", "output", at)
            self.term.modes()
            self.reply(reply, at)
        else:
            kind = rng.randrange(4)
            text = [reply, token + " " + reply, reply + " " + token, token][kind]
            self.reply(text, at)
        return None

    def poll(self, reply, at):
        for _ in range(3):
            log(ev="block", on="poll")
            ready, _, _ = select.select([0], [], [], 0.2)
            if ready:
                break
        self.reply(reply, at)

    def tick(self, n, gap):
        for letter in "ABCDEFGH":
            time.sleep(gap)
            # A blank first: a word of its own beside what was typed.
            out(f" TK{n}{letter}\r\n", "output", None, "ticker")

    def run(self):
        self.term.modes()
        if self.mode == "full":
            self.term.alt = True
            out("\x1b[?1049h", "output", None)
            self.term.modes()
        self.prompt(None)
        while True:
            if self.term.raw:
                line, at = self.raw_line()
            else:
                line, at = self.canonical_line()
            if self.answer(line, at) == "quit":
                return 0
            self.prompt(at)


def main():
    global LOG
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", default="line",
                        choices=["line", "password", "readline", "ptk", "full"])
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--chunk", type=int, default=0)
    args = parser.parse_args()
    CHUNK[0] = args.chunk
    path = os.environ.get("FUX_ORACLE_LOG")
    if path:
        LOG = open(path, "a", encoding="utf-8")
    log(ev="start", name="fixture", argv=sys.argv[1:])
    fixture = Fixture(args)
    status = 1
    try:
        status = fixture.run()
    finally:
        if fixture.term.alt:
            out("\x1b[?1049l", "output", None)
        fixture.term.restore()
        log(ev="exit", status=status)
    return status


if __name__ == "__main__":
    sys.exit(main())
