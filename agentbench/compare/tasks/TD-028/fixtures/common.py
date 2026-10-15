import atexit
import hashlib
import json
import os
from pathlib import Path
import re
import select
import sys
import termios
import time

def token(label, size=10):
    return hashlib.sha256(('terminal-lab-6107:' + label).encode()).hexdigest()[:size]

def read_record(work, name):
    try:
        return json.loads((Path(work) / '.fx' / (name + '.json')).read_text())
    except (OSError, ValueError):
        return {}

class Fixture:
    def __init__(self, work, name):
        self.work = Path(work).resolve()
        self.directory = self.work / '.fx'
        self.directory.mkdir(exist_ok=True)
        self.name = name
        old = read_record(self.work, name)
        self.r = dict(runs=old.get('runs', 0) + 1, tty=bool(sys.stdin.isatty() and sys.stdout.isatty()),
                      pid=os.getpid(), terminal=os.ttyname(0) if sys.stdin.isatty() else None,
                      state='starting', inputs=[])
        self.queue = ''
        self.escape = ''
        self.save()
        self.atomic(name + '.pid', str(os.getpid()) + '\n')
        if not self.r['tty']:
            self.fail('stdin and stdout must both be terminals')
        if self.r['runs'] != 1:
            self.fail('run this program exactly once')
        self.original = termios.tcgetattr(0)
        mode = termios.tcgetattr(0)
        mode[3] &= ~termios.ICANON
        mode[3] |= termios.ECHO
        mode[0] &= ~termios.ICRNL
        mode[6][termios.VMIN] = 1
        mode[6][termios.VTIME] = 0
        termios.tcsetattr(0, termios.TCSANOW, mode)
        atexit.register(self.restore)

    def restore(self):
        try:
            termios.tcsetattr(0, termios.TCSANOW, self.original)
        except (OSError, termios.error):
            pass

    def atomic(self, name, value):
        path = self.directory / name
        temporary = self.directory / (name + '.tmp.' + str(os.getpid()))
        temporary.write_text(value)
        temporary.replace(path)

    def save(self):
        self.atomic(self.name + '.json', json.dumps(self.r, sort_keys=True) + '\n')

    def out(self, text='', end='\n'):
        print(text, end=end, flush=True)

    def fail(self, reason):
        self.r.update(state='failed', reason=reason)
        self.save()
        self.out('ERROR: ' + reason)
        raise SystemExit(92)

    def incoming(self):
        data = os.read(0, 4096).decode('utf-8', 'replace')
        if not data:
            self.fail('terminal input closed')
        for char in data:
            if self.escape:
                self.escape += char
                if len(self.escape) == 2 and char not in '[O':
                    self.escape = ''
                elif len(self.escape) >= 3 and ('@' <= char <= '~'):
                    self.escape = ''
            elif char == '\x1b':
                self.escape = char
            else:
                self.queue += '\n' if char == '\r' else char

    def guard(self, duration):
        until = time.monotonic() + duration
        while True:
            if self.queue.strip('\n'):
                self.r['early_input'] = True
                self.fail('input arrived before the next INPUT READY prompt')
            self.queue = ''
            left = until - time.monotonic()
            if left <= 0:
                break
            if select.select([0], [], [], min(left, .05))[0]:
                self.incoming()

    def line(self, prompt):
        self.out('\nINPUT READY ' + prompt)
        current = ''
        while True:
            if not self.queue:
                self.incoming()
            char, self.queue = self.queue[0], self.queue[1:]
            if char == '\n':
                if not current:
                    continue
                self.r['inputs'].append(current)
                self.save()
                self.out()
                return current
            if char in ('\b', '\x7f'):
                current = current[:-1]
            elif char == '\x15':
                current = ''
            elif ord(char) >= 32:
                current += char

    def expect(self, prompt, expected):
        got = self.line(prompt)
        if got != expected:
            self.fail('submitted line does not match the live result')

    def hold(self, **values):
        self.r.update(state='holding', **values)
        self.save()

    def done(self, **values):
        self.r.update(state='done', **values)
        self.save()

if __name__ == '__main__':
    if not (sys.stdin.isatty() and sys.stdout.isatty()):
        raise SystemExit('terminal required')
    raise SystemExit('library; launch a named fixture program')
