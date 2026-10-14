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
name = 'calibrate-' + sys.argv[2]
(fx / (name + '.pid')).write_text(str(os.getpid()))
events = []
seed = hashlib.sha256(('TA-021:' + str(work)).encode()).hexdigest()
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


probe_status = 20 + int(seed[:4], 16) % 60
seal = 'AMBER-96x28-' + seed[4:16].upper()
mode = sys.argv[2]
out('Booting calibration display...')
delay(1)
size = os.get_terminal_size(0)
state['configuration'] = {'cwd': str(Path.cwd()), 'profile': os.environ.get('CAL_PROFILE'), 'cols': size.columns, 'rows': size.lines}
save()
if Path.cwd() != work / 'station' or os.environ.get('CAL_PROFILE') != 'amber' or (size.columns, size.lines) != (96, 28):
    fail('station, profile or geometry does not match')
out('\x1b[36mAmber calibration console / loading instrument map\x1b[0m')
delay(4)
if mode == 'probe':
    event('startup_crash', status=probe_status)
    state['done'] = True
    state['exit'] = probe_status
    save()
    try:
        raise RuntimeError('cached instrument map is incompatible; recovery requires status %d' % probe_status)
    except RuntimeError:
        import traceback
        traceback.print_exc()
        sys.exit(probe_status)
if mode != 'recover':
    fail('unknown mode')
prior = json.loads((fx / 'calibrate-probe.json').read_text())
if not prior['done'] or prior['exit'] != probe_status:
    fail('probe has not completed')
line('Recovery status [recover CODE]> ', 'recover %d' % probe_status)
out('Rebuilding map; do not confirm yet.')
delay(5)
out('Calibration seal: ' + seal)
line('Confirm seal> ', seal)
event('calibrated')
out('Calibration accepted; exit 0')
finish()
