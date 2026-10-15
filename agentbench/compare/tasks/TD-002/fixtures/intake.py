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

run = Run(sys.argv[1], 'intake')
old = termios.tcgetattr(0)
mode = termios.tcgetattr(0)
mode[3] |= termios.ICANON | termios.ECHO | termios.ECHOE | termios.ECHOK
mode[3] &= ~termios.ECHONL
mode[0] |= termios.ICRNL
mode[0] &= ~(termios.INLCR | termios.IGNCR)
mode[6][termios.VERASE] = b'\x7f'
mode[6][termios.VKILL] = b'\x15'
termios.tcsetattr(0, termios.TCSANOW, mode)
run.data['canonical_echo'] = True
run.save()
provisional = 'intake ' + code('intake-old') + ' receipt=' + code('intake-fake-receipt')
corrected = 'intake ' + code('intake-id') + ' receipt=' + code('intake-correction-tag')
def normalize(data):
    # Canonical erase/kill processing has already happened in the terminal.
    data = data.rstrip('\r\n')
    data = data.replace('\x1b[200~', '').replace('\x1b[201~', '')
    # Some terminals encode Backspace as Ctrl-H instead of the configured DEL.
    edited = Line()
    for ch in data:
        edited.edit('BACKSPACE' if ch == '\x08' else ch)
    return edited.text
def line():
    data = normalize(os.read(0, 4096).decode())
    run.receive(data)
    return data
try:
    out('Intake console: canonical echo is ON. Backspace erases; Ctrl-U kills the line.\r\n')
    out('DRAFT INPUT: type exactly the following WITHOUT Enter; a correction will arrive.\r\n')
    out(provisional + '\r\n')
    out('draft> ')
    until = time.monotonic() + 4.2
    while time.monotonic() < until:
        if select.select([0], [], [], min(.1, max(0, until-time.monotonic())))[0]:
            line()
            run.fail('A line was submitted before CORRECTION READY.')
    out('\r\nCORRECTION READY: erase the entire provisional line; submit exactly:\r\n' + corrected + '\r\ncorrected> ')
    if line() != corrected:
        run.fail('The final submitted intake line was not the correction.')
    out('Received ' + corrected + ' (request echo only)\r\n')
    until = time.monotonic() + 2.4
    out('CHECKING: wait for CONFIRM READY; no input yet.\r\n')
    separator_consumed = False
    while time.monotonic() < until:
        if select.select([0], [], [], min(.1, max(0, until-time.monotonic())))[0]:
            data = normalize(os.read(0, 4096).decode())
            if not data and not separator_consumed:
                # A CRLF paste can leave one extra canonical newline.
                separator_consumed = True
            else:
                run.receive(data)
                run.fail('A line was submitted during CHECKING.')
    # Check for unsubmitted typeahead as well, by temporarily disabling ICANON.
    pending_mode = termios.tcgetattr(0)
    probe_mode = termios.tcgetattr(0)
    probe_mode[3] &= ~(termios.ICANON | termios.ECHO)
    termios.tcsetattr(0, termios.TCSANOW, probe_mode)
    pending = b''
    while select.select([0], [], [], 0)[0]:
        pending += os.read(0, 4096)
    termios.tcsetattr(0, termios.TCSANOW, pending_mode)
    pending = pending.replace(b'\x1b[200~', b'').replace(b'\x1b[201~', b'').replace(b'\r', b'').replace(b'\n', b'')
    if pending:
        run.fail('Unsubmitted input was typed during CHECKING.')
    confirm = 'confirm ' + code('intake-confirm')
    out('CONFIRM READY: submit exactly ' + confirm + '\r\nconfirm> ')
    if line() != confirm:
        run.fail('Incorrect confirmation.')
    out('confirm> ' + confirm + ' -- echo, receipt is still pending\r\n')
    out('\rreceipt=' + code('intake-preview') + '\x1b[2K\r')
    run.done(intake=code('intake-id'), receipt=code('intake-issued'))
    out('INTAKE ACCEPTED id=' + code('intake-id') + '\r\n')
    out('ISSUED RECEIPT value=' + code('intake-issued') + '\r\n')
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
