
import os, sys, json, time, select, signal, hashlib, termios
from pathlib import Path
work = Path(sys.argv[1]).resolve()
fx = work / '.fx'
fx.mkdir(parents=True, exist_ok=True)
name = 'probe'
record = fx / (name + '.json')
previous = json.loads(record.read_text()) if record.exists() else {}
state = {'name': name, 'pid': os.getpid(), 'starts': previous.get('starts', 0) + 1,
         'phase': 'starting', 'inputs': [], 'started_ns': time.monotonic_ns()}
(fx / (name + '.pid')).write_text(str(os.getpid()) + '\n')
def save(**kw):
    state.update(kw)
    temporary = fx / (name + '.tmp')
    temporary.write_text(json.dumps(state, sort_keys=True))
    temporary.replace(record)
def out(s):
    print(s, flush=True)
def finish(code=0, **kw):
    save(phase='failed' if 'error' in kw else 'done', exit_code=code,
         finished_ns=time.monotonic_ns(), **kw)
    out('PROCESS_EXIT=' + str(code))
    raise SystemExit(code)
def bad(reason):
    out('PROTOCOL_ERROR: ' + reason)
    finish(90, error=reason)
def delay(seconds):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        ready, _, _ = select.select([sys.stdin], [], [], 0 if _keys else min(.1, max(0, end-time.monotonic())))
        if ready or _keys:
            submitted = edit_line(read_key())
            if submitted is not None:
                state['inputs'].append(submitted)
                bad('input arrived before the announced prompt')
def ask(prompt, wanted, secret=False):
    save(phase='prompt')
    out(prompt)
    received = read_line()
    state['inputs'].append(received)
    save()
    if received != wanted:
        bad('unexpected reply')
def token(s):
    return hashlib.sha256(s.encode()).hexdigest()[:12]

# Decode terminal input into semantic keys; records contain only submitted lines.
import atexit
from collections import deque
stdin_tty, stdout_tty = sys.stdin.isatty(), sys.stdout.isatty()
save(stdin_tty=stdin_tty, stdout_tty=stdout_tty)
if not (stdin_tty and stdout_tty):
    bad('stdin and stdout must both be terminals')
_input_fd = sys.stdin.fileno()
_original_terminal = termios.tcgetattr(_input_fd)
atexit.register(lambda: termios.tcsetattr(_input_fd, termios.TCSANOW, _original_terminal))
_input_terminal = termios.tcgetattr(_input_fd)
_input_terminal[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
_input_terminal[3] &= ~(termios.ICANON | termios.ECHO)
_input_terminal[3] |= termios.ISIG
_input_terminal[6][termios.VMIN] = 1
_input_terminal[6][termios.VTIME] = 0
termios.tcsetattr(_input_fd, termios.TCSANOW, _input_terminal)
_keys = deque()
_after_cr = False

def _byte():
    value = os.read(_input_fd, 1)
    if not value:
        bad('terminal closed')
    return value

def read_key():
    global _after_cr
    if _keys:
        key = _keys.popleft()
    else:
        key = _byte()
        if key == b'\x1b':
            # Wait for the full escape sequence, even if delivery is fragmented.
            prefix = _byte()
            if prefix in (b'[', b'O'):
                body = _byte()
                if prefix == b'[':
                    while not 0x40 <= body[-1] <= 0x7e:
                        body += _byte()
                if prefix == b'[' and body == b'200~':
                    pasted = b''
                    while not pasted.endswith(b'\x1b[201~'):
                        pasted += _byte()
                    _keys.extend(bytes([b]) for b in pasted[:-6])
                    return read_key() if _keys else None
                if prefix == b'[' and body == b'M':
                    button, x, y = (_byte()[0] - 32 for _ in range(3))
                    return ('click', x, y) if button & 3 == 0 and not button & 32 else None
                if prefix == b'[' and body.startswith(b'<') and body[-1:] in (b'M', b'm'):
                    try:
                        button, x, y = map(int, body[1:-1].split(b';'))
                    except ValueError:
                        return None
                    return ('click', x, y) if body.endswith(b'M') and button & 3 == 0 and not button & 32 else None
                final = body[-1:]
                if final in (b'A', b'B', b'C', b'D', b'H', b'F'):
                    return {b'A': 'up', b'B': 'down', b'C': 'right', b'D': 'left', b'H': 'home', b'F': 'end'}[final]
                if final in (b'P', b'Q', b'R', b'S'):
                    return 'f' + str(final[0] - ord('P') + 1)
                if body.endswith(b'~'):
                    return {b'11~': 'f1', b'12~': 'f2', b'13~': 'f3', b'14~': 'f4', b'15~': 'f5', b'17~': 'f6', b'18~': 'f7', b'19~': 'f8', b'20~': 'f9', b'21~': 'f10', b'23~': 'f11', b'24~': 'f12'}.get(body)
                return None
            return None
    if key == b'\n' and _after_cr:
        _after_cr = False
        return None
    _after_cr = key == b'\r'
    if key in (b'\r', b'\n'):
        return 'enter'
    return {b'\x15': 'clear', b'\x7f': 'backspace', b'\x08': 'backspace', b'\t': 'tab', b'\x07': 'cancel'}.get(key, key.decode('latin1'))

_line_text = ''
_line_cursor = 0

def edit_line(key):
    global _line_text, _line_cursor
    if key == 'enter':
        submitted = _line_text
        _line_text, _line_cursor = '', 0
        return submitted
    if key == 'clear':
        _line_text, _line_cursor = '', 0
    elif key == 'backspace' and _line_cursor:
        _line_text = _line_text[:_line_cursor-1] + _line_text[_line_cursor:]
        _line_cursor -= 1
    elif key == 'left':
        _line_cursor = max(0, _line_cursor-1)
    elif key == 'right':
        _line_cursor = min(len(_line_text), _line_cursor+1)
    elif key == 'home':
        _line_cursor = 0
    elif key == 'end':
        _line_cursor = len(_line_text)
    elif isinstance(key, str) and len(key) == 1 and key.isprintable():
        _line_text = _line_text[:_line_cursor] + key + _line_text[_line_cursor:]
        _line_cursor += 1
    return None

def read_line():
    while True:
        submitted = edit_line(read_key())
        if submitted is not None:
            return submitted

save()
if state['starts'] != 1:
    bad('this job must not be restarted')


service = json.loads((fx / 'service.json').read_text())
if service.get('stage') != 'ready' or sys.argv[2] != service.get('ready_token'):
    bad('probe must start after service readiness with its token')
try:
    os.kill(service['pid'], 0)
except ProcessLookupError:
    bad('service is not running')
out('Probe attached to ready service ' + sys.argv[2])
delay(1.6)
ask('CHECK_PROMPT: check <service endpoint>', 'check ' + service['endpoint'])
out('\x1b[32mAssertions 3/3 passed\x1b[0m')
delay(.8)
receipt = token('probe:' + service['ready_token'])
out('PROBE_RECEIPT=' + receipt)
finish(receipt=receipt)
