"""Shared, standard-library-only terminal and frozen side-log support."""
import argparse
import base64
import json
import os
import random
import select
import sys
import termios
import threading
import time


class Terminal:
    def __init__(self, name):
        if not os.isatty(0) or not os.isatty(1):
            raise SystemExit('fixture requires a terminal on stdin and stdout')
        self.lock = threading.RLock()
        self.consumed = 0
        self.alt = False
        self.original = termios.tcgetattr(0)
        path = os.environ.get('FUX_ORACLE_LOG')
        self.log = open(path, 'a', encoding='utf-8') if path else None
        self.seq = 0
        self.event('start', name=name, argv=sys.argv[1:])
        self.mode_record()
        self.set_mode(raw=False, echo=True)

    def event(self, ev, **fields):
        with self.lock:
            self.seq += 1
            if self.log:
                self.log.write(json.dumps(dict(seq=self.seq, ev=ev, **fields), separators=(',', ':')) + '\n')
                self.log.flush()

    def mode_record(self):
        a = termios.tcgetattr(0)
        self.event('mode', raw=not bool(a[3] & termios.ICANON), echo=bool(a[3] & termios.ECHO), alt=self.alt)

    def set_mode(self, raw, echo, vmin=1, vtime=0):
        with self.lock:
            a = termios.tcgetattr(0)
            # No output translation: write records are exactly the PTY bytes.
            a[1] = 0
            a[0] &= ~(termios.INLCR | termios.IGNCR | termios.IXON | termios.ISTRIP)
            a[3] &= ~(termios.ECHO | termios.ECHONL)
            if raw:
                a[0] &= ~termios.ICRNL
                a[3] &= ~(termios.ICANON | termios.ISIG | termios.IEXTEN)
            else:
                a[0] |= termios.ICRNL
                a[3] |= termios.ICANON | termios.ISIG | termios.IEXTEN
            if echo:
                a[3] |= termios.ECHO
            a[6][termios.VMIN] = vmin
            a[6][termios.VTIME] = vtime
            termios.tcsetattr(0, termios.TCSANOW, a)
            self.mode_record()

    def write(self, data, role='output', answers=None, thread='main'):
        if isinstance(data, str):
            data = data.encode('utf-8')
        with self.lock:
            # Each successful syscall gets its own exact record, including partial writes.
            offset = 0
            while offset < len(data):
                n = os.write(1, data[offset:])
                self.event('write', b64=base64.b64encode(data[offset:offset+n]).decode(), role=role, answers=answers, thread=thread)
                offset += n

    def bell(self, text=b'', answers=None, thread='main'):
        with self.lock:
            self.write(b'\a' + text, answers=answers, thread=thread)
            self.event('bell', answers=answers, thread=thread)

    def read(self, n=4096):
        self.event('block', on='read')
        data = os.read(0, n)
        with self.lock:
            self.consumed += len(data)
            self.event('read', b64=base64.b64encode(data).decode(), consumed=self.consumed)
        return data

    def sleep(self, seconds, on='sleep'):
        self.event('block', on=on)
        time.sleep(seconds)

    def poll(self, seconds):
        self.event('block', on='poll')
        return bool(select.select([0], [], [], seconds)[0])

    def alternate(self, enabled):
        with self.lock:
            self.write(b'\x1b[?1049h' if enabled else b'\x1b[?1049l')
            self.alt = enabled
            self.mode_record()

    def finish(self, status=0):
        with self.lock:
            if self.alt:
                self.alternate(False)
            termios.tcsetattr(0, termios.TCSANOW, self.original)
            self.mode_record()
            self.event('exit', status=status)
            if self.log:
                self.log.close()


def arguments(description, configure=None):
    p = argparse.ArgumentParser(description=description)
    p.add_argument('--seed', type=int, default=0)
    if configure:
        configure(p)
    return p.parse_args()


