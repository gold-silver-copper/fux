#!/usr/bin/env python3
import atexit
import hashlib
import json
import os
from pathlib import Path
import re
import select
import signal
import sys
import termios
import time
import tty

work = Path(sys.argv[1]).resolve()
fx = work / '.fx'
fx.mkdir(parents=True, exist_ok=True)
name = 'planner-agent'
(fx / (name + '.pid')).write_text(str(os.getpid()))
events = []
seed = hashlib.sha256(('TA-030:' + str(work)).encode()).hexdigest()
state = {'events': events, 'done': False, 'error': None, 'seed': seed,
         'terminal': {'stdin': os.isatty(0), 'stdout': os.isatty(1)}}
def save():
    temp = fx / (name + '.json.tmp')
    temp.write_text(json.dumps(state, sort_keys=True))
    temp.replace(fx / (name + '.json'))
def event(kind, **data):
    events.append({'kind': kind, **data})
    save()
def out(s='', end='\n'):
    print(s, end=end, flush=True)
def fail(message):
    state['error'] = message
    save()
    out('PROTOCOL FAILURE: ' + message)
    sys.exit(93)
def finish(code=0):
    state['done'] = True
    state['exit'] = code
    save()
    sys.exit(code)
if not all(state['terminal'].values()):
    fail('stdin and stdout must both be terminals')
old_tty = termios.tcgetattr(0)
tty.setcbreak(0)
input_settings = termios.tcgetattr(0)
input_settings[0] &= ~(termios.ICRNL | termios.INLCR)
input_settings[3] &= ~termios.ECHO
termios.tcsetattr(0, termios.TCSANOW, input_settings)
atexit.register(lambda: termios.tcsetattr(0, termios.TCSANOW, old_tty))
event('started')

# Decode terminal inputs into their meaning before recording them.
# Prompt reads wait indefinitely; delays model preparation, not input deadlines.
after_cr = False
in_paste = False
def byte(timeout=None):
    if not select.select([0], [], [], timeout)[0]:
        return None
    value = os.read(0, 1)
    if not value:
        fail('terminal closed')
    return value

def read_token(timeout=None):
    global after_cr, in_paste
    while True:
        c = byte(timeout)
        if c is None:
            return None
        if after_cr and c == b'\n':
            after_cr = False
            continue
        after_cr = c == b'\r'
        if c in (b'\r', b'\n'):
            return 'ENTER'
        if c == b'\t':
            return 'TAB'
        if c == b'\x15':
            return 'CLEAR'
        if c in (b'\x08', b'\x7f'):
            return 'BACKSPACE'
        if c == b'\x1b':
            second = byte()
            if second not in (b'[', b'O'):
                return 'ALT-' + second.decode('utf-8').lower()
            seq = second
            if second == b'[':
                first = byte()
                seq += first
                if first == b'M':
                    button, x, y = [byte()[0] - 32 for _ in range(3)]
                    action = 'release' if button & 3 == 3 else 'press'
                    if button & 3 not in (0, 3) or button & 64:
                        fail('expected left mouse click')
                    return 'mouse:%s:%d:%d' % (action, x, y)
                while not 0x40 <= seq[-1] <= 0x7e:
                    seq += byte()
            else:
                seq += byte()
            if seq == b'[200~':
                in_paste = True
                continue
            if seq == b'[201~':
                in_paste = False
                continue
            mouse = re.fullmatch(rb'\[<(\d+);(\d+);(\d+)([Mm])', seq)
            if mouse:
                button, x, y = map(int, mouse.group(1, 2, 3))
                if button & 3 != 0 or button & 64:
                    fail('expected left mouse click')
                return 'mouse:%s:%d:%d' % ('press' if mouse[4] == b'M' else 'release', x, y)
            names = {b'A': 'UP', b'B': 'DOWN', b'C': 'RIGHT', b'D': 'LEFT',
                     b'P': 'F1', b'Q': 'F2', b'R': 'F3', b'S': 'F4'}
            if seq[1:] in names:
                return names[seq[1:]]
            if seq in (b'[11~', b'[12~', b'[13~', b'[14~'):
                return 'F' + str(int(seq[1:-1])-10)
            fail('unsupported terminal key ' + repr(seq))
        # UTF-8 characters are kept intact when editing or submitting text.
        if c[0] >= 0xc0:
            count = 1 if c[0] < 0xe0 else 2 if c[0] < 0xf0 else 3
            c += b''.join(byte() for _ in range(count))
        try:
            return c.decode('utf-8')
        except UnicodeDecodeError:
            fail('invalid UTF-8')

def delay(seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if read_token(max(0, deadline - time.monotonic())) is not None:
            fail('input arrived before the next live prompt')

def field(stop='ENTER', initial=''):
    chars = list(initial)
    while True:
        value = read_token()
        if value == stop:
            return ''.join(chars)
        if value == 'CLEAR':
            chars.clear()
        elif value == 'BACKSPACE':
            if chars:
                chars.pop()
        elif len(value) == 1:
            chars.append(value)
        else:
            fail('unexpected key in text field: ' + value)

def line(prompt, expected=None, initial=''):
    out(prompt, end='')
    value = field(initial=initial)
    out(end='\r\n')
    if expected is not None and value != expected:
        fail('incorrect submitted response')
    event('input', prompt=prompt, value=value)
    return value

def keys(prompt, expected):
    out(prompt)
    values = [read_token() for _ in expected]
    if values != expected:
        fail('incorrect key actions: ' + repr(values))
    event('keys', prompt=prompt, value=values)


original = (work / 'manifest.txt').read_text()
version = int(original.splitlines()[0].split('=',1)[1])
if original != 'manifest-version=%d\nstate=needs-fix\n' % version:
    fail('invalid source manifest')
result = 'manifest-%d:fixed' % version
if (work / 'manifest.txt').read_text() != original:
    fail('initial manifest differs')
out('\x1b[36mLocal planning assistant / preparing a workspace-only proposal\x1b[0m')
delay(4)
out('PLAN: inspect manifest.txt, rebuild local index, keep source manifest.')
line('Plan approval> ', 'approve workspace')
out('Permission request: read manifest.txt only.')
line('Read permission> ', 'allow manifest.txt')
event('manifest_read', version=version)
out('Alternative proposal: remove source manifest after rebuilding.')
line('Delete permission> ', 'deny delete')
out('Rebuilding without deleting source...')
delay(5)
out('\x1b[33mBLOCKED: parallel index lock held; awaiting operator diagnosis.\x1b[0m')
line('Blocked action> ', 'inspect lock')
out('LOCK REPORT: stale parallel lease; use serial recovery, do not force deletion.')
line('Recovery choice> ', 'retry serial')
out('Serial recovery running; proposed result is not yet accepted.')
delay(6)
digest = hashlib.sha256(result.encode()).hexdigest()[:12]
event('result', result=result, digest=digest)
out('RESULT REVIEW ' + result + ' digest=' + digest)
line('Accept result> ', 'accept ' + digest)
if (work / 'manifest.txt').read_text() != original:
    fail('source manifest changed')
event('accepted', result=result)
out('ASSISTANT COMPLETE / exit 0')
finish()
