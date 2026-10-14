"""Terminal input decoded as keys and submitted text, independent of encoding."""
import atexit
import os
import sys
import termios

TTY = {'stdin_tty': os.isatty(0), 'stdout_tty': os.isatty(1)}
if not all(TTY.values()):
    print('FATAL: stdin and stdout must both be terminals', file=sys.stderr)
    raise SystemExit(41)

_original = termios.tcgetattr(0)
_mode = termios.tcgetattr(0)
_mode[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR | termios.IXON)
_mode[3] &= ~(termios.ICANON | termios.ECHO)
_mode[6][termios.VMIN] = 1
_mode[6][termios.VTIME] = 0
termios.tcsetattr(0, termios.TCSANOW, _mode)
atexit.register(termios.tcsetattr, 0, termios.TCSANOW, _original)
_skip_lf = False
_queued = []

def _byte():
    b = os.read(0, 1)
    if not b:
        raise EOFError('terminal input closed')
    return b

def normalize(text):
    return text.replace('\r\n', '\n').replace('\r', '\n')

def meaningful(data):
    """Discard only the LF half of a CRLF already submitted."""
    global _skip_lf
    if _skip_lf and data:
        _skip_lf = False
        if data.startswith(b'\n'):
            data = data[1:]
    return data

def key():
    global _skip_lf
    if _queued:
        return _queued.pop(0)
    b = meaningful(_byte())
    while not b:
        b = _byte()
    if b == b'\r':
        _skip_lf = True
        return '\n'
    if b == b'\n':
        return '\n'
    if b != b'\x1b':
        if b in (b'\x08', b'\x7f'):
            return 'BACKSPACE'
        while True:
            try:
                return b.decode('utf-8')
            except UnicodeDecodeError:
                b += _byte()
    b += _byte()
    if b == b'\x1b[':
        b += _byte()
        if b == b'\x1b[M':
            button, x, y = (_byte()[0] - 32 for _ in range(3))
            return ('mouse', button, x, y, (button & 3) == 3)
        while not 0x40 <= b[-1] <= 0x7e:
            b += _byte()
        if b.startswith(b'\x1b[<') and b[-1:] in (b'M', b'm'):
            button, x, y = map(int, b[3:-1].split(b';'))
            return ('mouse', button, x, y, b.endswith(b'm'))
    elif b == b'\x1bO':
        b += _byte()
    if b == b'\x1b[200~':
        pasted = b''
        while not pasted.endswith(b'\x1b[201~'):
            pasted += _byte()
            if len(pasted) > 1048576:
                raise ValueError('paste too large')
        return ('paste', normalize(pasted[:-6].decode()))
    codes = {
        b'\x1b[A': 'UP', b'\x1bOA': 'UP',
        b'\x1b[B': 'DOWN', b'\x1bOB': 'DOWN',
        b'\x1b[C': 'RIGHT', b'\x1bOC': 'RIGHT',
        b'\x1b[D': 'LEFT', b'\x1bOD': 'LEFT',
        b'\x1b[H': 'HOME', b'\x1bOH': 'HOME', b'\x1b[1~': 'HOME', b'\x1b[7~': 'HOME',
        b'\x1b[F': 'END', b'\x1bOF': 'END', b'\x1b[4~': 'END', b'\x1b[8~': 'END',
        b'\x1b[3~': 'DELETE',
        b'\x1bOP': 'F1', b'\x1b[P': 'F1', b'\x1b[11~': 'F1', b'\x1b[[A': 'F1',
        b'\x1bOQ': 'F2', b'\x1b[Q': 'F2', b'\x1b[12~': 'F2',
        b'\x1bOR': 'F3', b'\x1b[R': 'F3', b'\x1b[13~': 'F3',
        b'\x1bOS': 'F4', b'\x1b[S': 'F4', b'\x1b[14~': 'F4',
        b'\x1bs': 'ALT-S', b'\x1bS': 'ALT-S',
    }
    return codes.get(b, 'UNKNOWN')

def text_key():
    """Flatten paste into text for editors that accept one character at a time."""
    k = key()
    if isinstance(k, tuple) and k[0] == 'paste':
        _queued.extend(k[1])
        return text_key() if _queued else text_key()
    return k

def edit(value, cursor, k):
    if k == 'LEFT': cursor = max(0, cursor - 1)
    elif k == 'RIGHT': cursor = min(len(value), cursor + 1)
    elif k == 'HOME': cursor = 0
    elif k == 'END': cursor = len(value)
    elif k == 'BACKSPACE':
        if cursor: del value[cursor - 1]; cursor -= 1
    elif k == 'DELETE':
        if cursor < len(value): del value[cursor]
    elif k == '\x15': value.clear(); cursor = 0
    elif k == '\x17':
        while cursor and value[cursor - 1].isspace(): del value[cursor - 1]; cursor -= 1
        while cursor and not value[cursor - 1].isspace(): del value[cursor - 1]; cursor -= 1
    elif isinstance(k, str) and len(k) == 1:
        value.insert(cursor, k); cursor += 1
    return cursor

def line():
    value = []; cursor = 0
    while True:
        k = text_key()
        if k == '\n': return ''.join(value)
        if k == '\x04': raise EOFError('input closed at prompt')
        cursor = edit(value, cursor, k)
        if len(value) > 4096: raise ValueError('response too long')