def run(name, args, body):
    t = Terminal(name)
    try:
        body(t, args, random.Random(args.seed))
    except BaseException as exc:
        # Even abnormal diagnostics must go through the terminal/log lock.
        status = 130 if isinstance(exc, KeyboardInterrupt) else 1
        t.write('\r\nfixture stopped: %s: %s\r\n' % (type(exc).__name__, exc))
        t.finish(status)
        raise SystemExit(status) from None
    else:
        t.finish()


class Editor:
    """ASCII byte editor; escape sequences are consumed byte by byte."""
    def __init__(self, t, prompt='edit> ', area=False):
        self.t, self.prompt, self.area = t, prompt, area
        self.buf, self.cursor = '', 0
        self.guard = threading.RLock()
        self.started = False
        self.suggestions = ['deploy', 'describe', 'delete']

    def paint(self, role='echo', answers=None, thread='main'):
        with self.guard:
            if self.area:
                candidates = ', '.join(x for x in self.suggestions if x.startswith(self.buf)) or '(no matches)'
                data = '\r\x1b[2K\x1b[36m' + self.prompt + '\x1b[0m' + self.buf
                data += '\r\n\x1b[2K  completions: ' + candidates
                data += '\r\n\x1b[2K  Tab completes | Enter submits'
                data += '\x1b[2A\r\x1b[%dC' % (len(self.prompt) + self.cursor)
            else:
                data = '\r\x1b[2K\x1b[32m' + self.prompt + '\x1b[0m' + self.buf
                if self.cursor < len(self.buf):
                    data += '\x1b[%dD' % (len(self.buf) - self.cursor)
            self.t.write(data, role=role, answers=answers, thread=thread)
            self.started = True

    def key(self):
        key = self.t.read(1)
        if key == b'\x1b':
            key += self.t.read(1)
            if key[-1:] in (b'[', b'O'):
                for _ in range(6):
                    c = self.t.read(1)
                    key += c
                    if not c or 0x40 <= c[0] <= 0x7e:
                        break
        return key

    def line(self, already_painted=False):
        if not already_painted:
            self.paint('output', answers=self.t.consumed or None)
        while True:
            key = self.key()
            with self.guard:
                a = self.t.consumed
                if key in (b'\r', b'\n'):
                    if self.area:
                        self.t.write(b'\x1b[2B\r', role='echo', answers=a)
                    self.t.write(b'\r\n', role='echo', answers=a)
                    return self.buf
                if key in (b'\x7f', b'\x08'):
                    if self.cursor:
                        self.buf = self.buf[:self.cursor-1] + self.buf[self.cursor:]
                        self.cursor -= 1
                    else:
                        self.t.bell(answers=a)
                elif key == b'\x15':
                    self.buf = self.buf[self.cursor:]
                    self.cursor = 0
                elif key == b'\x17':
                    start = self.cursor
                    while start and self.buf[start-1].isspace():
                        start -= 1
                    while start and not self.buf[start-1].isspace():
                        start -= 1
                    self.buf = self.buf[:start] + self.buf[self.cursor:]
                    self.cursor = start
                elif key in (b'\x1b[D', b'\x1bOD'):
                    self.cursor = max(0, self.cursor-1)
                elif key in (b'\x1b[C', b'\x1bOC'):
                    self.cursor = min(len(self.buf), self.cursor+1)
                elif key in (b'\x1b[H', b'\x1bOH', b'\x01'):
                    self.cursor = 0
                elif key in (b'\x1b[F', b'\x1bOF', b'\x05'):
                    self.cursor = len(self.buf)
                elif key == b'\x1b[3~':
                    self.buf = self.buf[:self.cursor] + self.buf[self.cursor+1:]
                elif key == b'\t' and self.area:
                    matches = [x for x in self.suggestions if x.startswith(self.buf)]
                    if matches:
                        self.buf = matches[0]
                        self.cursor = len(self.buf)
                    else:
                        self.t.bell(answers=a)
                elif len(key) == 1 and 32 <= key[0] < 127:
                    self.buf = self.buf[:self.cursor] + key.decode('ascii') + self.buf[self.cursor:]
                    self.cursor += 1
                else:
                    self.t.bell(answers=a)
                self.paint(answers=a)
