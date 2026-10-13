"""Terminal input decoded as keys and submitted text, independent of wire encoding."""
import os
import re
import select
import sys
import time
from collections import deque

class TerminalInput:
    def __init__(self, fail):
        self.fail = fail
        self.fd = sys.stdin.fileno()
        self.pending = deque()
        self.skip_lf = False

    def byte(self, deadline=None):
        while True:
            if self.pending:
                part = self.pending.popleft()
            else:
                if deadline is not None:
                    left = deadline - time.monotonic()
                    if left <= 0 or not select.select([self.fd], [], [], left)[0]:
                        self.fail('prompt deadline expired')
                part = os.read(self.fd, 1)
                if not part:
                    self.fail('input disconnected at prompt')
            if self.skip_lf:
                self.skip_lf = False
                if part == b'\n':
                    continue
            return part

    def keypress(self, deadline=None):
        first = self.byte(deadline)
        if first == b'\r':
            self.skip_lf = True
            return b'\n'
        if first != b'\x1b':
            return first
        second = self.byte(deadline)
        sequence = first + second
        if second == b'[':
            while True:
                part = self.byte(deadline)
                sequence += part
                if 64 <= part[0] <= 126:
                    break
            if sequence == b'\x1b[M':
                # X10 mouse reports have three payload bytes after CSI M.
                sequence += b''.join(self.byte(deadline) for _ in range(3))
            if sequence == b'\x1b[200~':
                pasted = bytearray()
                while not pasted.endswith(b'\x1b[201~'):
                    pasted.extend(self.byte(deadline))
                self.pending.extend(bytes([c]) for c in pasted[:-6])
                return self.keypress(deadline)
        elif second == b'O':
            sequence += self.byte(deadline)
        # Mouse reports normalize to button, coordinates and press/release.
        if sequence.startswith(b'\x1b[<'):
            match = re.fullmatch(rb'\x1b\[<(\d+);(\d+);(\d+)([Mm])', sequence)
            if match:
                button, x, y = (int(v) for v in match.groups()[:3])
                pressed = match[4] == b'M'
                return self.mouse(button, x, y, pressed)
        if sequence.startswith(b'\x1b[M') and len(sequence) == 6:
            button, x, y = (v - 32 for v in sequence[3:])
            return self.mouse(button, x, y, button & 3 != 3)
        if sequence in (b'\x1b[7~', b'\x1b[8~'):
            return b'\x1b[H' if sequence == b'\x1b[7~' else b'\x1b[F'
        # SS3 and CSI arrows, Home/End and F1-F4 express the same keys.
        suffix = sequence[2:]
        if second in (b'O', b'[') and suffix in (b'A', b'B', b'C', b'D', b'H', b'F'):
            return b'\x1b[' + suffix
        if second in (b'O', b'[') and suffix in (b'P', b'Q', b'R', b'S'):
            return b'\x1b[' + str(11 + b'PQRS'.index(suffix)).encode() + b'~'
        return sequence

    @staticmethod
    def mouse(button, x, y, pressed):
        return ('MOUSE %d %d %d %d' % (button, x, y, pressed)).encode()

    def rawline(self, deadline=None):
        result = bytearray()
        while True:
            key = self.keypress(deadline)
            if key == b'\n':
                return result.decode('ascii', errors='replace')
            if key in (b'\x7f', b'\x08'):
                del result[-1:]
            elif key == b'\x15':
                result.clear()
            elif len(key) == 1 and 32 <= key[0] <= 126:
                result.extend(key)
            else:
                self.fail('unexpected text input key')
            if len(result) > 2048:
                self.fail('prompt response is too long')

    def line(self, expected, deadline=None):
        actual = self.rawline(deadline)
        if actual != expected:
            self.fail('expected ' + repr(expected) + ', received ' + repr(actual))
        return actual

    def delay(self, seconds):
        end = time.monotonic() + seconds
        while True:
            # CRLF is one Enter even when the LF arrives after submission.
            while self.pending or select.select([self.fd], [], [], 0)[0]:
                part = self.pending.popleft() if self.pending else os.read(self.fd, 1)
                if self.skip_lf:
                    self.skip_lf = False
                    if part == b'\n':
                        continue
                if not part:
                    self.fail('input disconnected during work')
                self.fail('input before prompt')
            left = end - time.monotonic()
            if left <= 0:
                return
            select.select([self.fd], [], [], min(.1, left))

if __name__ == '__main__':
    raise SystemExit('terminal_io is a support module, not a standalone fixture')
