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

run = Run(sys.argv[1], 'routing')
recipient = Line('day-ops receipt=' + code('routing-draft'))
mode, focus = 0, 0
def draw():
    out('\x1b[H\x1b[2J\x1b[36mROUTING FORM\x1b[0m\r\n')
    out('Tab switches fields. Recipient: Backspace Ctrl-U Ctrl-W Left/Right Home/End Delete.\r\n')
    out('Mode: Up/Down or Left/Right toggles standard/guarded. F2 opens review. Ctrl-C cancels.\r\n')
    out(('> ' if focus == 0 else '  ') + 'Recipient: ' + recipient.text + '\r\n')
    out(('> ' if focus == 1 else '  ') + 'Mode: ' + ('standard', 'guarded')[mode] + '\r\n')
    out('saved=' + recipient.text + ' [LIVE INPUT PREVIEW ONLY]\r\n')
    out('FORM READY\r\n')
    if focus == 0:
        out('\x1b[4;' + str(14 + recipient.pos) + 'H')
with raw():
    out('\x1b[?1049h')
    try:
        draw()
        while True:
            k = key()
            if k == 'CANCEL':
                run.fail('Cancelled.')
            if k == 'F2':
                if recipient.text != 'night-ops' or mode != 1:
                    run.fail('Review opened with incorrect recipient or mode.')
                break
            if k == 'TAB':
                focus = 1 - focus
            elif focus == 0:
                recipient.edit(k)
            elif k in ('UP', 'DOWN', 'LEFT', 'RIGHT'):
                mode = 1 - mode
            draw()
        out('\x1b[H\x1b[2JREVIEW MODAL\r\nRecipient: ' + recipient.text + '\r\nMode: guarded\r\n')
        out('receipt=' + code('routing-modal-echo') + ' [review label, not saved]\r\n')
        out('REVIEW READY: submit exactly prepare\r\n')
        if read_line(run, 'review> ') != 'prepare':
            run.fail('Expected prepare.')
        out('\x1b[H\x1b[2JPREPARING revision=' + code('routing-preview-revision') + '\r\n')
        quiet_work(run, 5.4, 'PREPARING: do not type until COMMIT READY.')
        token = code('routing-commit')
        out('\x1b[H\x1b[2JCOMMIT READY: submit exactly commit ' + token + '\r\n')
        if read_line(run, 'commit receipt=' + code('routing-modal-echo') + '> ') != 'commit ' + token:
            run.fail('Incorrect commit confirmation.')
        out('\x1b[H\x1b[2JROUTING COMMITTED\r\n')
        out('SAVED REVISION value=' + code('routing-revision') + '\r\n')
        out('SAVED RECEIPT value=' + code('routing-receipt') + '\r\n')
        out('EXIT READY: press q to exit.\r\n')
        while True:
            k = key()
            if k == 'q':
                break
            if k not in ('IGNORE', 'PASTE'):
                run.fail('Expected q to exit the committed view.')
        run.done(recipient=recipient.text, mode='guarded', revision=code('routing-revision'), receipt=code('routing-receipt'), clean_exit=True)
    finally:
        out('\x1b[?1049l')
out('Routing editor exited successfully.\r\n')
