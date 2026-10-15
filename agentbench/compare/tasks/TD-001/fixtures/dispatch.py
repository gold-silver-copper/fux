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

run = Run(sys.argv[1], 'dispatch')
job, challenge, final = code('dispatch-job'), code('dispatch-challenge'), code('dispatch-final')
with raw():
    out('\x1b[36mDispatch editor 4.2\x1b[0m\r\n')
    quiet_work(run, 1.7, 'Loading dispatch ledger; wait for READY DISPATCH EDITOR before typing.')
    out('Editing: Backspace Ctrl-U Ctrl-W Left Right Home End Delete; Enter submits. Ctrl-C cancels.\r\n')
    out('Replace the draft with exactly: dispatch ' + job + '\r\n')
    out('READY DISPATCH EDITOR\r\n')
    got = read_line(run, 'dispatch> ', 'dispatch ' + code('stale-job') + ' seal=' + code('draft-seal'))
    if got != 'dispatch ' + job:
        run.fail('The dispatch draft was submitted without the requested repair.')
    out('dispatch> ' + got + ' -- echoed request, not a server result\r\n')
    quiet_work(run, 3.6, 'VALIDATING dispatch; do not type until READY SEAL')
    out('\rseal=' + code('preview-seal') + ' [preview]\x1b[2K\r')
    out('READY SEAL: submit exactly seal ' + challenge + '\r\n')
    got = read_line(run, 'seal> ')
    if got != 'seal ' + challenge:
        run.fail('Incorrect seal command.')
    out('seal> ' + got + ' -- request echo\r\n')
    run.done(dispatch=job, seal=final)
    out('ACCEPTED DISPATCH id=' + job + '\r\n')
    out('SERVER SEALED value=' + final + '\r\n')
