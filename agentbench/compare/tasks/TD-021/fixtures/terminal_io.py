"""Terminal I/O shared by this task's fixtures; no third-party modules."""
import hashlib
import json
import os
import select
import sys
import termios
import time
from collections import deque
from pathlib import Path


def value(namespace, label, source, width=12):
    return hashlib.sha256((namespace + "|" + label + "|" + source).encode()).hexdigest()[:width].upper()


def atomic(path, text):
    temporary = path.with_name(path.name + ".tmp." + str(os.getpid()))
    temporary.write_text(text, encoding="utf-8")
    os.replace(temporary, path)


def emit(text):
    sys.stdout.write(text)
    sys.stdout.flush()


class Terminal:
    def __init__(self, name, work):
        self.name = name
        self.work = Path(work).resolve()
        self.fx = self.work / ".fx"
        self.fx.mkdir(exist_ok=True)
        self.record = {"pid": os.getpid(), "stdin_tty": sys.stdin.isatty(),
                       "stdout_tty": sys.stdout.isatty(), "ok": False, "stage": "starting"}
        atomic(self.fx / (name + ".pid"), str(os.getpid()) + "\n")
        self.save()
        if not (self.record["stdin_tty"] and self.record["stdout_tty"]):
            self.record["error"] = "Both stdin and stdout must be terminals"
            self.save()
            raise SystemExit(2)
        self.record["tty"] = os.ttyname(0)
        self.save()
        self.old = termios.tcgetattr(0)
        mode = termios.tcgetattr(0)
        mode[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR | termios.IXON)
        mode[3] &= ~(termios.ICANON | termios.ECHO | termios.ECHONL)
        mode[6][termios.VMIN] = 1
        mode[6][termios.VTIME] = 0
        termios.tcsetattr(0, termios.TCSANOW, mode)
        self.pending = b""
        self.events = deque()
        self.after_cr = False

    def __enter__(self):
        return self

    def __exit__(self, kind, error, traceback):
        termios.tcsetattr(0, termios.TCSANOW, self.old)

    def save(self, **changes):
        self.record.update(changes)
        atomic(self.fx / (self.name + ".json"), json.dumps(self.record, sort_keys=True) + "\n")

    def fail(self, reason):
        self.save(ok=False, stage="failed", error=reason)
        emit("\nINPUT REJECTED: " + reason + ". Restart this attempt.\n")
        raise SystemExit(3)

    def _decode(self):
        sequences = {b"\x1bOQ": "F2", b"\x1b[12~": "F2", b"\x1b[[B": "F2",
                     b"\x1b[1;1Q": "F2", b"\x1b[Q": "F2",
                     b"\x1b[200~": None, b"\x1b[201~": None}
        while self.pending:
            if self.pending.startswith(b"\x1b"):
                matched = False
                for sequence, event in sequences.items():
                    if self.pending.startswith(sequence):
                        self.pending = self.pending[len(sequence):]
                        if event:
                            self.events.append(("key", event))
                        matched = True
                        break
                if matched:
                    continue
                if any(sequence.startswith(self.pending) for sequence in sequences):
                    return
                # No other escape sequences have a meaning in these programs.
                self.events.append(("key", "unsupported escape"))
                self.pending = self.pending[1:]
                continue
            byte, self.pending = self.pending[0], self.pending[1:]
            if byte == 10 and self.after_cr:
                self.after_cr = False
                continue
            self.after_cr = byte == 13
            if byte in (10, 13):
                self.events.append(("enter", ""))
            elif byte in (8, 127):
                self.events.append(("backspace", ""))
            elif byte == 21:
                self.events.append(("clear", ""))
            elif 32 <= byte < 127:
                self.events.append(("char", chr(byte)))
            else:
                self.events.append(("key", "control-" + str(byte)))

    def event(self, timeout=None):
        self._decode()
        if self.events:
            return self.events.popleft()
        ready, _, _ = select.select([0], [], [], timeout)
        if not ready:
            return None
        data = os.read(0, 4096)
        if not data:
            self.fail("terminal input closed")
        self.pending += data
        self._decode()
        if self.events:
            return self.events.popleft()
        return None

    def quiet(self, seconds, stage):
        """Processing delay, with any premature input rejected and recorded."""
        self.save(stage=stage)
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if self.event(min(0.1, max(0, end - time.monotonic()))) is not None:
                self.fail("input before " + stage + " finished")
        if self.event(0) is not None:
            self.fail("input before " + stage + " finished")

    def line(self, echo=True):
        text = []
        while True:
            event = self.event()
            if event is None:
                continue
            kind, char = event
            if kind == "enter":
                if echo:
                    emit("\n")
                return "".join(text)
            if kind == "char":
                text.append(char)
                if echo:
                    emit(char)
            elif kind == "backspace":
                if text:
                    text.pop()
                    if echo:
                        emit("\b \b")
            elif kind == "clear":
                if echo:
                    emit("\b \b" * len(text))
                text.clear()
            else:
                self.fail("unexpected key " + char)

    def expect_key(self, key):
        while True:
            event = self.event()
            if event is None:
                continue
            if event == ("key", key):
                return
            self.fail("expected " + key)


def read_record(work, name):
    try:
        return json.loads((Path(work) / ".fx" / (name + ".json")).read_text())
    except (OSError, ValueError):
        return {}


def alive(record):
    try:
        os.kill(record["pid"], 0)
        return True
    except (KeyError, ProcessLookupError, PermissionError):
        return False


if __name__ == "__main__":
    raise SystemExit("Support module only; launch the program named in the goal in a terminal")
