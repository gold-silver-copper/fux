import contextlib
import json
import os
from pathlib import Path
import select
import sys
import termios
import time
import tty
import hashlib

def token(text, n=12):
    return hashlib.sha256(text.encode()).hexdigest()[:n]

@contextlib.contextmanager
def raw():
    if not (sys.stdin.isatty() and sys.stdout.isatty()):
        raise RuntimeError("a real terminal is required")
    old = termios.tcgetattr(0)
    tty.setraw(0, termios.TCSANOW)
    try:
        yield
    finally:
        termios.tcsetattr(0, termios.TCSADRAIN, old)

class App:
    def __init__(self, work, name):
        self.work = Path(work).resolve()
        self.fx = self.work / '.fx'
        self.fx.mkdir(parents=True, exist_ok=True)
        self.name = name
        self.data = {'pid': os.getpid(), 'name': name, 'status': 'starting',
                     'stdin_tty': sys.stdin.isatty(), 'stdout_tty': sys.stdout.isatty()}
        (self.fx / (name + '.pid')).write_text(str(os.getpid()) + '\n')
        self.save()
        self.event('start')
        if not (self.data['stdin_tty'] and self.data['stdout_tty']):
            self.save(status='failed', exit_code=2, error='stdin and stdout must be terminals')
            self.event('failed', error='stdin and stdout must be terminals')
            raise SystemExit('stdin and stdout must be terminals')

    def save(self, **values):
        self.data.update(values)
        dest = self.fx / (self.name + '.json')
        tmp = self.fx / (self.name + '.tmp')
        tmp.write_text(json.dumps(self.data, sort_keys=True) + '\n')
        tmp.replace(dest)

    def event(self, kind, **values):
        payload = json.dumps(dict(program=self.name, event=kind, **values)) + '\n'
        fd = os.open(self.fx / 'events.jsonl', os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        try:
            os.write(fd, payload.encode())
        finally:
            os.close(fd)

    def peer(self, name):
        return json.loads((self.fx / (name + '.json')).read_text())

    def delay(self, seconds):
        # During startup ANY queued input is an error, even without a newline.
        end = time.monotonic() + seconds
        with raw():
            while True:
                left = end - time.monotonic()
                if left <= 0:
                    break
                if select.select([0], [], [], min(left, .1))[0]:
                    data = os.read(0, 4096)
                    global _after_cr
                    if _after_cr:
                        _after_cr = False
                        if data.startswith(b'\n'):
                            data = data[1:]
                            if not data:
                                continue
                    if data:
                        raise RuntimeError('input arrived before the next prompt was ready')
                    raise RuntimeError('terminal input closed')

    def line(self, prompt):
        with raw():
            display(prompt)
            return read_line(echo=True)

    def expect(self, prompt, value):
        received = self.line(prompt)
        self.event('input', received=received)
        if received != value:
            raise RuntimeError('incorrect response: ' + repr(received))
        return received

    def done(self, code=0, **values):
        self.save(status='done', exit_code=code, **values)
        self.event('done', exit_code=code)

    def run(self, function):
        try:
            code = function() or 0
        except Exception as exc:
            self.save(status='failed', error=str(exc), exit_code=2)
            self.event('failed', error=str(exc))
            print('\nERROR: ' + str(exc), flush=True)
            code = 2
        raise SystemExit(code)

def alive(record):
    try:
        os.kill(record['pid'], 0)
        return True
    except (ProcessLookupError, KeyError):
        return False

def byte():
    data = os.read(0, 1)
    if not data:
        raise RuntimeError('terminal closed')
    return data

def until(end):
    data = byte()
    while not data.endswith(end):
        if len(data) > 65536:
            raise RuntimeError('input too long')
        data += byte()
    return data[:-len(end)]

KEYS = {b'\x1b[A': 'UP', b'\x1b[B': 'DOWN', b'\x1b[C': 'RIGHT',
        b'\x1b[D': 'LEFT', b'\x1bOA': 'UP', b'\x1bOB': 'DOWN',
        b'\x1bOC': 'RIGHT', b'\x1bOD': 'LEFT',
        b'\x1bOQ': 'F2', b'\x1bOS': 'F4', b'\x1b[Q': 'F2', b'\x1b[S': 'F4',
        b'\x1b[12~': 'F2', b'\x1b[14~': 'F4', b'\x1b[[B': 'F2', b'\x1b[[D': 'F4', b'\x1bu': 'ALT-U',
        b'\x1b[200~': 'PASTE_START', b'\x1b[201~': 'PASTE_END'}
_after_cr = False

def key():
    global _after_cr
    data = byte()
    if _after_cr:
        _after_cr = False
        if data == b'\n':
            data = byte()
    if data == b'\r':
        _after_cr = True
    if data == b'\x1b':
        while True:
            if data in KEYS:
                return KEYS[data]
            if data == b'\x1b[M':
                button, x, y = (byte()[0] - 32 for _ in range(3))
                return ('MOUSE', button & 3, x, y, (button & 3) != 3)
            if data.startswith(b'\x1b[<'):
                while data[-1:] not in (b'M', b'm'):
                    data += byte()
                    if len(data) > 64:
                        raise RuntimeError('invalid mouse report')
                button, x, y = map(int, data[3:-1].split(b';'))
                return ('MOUSE', button & 3, x, y, data[-1:] == b'M')
            if not (any(k.startswith(data) for k in KEYS) or
                    b'\x1b[M'.startswith(data) or b'\x1b[<'.startswith(data)):
                raise RuntimeError('unsupported escape sequence ' + repr(data))
            data += byte()
    controls = {b'\r': 'ENTER', b'\n': 'ENTER', b'\t': 'TAB', b'\x15': 'CTRL-U',
                b'\x18': 'CTRL-X', b'\x7f': 'BACKSPACE', b'\x08': 'BACKSPACE'}
    if data in controls:
        return controls[data]
    # UTF-8 text is a character, independent of the number of bytes used to type it.
    length = 1 if data[0] < 128 else (2 if data[0] < 224 else (3 if data[0] < 240 else 4))
    return (data + b''.join(byte() for _ in range(length - 1))).decode('utf-8')

def input_key():
    while True:
        value = key()
        if value not in ('PASTE_START', 'PASTE_END'):
            return value

def read_line(echo=False):
    text = ''
    pasted = False
    submitted = False
    while True:
        value = key()
        if value == 'PASTE_START':
            pasted = True
        elif value == 'PASTE_END':
            pasted = False
            if submitted:
                return text
        elif value == 'ENTER':
            if echo:
                display('\n')
            if not pasted:
                return text
            submitted = True
        elif value == 'BACKSPACE':
            text = text[:-1]
            if echo:
                display('\b \b')
        elif value == 'CTRL-U':
            if echo:
                display('\b \b' * len(text))
            text = ''
        elif isinstance(value, str) and len(value) == 1 and not submitted:
            text += value
            if echo:
                display(value)
        else:
            raise RuntimeError('unexpected line input')

def read_manifest(expected):
    # The logical submitted document, rather than paste wrappers or editing keys,
    # determines acceptance. Without wrappers its final newline terminates input.
    text = ''
    bracketed = False
    first = True
    while True:
        value = key()
        if first and value == 'PASTE_START':
            bracketed = True
            first = False
            continue
        first = False
        if value == 'PASTE_END' and bracketed:
            return text, True
        if value == 'ENTER':
            text += '\n'
        elif value == 'TAB':
            text += '\t'
        elif value == 'BACKSPACE':
            if text and not text.endswith('\n'):
                text = text[:-1]
        elif value == 'CTRL-U':
            text = text[:text.rfind('\n') + 1]
        elif isinstance(value, str) and len(value) == 1:
            text += value
        else:
            raise RuntimeError('unexpected manifest input')
        if len(text) > 65536:
            raise RuntimeError('manifest too long')
        if not bracketed and text.count('\n') == expected.count('\n'):
            return text, False

def display(text):
    sys.stdout.write(text.replace('\n', '\r\n'))
    sys.stdout.flush()
