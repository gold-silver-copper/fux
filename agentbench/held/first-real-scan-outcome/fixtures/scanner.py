#!/usr/bin/env python3
import os, sys, json, time, signal, select, hashlib, termios, tty
from pathlib import Path
work = Path(sys.argv[1]).resolve()
fx = work / '.fx'
fx.mkdir(parents=True, exist_ok=True)
name = 'scanner'
state = {'events': [], 'exit': None}
(fx / (name + '.pid')).write_text(str(os.getpid()) + '\n')
def save():
    target = fx / (name + '.json')
    temp = fx / (name + '.json.tmp')
    temp.write_text(json.dumps(state, sort_keys=True) + '\n')
    temp.replace(target)
def event(value):
    state['events'].append(value)
    save()
def out(value, end='\n'):
    print(value, end=end, flush=True)
def finish(code=0):
    state['exit'] = code
    save()
    raise SystemExit(code)
def fail(reason):
    state['error'] = reason
    out('\033[31mERROR: ' + reason + '\033[0m')
    finish(64)
# All submitted text is decoded here; records contain semantic input only.
import atexit
from collections import deque
_pending = deque()
_skip_lf = False

def readbyte():
    global _skip_lf
    while True:
        if _pending:
            data = _pending.popleft()
        else:
            data = os.read(sys.stdin.fileno(), 1)
        if not data:
            fail('unexpected EOF')
        if _skip_lf:
            _skip_lf = False
            if data == b'\n':
                continue
        return data

def ready_guard():
    global _skip_lf
    # A CRLF submission may leave its LF behind. It is not an early reply.
    if _skip_lf and select.select([sys.stdin], [], [], 0)[0]:
        data = os.read(sys.stdin.fileno(), 1)
        _skip_lf = False
        if data != b'\n':
            _pending.append(data)
    if _pending or select.select([sys.stdin], [], [], 0)[0]:
        fail('input arrived before the prompt')

def key():
    global _skip_lf
    data = readbyte()
    if data in (b'\r', b'\n'):
        _skip_lf = data == b'\r'
        return '\r'
    if data != b'\x1b':
        return data.decode('utf-8', errors='surrogateescape')
    # This is Escape/sequence disambiguation, not an input deadline.
    if not _pending and not select.select([sys.stdin], [], [], 1.0)[0]:
        return 'ESC'
    second = readbyte()
    if second not in (b'[', b'O'):
        return 'ALT-' + second.decode('utf-8', errors='surrogateescape')
    tail = b''
    while True:
        c = readbyte()
        tail += c
        if 64 <= c[0] <= 126:
            break
    text = tail.decode('ascii')
    if second == b'[' and text == 'M':
        button, x, y = (readbyte()[0] - 32 for _ in range(3))
        return ('MOUSE', button & 3, x, y, (button & 3) != 3)
    if text.startswith('<') and text[-1] in 'Mm':
        button, x, y = map(int, text[1:-1].split(';'))
        return ('MOUSE', button & 3, x, y, text[-1] == 'M')
    names = {'A': 'UP', 'B': 'DOWN', 'C': 'RIGHT', 'D': 'LEFT',
             'H': 'HOME', 'F': 'END', 'P': 'F1', 'Q': 'F2',
             'R': 'F3', 'S': 'F4', '200~': 'PASTE', '201~': 'PASTE_END',
             '1~': 'HOME', '4~': 'END', '7~': 'HOME', '8~': 'END',
             '11~': 'F1', '12~': 'F2', '13~': 'F3', '14~': 'F4',
             '15~': 'F5', '17~': 'F6', '18~': 'F7', '19~': 'F8',
             '20~': 'F9', '21~': 'F10', '23~': 'F11', '24~': 'F12'}
    return names.get(text, 'CSI-' + text)

def pasted_text():
    data = bytearray()
    end = b'\x1b[201~'
    while not data.endswith(end):
        data.extend(readbyte())
        if len(data) > 65536:
            fail('paste too long')
    return bytes(data[:-len(end)]).decode('utf-8').replace('\r\n', '\n').replace('\r', '\n')

def edited(text):
    result = []
    for c in text:
        if c in ('\x08', '\x7f'):
            if result: result.pop()
        elif c == '\x15':
            result.clear()
        else:
            result.append(c)
    return ''.join(result)

def line(prompt, expected=None, hidden=False):
    ready_guard()
    out(prompt, end='')
    chars = []
    while True:
        k = key()
        if k == 'PASTE':
            text = pasted_text()
            if '\n' in text:
                if text.rstrip('\n').count('\n'):
                    fail('one submitted line is required')
                chars.extend(text.rstrip('\n'))
                break
            chars.extend(text)
        elif k == '\r':
            break
        elif k in ('\x08', '\x7f'):
            if chars: chars.pop()
        elif k == '\x15':
            chars.clear()
        elif isinstance(k, str) and len(k) == 1:
            chars.append(k)
        else:
            fail('unexpected key in text input')
    value = edited(''.join(chars))
    out('' if hidden else value)
    if not hidden:
        event('input:' + value)
    if expected is not None and value != expected:
        fail('unexpected response')
    return value

save()
state.update(stdin_tty=sys.stdin.isatty(), stdout_tty=sys.stdout.isatty())
save()
if not state['stdin_tty'] or not state['stdout_tty']:
    fail('stdin and stdout must both be terminals')
state.update(stdin_terminal=os.ttyname(sys.stdin.fileno()), stdout_terminal=os.ttyname(sys.stdout.fileno()))
save()
_terminal_original = termios.tcgetattr(sys.stdin.fileno())
def restore_terminal():
    termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, _terminal_original)
atexit.register(restore_terminal)
# Keep ISIG, so terminal Ctrl-C still sends SIGINT; decode CR/LF ourselves.
_input_mode = termios.tcgetattr(sys.stdin.fileno())
_input_mode[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
_input_mode[3] &= ~(termios.ICANON | termios.ECHO)
_input_mode[6][termios.VMIN] = 1
_input_mode[6][termios.VTIME] = 0
termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, _input_mode)
seed=402
ticket=f'F-{seed}'
generation=seed%11
digest=hashlib.sha256(f'{seed}:recovered'.encode()).hexdigest()[:4]
state.update(ticket=ticket,generation=generation,digest=digest)
out('Example: OUTCOME READY ticket=example')
out('Cached result: OUTCOME READY ticket=R-111')
for step,delay in [('loading old index',2.4),('checking current index',3.2),('checking journal',2)]:
    time.sleep(delay)
    out('\r\033[2K\033[36m'+step+'\033[0m',end='')
out('')
ready_guard()
event('outcome:FAILED:'+ticket)
out('OUTCOME FAILED ticket='+ticket)
line('decision> ', 'recover '+ticket)
for i in range(3):
    out('Recovery replay batch '+str(i+1))
    time.sleep(1.4)
ready_guard()
event('idle:'+str(generation))
out('RECOVERY IDLE generation='+str(generation)+'; no further output until continuation')
line('continuation> ', 'continue '+str(generation))
time.sleep(1.3)
event('complete:'+digest)
out('SCAN COMPLETE digest='+digest)
finish()
