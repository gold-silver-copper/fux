import contextlib
import hashlib
import json
import os
from pathlib import Path
import select
import sys
import termios
import time
import tty

def code(label):
    return hashlib.sha256(('echo-lab-v3:' + label).encode()).hexdigest()[:9].upper()

def out(text):
    os.write(1, text.encode())

class Run:
    def __init__(self, work, name):
        self.root = Path(work) / '.fx'
        self.root.mkdir(parents=True, exist_ok=True)
        self.name = name
        self.data = dict(tty_in=os.isatty(0), tty_out=os.isatty(1), complete=False, received=[])
        self.save()
        (self.root / (name + '.pid')).write_text(str(os.getpid()) + '\n')
        if not (self.data['tty_in'] and self.data['tty_out']):
            self.fail('A terminal is required for stdin and stdout.')
    def save(self):
        dst = self.root / (self.name + '.json')
        tmp = self.root / (self.name + '.' + str(os.getpid()) + '.tmp')
        tmp.write_text(json.dumps(self.data))
        tmp.replace(dst)
    def fail(self, reason):
        self.data['error'] = reason
        self.save()
        out('\r\nFAILED: ' + reason + '\r\n')
        raise SystemExit(2)
    def receive(self, value):
        self.data['received'].append(value)
        self.save()
    def done(self, **values):
        self.data.update(values)
        self.data['complete'] = True
        self.save()

@contextlib.contextmanager
def raw():
    old = termios.tcgetattr(0)
    tty.setraw(0)
    try:
        yield
    finally:
        termios.tcsetattr(0, termios.TCSANOW, old)

_after_cr = False
def key(timeout=None):
    global _after_cr
    if not select.select([0], [], [], timeout)[0]:
        return None
    b = os.read(0, 1)
    if not b:
        raise SystemExit(3)
    if b == b'\n' and _after_cr:
        _after_cr = False
        return key(timeout)
    _after_cr = b == b'\r'
    if b in (b'\r', b'\n'):
        return 'ENTER'
    if b == b'\x1b':
        seq = b''
        while select.select([0], [], [], 0.08)[0]:
            seq += os.read(0, 1)
            if len(seq) > 1 and (65 <= seq[-1] <= 90 or 97 <= seq[-1] <= 122 or seq[-1] == 126):
                break
        mapping = {b'[A':'UP', b'OA':'UP', b'[B':'DOWN', b'OB':'DOWN', b'[C':'RIGHT', b'OC':'RIGHT', b'[D':'LEFT', b'OD':'LEFT', b'[H':'HOME', b'OH':'HOME', b'[1~':'HOME', b'[7~':'HOME', b'[F':'END', b'OF':'END', b'[4~':'END', b'[8~':'END', b'[3~':'DELETE', b'OQ':'F2', b'[12~':'F2', b'[[B':'F2', b'[200~':'PASTE', b'[201~':'PASTE'}
        return mapping.get(seq, 'IGNORE')
    return {b'\x7f':'BACKSPACE', b'\x08':'BACKSPACE', b'\x15':'CLEAR', b'\x17':'WORD', b'\x01':'HOME', b'\x05':'END', b'\t':'TAB', b'\x03':'CANCEL'}.get(b, b.decode('ascii', 'ignore') or 'IGNORE')

class Line:
    def __init__(self, text=''):
        self.text = text
        self.pos = len(text)
    def edit(self, k):
        if k == 'CLEAR':
            self.text, self.pos = '', 0
        elif k == 'WORD':
            left = self.text[:self.pos].rstrip()
            cut = left.rfind(' ') + 1
            self.text, self.pos = self.text[:cut] + self.text[self.pos:], cut
        elif k == 'BACKSPACE' and self.pos:
            self.text = self.text[:self.pos-1] + self.text[self.pos:]
            self.pos -= 1
        elif k == 'DELETE':
            self.text = self.text[:self.pos] + self.text[self.pos+1:]
        elif k == 'LEFT':
            self.pos = max(0, self.pos-1)
        elif k == 'RIGHT':
            self.pos = min(len(self.text), self.pos+1)
        elif k == 'HOME':
            self.pos = 0
        elif k == 'END':
            self.pos = len(self.text)
        elif len(k) == 1 and ' ' <= k <= '~':
            self.text = self.text[:self.pos] + k + self.text[self.pos:]
            self.pos += 1
    def draw(self, prompt):
        out('\r\x1b[2K' + prompt + self.text + '\x1b[K')
        back = len(self.text) - self.pos
        if back:
            out('\x1b[' + str(back) + 'D')

def read_line(run, prompt, initial=''):
    line = Line(initial)
    line.draw(prompt)
    while True:
        k = key()
        if k == 'CANCEL':
            run.fail('Cancelled with Ctrl-C.')
        if k == 'ENTER':
            out('\r\n')
            run.receive(line.text)
            return line.text
        line.edit(k)
        line.draw(prompt)

def quiet_work(run, seconds, message):
    out(message + '\r\n')
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        k = key(min(0.1, max(0, until-time.monotonic())))
        if k and k not in ('PASTE', 'IGNORE'):
            run.fail('Input arrived before the displayed READY event.')

import sys

run = Run(sys.argv[1], 'build')
if len(sys.argv) != 3 or 'i' not in sys.argv[2]:
    run.fail('Run from an interactive shell and pass "$-".')
run.data['interactive'] = True
run.save()
draft = 'note ' + code('build-note') + ' status=' + str(int(code('build-decoy')[:2], 16))
status = 17 + int(code('build-exit')[:2], 16) % 73
with raw():
    out('\x1b[33mCompiling deployment bundle\x1b[0m\r\n')
    out('BUSY DRAFT: type exactly this WITHOUT Enter: ' + draft + '\r\n')
    out('Type the draft while busy. The build waits for a complete draft, then prepares the note prompt.\r\n')
    line = Line()
    line.draw('buffered> ')
    while line.text != draft:
        k = key()
        if k in ('ENTER', 'CANCEL'):
            run.fail('The draft was submitted before NOTE READY.')
        line.edit(k)
        line.draw('buffered> ')
    out('\r\nCompiling; draft retained. Wait for NOTE READY before Enter.\r\n')
    until = time.monotonic() + 12.1
    while time.monotonic() < until:
        k = key(min(.1, max(0, until-time.monotonic())))
        if k in ('ENTER', 'CANCEL'):
            run.fail('Enter arrived while compilation was busy.')
        if k:
            line.edit(k)
    out('Queued keys replay (not a result): ' + line.text + '\r\n')
    out('NOTE READY: review buffered draft, then press Enter. Editing keys remain available.\r\n')
    line.draw('note> ')
    while True:
        k = key()
        if k == 'CANCEL':
            run.fail('Cancelled.')
        if k == 'ENTER':
            run.receive(line.text)
            if line.text != draft:
                run.fail('Incorrect submitted build note.')
            break
        line.edit(k)
        line.draw('note> ')
    out('\r\nNOTE ACKNOWLEDGED id=' + code('build-note') + '\r\n')
    quiet_work(run, 3.1, 'PUBLISHING: do not type; wait for process exit.')
    out('\rartifact=' + code('build-preview') + ' status=0\x1b[2K\r')
    out('PUBLISHED ARTIFACT id=' + code('build-artifact') + '\r\n')
    out('Build finished; obtain the exit status from the shell.\r\n')
    run.done(artifact=code('build-artifact'), note=code('build-note'), exit_status=status, finished_ns=time.monotonic_ns())
raise SystemExit(status)
