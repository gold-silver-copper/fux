import atexit
import json
import os
from pathlib import Path
import re
import select
import sys
import termios
import time


class TerminalInput:
    """Decode events individually, even when a paste or key burst is coalesced."""
    def __init__(self, fd):
        self.fd = fd
        self.line = ''
        self.last_end = None

    def byte(self, timeout=None):
        if not select.select([self.fd], [], [], timeout)[0]:
            return None
        value = os.read(self.fd, 1)
        if not value:
            raise EOFError('terminal input closed')
        return value

    def key(self, timeout=None):
        b = self.byte(timeout)
        if b is None:
            return None
        if b != b'\x1b':
            return b
        b2 = self.byte()  # Escape sequences have no inter-byte deadline.
        seq = b + b2
        if b2 == b'O':
            while True:
                c = self.byte()
                seq += c
                if 64 <= c[0] <= 126:
                    break
        elif b2 == b'[':
            while True:
                c = self.byte()
                seq += c
                if 64 <= c[0] <= 126:
                    break
            if seq == b'\x1b[[':
                seq += self.byte()  # Linux console function keys.
            if seq == b'\x1b[M':
                seq += b''.join(self.byte() for _ in range(3))
        return seq

    def feed(self, key):
        if key in (b'\x1b[200~', b'\x1b[201~'):
            return None
        if key in (b'\r', b'\n'):
            if self.last_end is not None and key != self.last_end:
                self.last_end = None
                return None  # CRLF (or LFCR) is one submission.
            self.last_end = key
            result, self.line = self.line, ''
            return result
        self.last_end = None
        if key in (b'\x08', b'\x7f'):
            self.line = self.line[:-1]
        elif key == b'\x15':
            self.line = ''
        elif len(key) == 1 and key[0] >= 32:
            self.line += key.decode('utf-8', errors='replace')
        return None

    def submitted(self, timeout=None):
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            remaining = None if deadline is None else max(0, deadline - time.monotonic())
            key = self.key(remaining)
            if key is None:
                return None
            result = self.feed(key)
            if result is not None:
                return result


class Fixture:
    def __init__(self, work, name):
        if not re.fullmatch(r'[A-Za-z0-9_-]+', name):
            raise SystemExit('invalid fixture instance name')
        self.work = Path(work).resolve()
        self.fx = self.work / '.fx'
        self.fx.mkdir(parents=True, exist_ok=True)
        self.name = name
        (self.fx / (name + '.pid')).write_text(str(os.getpid()) + '\n')
        self.data = {'pid': os.getpid(), 'status': 'running', 'inputs': [],
                     'stdin_tty': sys.stdin.isatty(), 'stdout_tty': sys.stdout.isatty()}
        self.save()
        if not (self.data['stdin_tty'] and self.data['stdout_tty']):
            self.fail('stdin and stdout must both be terminals')
        fd = sys.stdin.fileno()
        original = termios.tcgetattr(fd)
        settings = termios.tcgetattr(fd)
        settings[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
        settings[3] &= ~(termios.ICANON | termios.ECHO)
        settings[6][termios.VMIN] = 1
        settings[6][termios.VTIME] = 0
        termios.tcsetattr(fd, termios.TCSANOW, settings)
        atexit.register(termios.tcsetattr, fd, termios.TCSANOW, original)
        self.terminal = TerminalInput(fd)

    def save(self, **values):
        self.data.update(values)
        self.publish_json(self.name + '.json', self.data)

    def publish_json(self, name, data):
        dest = self.fx / name
        temp = self.fx / (name + '.%d.tmp' % os.getpid())
        temp.write_text(json.dumps(data, sort_keys=True) + '\n')
        temp.replace(dest)

    def out(self, message='', end='\n'):
        print(message, end=end, flush=True)

    def fail(self, reason):
        self.save(status='failed', reason=reason, exit_code=2)
        self.out('\nERROR: ' + reason)
        raise SystemExit(2)

    def guard(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if self.terminal.submitted(max(0, end - time.monotonic())) is not None:
                self.fail('input submitted before an input prompt')

    def wait_file(self, name, message=None):
        if message:
            self.out(message)
        while not (self.fx / name).exists():
            self.guard(.1)
        return self.fx / name

    def response(self, expected):
        response = self.terminal.submitted()
        self.data['inputs'].append(response)
        self.save()
        if response != expected:
            self.fail('unexpected response: ' + repr(response))
        self.out()
        return response

    def ask(self, prompt, expected):
        self.out(prompt, end='')
        return self.response(expected)

    def finish(self, **values):
        self.save(status='complete', exit_code=0, **values)
