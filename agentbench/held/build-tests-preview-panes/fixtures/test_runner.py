NAME = 'test_runner'

import atexit
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import sys
import time

work = Path(sys.argv[1]).resolve()
fx = work / '.fx'
fx.mkdir(parents=True, exist_ok=True)
(fx / (NAME + '.pid')).write_text(str(os.getpid()))
with (fx / (NAME + '.starts')).open('a') as f:
    f.write('start\n')

def digest(s, n=12):
    return hashlib.sha256(s.encode()).hexdigest()[:n]

def save(name, value):
    (fx / name).write_text(json.dumps(value, sort_keys=True))

def emit(s='', end='\n'):
    print(s, end=end, flush=True)

# Read terminal input semantically, without inter-byte deadlines.
import termios

class TerminalInput:
    def __init__(self):
        self.pending = bytearray()
        self.skip_lf = False

    def byte(self):
        while True:
            if self.pending:
                b = bytes(self.pending[:1])
                del self.pending[:1]
            else:
                b = os.read(0, 1)
            require(bool(b), 'input closed')
            if self.skip_lf:
                self.skip_lf = False
                if b == b'\n':
                    continue
            return b

    def no_early(self):
        # CRLF is one Enter, even if its LF arrives after the CR.
        if self.skip_lf and select.select([0], [], [], 0)[0]:
            b = os.read(0, 1)
            self.skip_lf = False
            if b != b'\n':
                self.pending.extend(b)
        require(not self.pending and not select.select([0], [], [], 0)[0],
                'input arrived before the prompt')

    def key(self):
        b = self.byte()
        if b != b'\x1b':
            if b == b'\r':
                self.skip_lf = True
            return b
        prefix = self.byte()
        require(prefix in (b'[', b'O'), 'unsupported escape key')
        sequence = bytearray()
        while True:
            part = self.byte()
            sequence.extend(part)
            if 0x40 <= part[0] <= 0x7e:
                break
        suffix = bytes(sequence)
        if suffix in (b'A', b'B', b'C', b'D'):
            return b'\x1b[' + suffix  # CSI and SS3 arrows have the same meaning.
        if suffix in (b'P', b'Q', b'R', b'S'):
            return b'\x1bO' + suffix
        if suffix == b'12~':
            return b'\x1bOQ'
        if prefix == b'[' and suffix == b'M':
            # Consume the complete legacy mouse report, never its coordinates as text.
            return b'\x1b[M' + self.byte() + self.byte() + self.byte()
        return b'\x1b' + prefix + suffix

    def paste(self):
        data = bytearray()
        end = b'\x1b[201~'
        while not data.endswith(end):
            data.extend(self.byte())
            require(len(data) < 65536, 'paste too long')
        return bytes(data[:-len(end)]).replace(b'\r\n', b'\n').replace(b'\r', b'\n')

    def line(self):
        value = bytearray()
        pos = 0
        while True:
            k = self.key()
            if k == b'\x1b[200~':
                self.pending[:0] = self.paste()
            elif k in (b'\r', b'\n'):
                self.skip_lf = k == b'\r'
                return value.decode()
            elif k in (b'\x7f', b'\x08'):
                if pos:
                    del value[pos - 1]
                    pos -= 1
            elif k == b'\x15':
                del value[:pos]
                pos = 0
            elif k == b'\x01':
                pos = 0
            elif k == b'\x05':
                pos = len(value)
            elif k == b'\x1b[D':
                pos = max(0, pos - 1)
            elif k == b'\x1b[C':
                pos = min(len(value), pos + 1)
            elif len(k) == 1 and k[0] >= 32:
                value[pos:pos] = k
                pos += 1
            else:
                raise RuntimeError('unexpected key at line prompt')

_input = TerminalInput()

def no_early():
    _input.no_early()

def raw_open():
    old = termios.tcgetattr(0)
    attrs = termios.tcgetattr(0)
    attrs[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
    attrs[3] &= ~(termios.ICANON | termios.ECHO)
    attrs[6][termios.VMIN] = 1
    attrs[6][termios.VTIME] = 0
    termios.tcsetattr(0, termios.TCSANOW, attrs)
    return old

def raw_close(old):
    termios.tcsetattr(0, termios.TCSANOW, old)

def key():
    return _input.key()

def raw_line(prompt):
    no_early()
    emit(prompt)
    return _input.line()

def ask(s):
    old = raw_open()
    try:
        return raw_line(s)
    finally:
        raw_close(old)

def require(ok, message):
    if not ok:
        raise RuntimeError(message)

def alive(name):
    try:
        os.kill(int((fx / (name + '.pid')).read_text()), 0)
        return True
    except (OSError, ValueError):
        return False

def main():
    emit('Tests: allocating sandbox')
    time.sleep(2)
    token = ask('BUILD TOKEN>')
    build = json.loads((fx / 'build.json').read_text())
    require(token == build['token'], 'build token not ready or incorrect')
    for phase in ('unit', 'integration', 'contract'):
        time.sleep(2.4)
        emit('PASS ' + phase)
    signature = 'T-' + digest(token + '|27-passed')
    save('tests.json', {'build': token, 'signature': signature, 'passed': 27})
    emit('TESTS PASSED signature=' + signature)

try:
    require(os.isatty(0) and os.isatty(1), 'interactive terminal required')
    save(NAME + '.terminal.json', {'stdin': os.ttyname(0), 'stdout': os.ttyname(1)})
    main()
except SystemExit as e:
    save(NAME + '.exit.json', {'code': e.code or 0})
    raise
except BaseException as e:
    save(NAME + '.error.json', {'error': str(e)})
    save(NAME + '.exit.json', {'code': 91})
    emit('FIXTURE ERROR: ' + str(e))
    raise SystemExit(91)
else:
    save(NAME + '.exit.json', {'code': 0})
