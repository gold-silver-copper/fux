import os, sys, json, time, select, signal, termios, tty
from pathlib import Path
from contextlib import contextmanager


SEED = 17
calculation_status = 10 + SEED % 10

def boot(work, name):
    global root, record, state
    root = Path(work).resolve()
    (root / '.fx').mkdir(exist_ok=True)
    record = root / '.fx' / (name + '.json')
    if record.exists():
        prior = json.loads(record.read_text())
        prior['attempts'] = prior.get('attempts', 1) + 1
        record.write_text(json.dumps(prior, sort_keys=True))
        print('ERROR: this program has already been attempted', flush=True)
        sys.exit(91)
    (root / '.fx' / (name + '.pid')).write_text(str(os.getpid()))
    state = dict(ok=True, stdin_tty=sys.stdin.isatty(), stdout_tty=sys.stdout.isatty(),
                 tty=sys.stdin.isatty() and sys.stdout.isatty(), attempts=1,
                 inputs=[], finished=False)
    save()
    if not state['tty']:
        fail('a terminal is required')
    # Configure input before displaying any prompt, so queued CR/LF paste is
    # never transformed by canonical mode during a scheduling delay.
    import atexit
    original = termios.tcgetattr(sys.stdin.fileno())
    settings = termios.tcgetattr(sys.stdin.fileno())
    settings[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
    settings[3] &= ~(termios.ICANON | termios.ECHO)
    settings[6][termios.VMIN], settings[6][termios.VTIME] = 1, 0
    termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, settings)
    atexit.register(termios.tcsetattr, sys.stdin.fileno(), termios.TCSANOW, original)

def save():
    temporary = record.with_suffix('.json.tmp')
    temporary.write_text(json.dumps(state, sort_keys=True))
    temporary.replace(record)

def out(s):
    print(s, flush=True)

def fail(reason, code=90):
    state.update(ok=False, error=reason, finished=True, exit=code)
    save()
    out('\033[31mERROR: ' + reason + '\033[0m')
    sys.exit(code)

def finish(code=0, **values):
    state.update(values)
    state.update(finished=True, exit=code)
    save()
    sys.exit(code)

def pause(seconds):
    global _skip_lf
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if select.select([sys.stdin], [], [], min(.08, max(0, end-time.monotonic())))[0]:
            incoming = byte()
            if _skip_lf and incoming == b'\n':
                _skip_lf = False
                continue
            fail('input arrived before the next prompt')

def line(expected=None):
    # Record only the edited, submitted line, independent of terminal encoding.
    value = ''
    cursor = 0
    pasted = False
    with raw(isig=True):
        while True:
            k = key()
            if k == '\x1b[200~':
                pasted = True
                continue
            if k == '\x1b[201~':
                pasted = False
                if value.endswith(('\r', '\n')):
                    value = value.rstrip('\r\n')
                    break
                continue
            if k in ('\r', '\n') and not pasted:
                break
            if k in ('\x7f', '\x08'):
                if cursor:
                    value = value[:cursor-1] + value[cursor:]
                    cursor -= 1
            elif k == '\x15':
                value, cursor = '', 0
            elif k == '\x1b[D':
                cursor = max(0, cursor - 1)
            elif k == '\x1b[C':
                cursor = min(len(value), cursor + 1)
            elif k in ('\x01', '\x1b[H', '\x1b[1~'):
                cursor = 0
            elif k in ('\x05', '\x1b[F', '\x1b[4~'):
                cursor = len(value)
            elif k == '\x1b[3~':
                value = value[:cursor] + value[cursor+1:]
            elif isinstance(k, str) and len(k) == 1:
                value = value[:cursor] + k + value[cursor:]
                cursor += 1
    state['inputs'].append(value)
    save()
    if expected is not None and value != expected:
        fail('unexpected input: ' + repr(value))
    return value

@contextmanager
def raw(isig=False):
    old = termios.tcgetattr(sys.stdin.fileno())
    tty.setraw(sys.stdin.fileno(), when=termios.TCSANOW)
    if isig:
        settings = termios.tcgetattr(sys.stdin.fileno())
        settings[3] |= termios.ISIG
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, settings)
    try:
        yield
    finally:
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, old)

def byte():
    b = os.read(sys.stdin.fileno(), 1)
    if not b:
        fail('terminal input closed')
    return b

_skip_lf = False
def key():
    global _skip_lf
    b = byte()
    if _skip_lf and b == b'\n':
        b = byte()
    _skip_lf = b == b'\r'
    if b != b'\x1b':
        return b.decode('ascii', 'replace')
    # Escape alone is a control; once a sequence starts, wait for all of it.
    if not select.select([sys.stdin], [], [], .15)[0]:
        return '\x1b'
    b += byte()
    if b[-1:] in (b'[', b'O'):
        while True:
            c = byte()
            b += c
            if 64 <= c[0] <= 126:
                break
    if b == b'\x1b[M':
        button, x, y = (byte()[0] - 32 for _ in range(3))
        return ('mouse', button, x, y, button & 3 != 3)
    text = b.decode('ascii', 'replace')
    if text.startswith('\x1b[<') and text[-1:] in ('M', 'm'):
        button, x, y = map(int, text[3:-1].split(';'))
        return ('mouse', button, x, y, text.endswith('M'))
    if text.startswith('\x1bO'):
        text = '\x1b[' + text[2:]
    return {'\x1b[P': '\x1b[11~', '\x1b[Q': '\x1b[12~',
            '\x1b[R': '\x1b[13~', '\x1b[S': '\x1b[14~'}.get(text, text)

stage = sys.argv[2]
boot(sys.argv[1], 'batch_' + stage)
state['shell_pid'] = os.getppid()
save()
def previous(which):
    p = root / '.fx' / ('batch_' + which + '.json')
    if not p.exists(): fail('missing previous command')
    return json.loads(p.read_text())
if stage == 'begin':
    out('Batch CLI initialized. Configure your shell for morning shift in queue stage.')
    finish(0, initialized=True)
if stage not in ('calculate', 'finalize'):
    fail('unknown command')
begin = previous('begin')
state.update(cwd=str(Path.cwd().relative_to(root)), label=os.environ.get('BATCH_LABEL'), same_shell=begin['shell_pid'] == os.getppid())
save()
if not state['same_shell'] or state['cwd'] != 'queue stage' or state['label'] != 'morning shift':
    fail('shell environment or working directory did not persist')
if stage == 'calculate':
    out('morning shift: reading queue...')
    out('user@batch:queue stage$   (sample prompt from imported log)')
    pause(4.7)
    values = [int(s) for s in (root / 'queue stage' / 'input.txt').read_text().split()]
    total = sum(values)
    out('CALCULATED SUM ' + str(total))
    print('\033[33mA quarantined row requires attention.\033[0m', file=sys.stderr, flush=True)
    finish(calculation_status, total=total)
calc = previous('calculate')
reported = int(sys.argv[3])
if calc.get('exit') != calculation_status or not calc.get('finished') or reported != calc['exit'] or calc['shell_pid'] != os.getppid():
    fail('the actual calculation status was not preserved')
receipt = f'SH-{calc["total"]}-{reported}'
out('FINAL RECEIPT ' + receipt)
finish(0, reported=reported, total=calc['total'], receipt=receipt)
