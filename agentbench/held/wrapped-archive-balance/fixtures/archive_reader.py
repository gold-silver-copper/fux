import os, sys, time, json, select, signal, termios, tty, traceback, atexit
from pathlib import Path
from contextlib import contextmanager

RAW = False
WORK = Path(sys.argv[1]).resolve()
if not (sys.stdin.isatty() and sys.stdout.isatty()):
    raise SystemExit('stdin and stdout must both be terminals')
FX = WORK / '.fx'
FX.mkdir(parents=True, exist_ok=True)
STATE = {}

def seed_token(label, prefix='', length=4):
    # The resolved work path is a per-validation seed shared by related programs.
    import hashlib
    return prefix + hashlib.sha256((str(WORK) + ':' + label).encode()).hexdigest()[:length]

def init(name):
    previous = {}
    try:
        previous = json.loads((FX / (name + '.json')).read_text())
    except (OSError, ValueError):
        pass
    STATE.update(name=name, pid=os.getpid(), started=time.monotonic(),
                 terminal=dict(stdin=sys.stdin.isatty(), stdout=sys.stdout.isatty()),
                 history=[], complete=False, exit=None, runs=previous.get('runs', 0) + 1)
    (FX / (name + '.pid')).write_text(str(os.getpid()) + '\n')
    save()
    # Observe individual premature keystrokes, not just complete early lines.
    # Keep echo and signal generation for ordinary prompts and Ctrl-C.
    old = termios.tcgetattr(sys.stdin.fileno())
    settings = old[:]
    settings[6] = old[6][:]
    settings[3] &= ~termios.ICANON
    settings[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
    settings[6][termios.VMIN] = 1
    settings[6][termios.VTIME] = 0
    termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, settings)
    def restore():
        try:
            termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, old)
        except termios.error:
            pass
    atexit.register(restore)

def save(**values):
    # A SIGINT handler can itself save a receipt. Defer it until this atomic
    # update is finished so the handler cannot steal the temporary file.
    old_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT})
    try:
        STATE.update(values)
        target = FX / (STATE['name'] + '.json')
        tmp = target.with_suffix('.tmp')
        tmp.write_text(json.dumps(STATE, sort_keys=True))
        tmp.replace(target)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)

def say(message='', end=None):
    print(message, end=('\r\n' if RAW else '\n') if end is None else end, flush=True)

def fail(reason, code=91):
    save(failure=reason, exit=code)
    say('\x1b[31mREJECTED: ' + reason + '\x1b[0m')
    raise SystemExit(code)

def delay(seconds, phase):
    global _after_cr
    # Reject queued input: a guessed response sent during this phase cannot pass.
    save(phase=phase)
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        ready, _, _ = select.select([sys.stdin], [], [], min(.1, max(0, deadline-time.monotonic())))
        if ready:
            data = os.read(sys.stdin.fileno(), 4096)
            if _after_cr and data.startswith(b'\n'):
                data = data[1:]
                _after_cr = False
            if not data:
                continue
            save(early_input=data.hex())
            fail('input before ' + phase + ' finished')

def answer(prompt, expected):
    say(prompt, end='')
    value = raw_line()
    STATE['history'].append(value)
    save()
    if value != expected:
        fail('unexpected response: ' + repr(value))
    return value

def finish(code=0, **values):
    save(complete=True, exit=code, finished=time.monotonic(), **values)
    say('\x1b[32mPROCESS COMPLETE exit=' + str(code) + '\x1b[0m')
    raise SystemExit(code)

@contextmanager
def raw_terminal():
    global RAW
    old = termios.tcgetattr(sys.stdin.fileno())
    tty.setraw(sys.stdin.fileno(), when=termios.TCSANOW)
    RAW = True
    try:
        yield
    finally:
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSADRAIN, old)
        RAW = False

_after_cr = False

def byte():
    global _after_cr
    while True:
        c = os.read(sys.stdin.fileno(), 1)
        if not c:
            fail('terminal input closed')
        paired_lf = _after_cr and c == b'\n'
        _after_cr = c == b'\r'
        if paired_lf:
            continue
        return b'\n' if c == b'\r' else c

def key(lone_escape=False):
    b = byte()
    if b != b'\x1b' or lone_escape:
        return b
    # Help's close prompt explicitly requests a lone Escape. Other prompts
    # read a complete key sequence with no inter-byte arrival deadline.
    b += byte()
    if b[1:] not in (b'[', b'O'):
        return b
    b += byte()
    if b == b'\x1b[M':
        data = bytes(os.read(sys.stdin.fileno(), 1)[0] for _ in range(3))
        button, x, y = (n - 32 for n in data)
        return ('mouse', button, x, y, button & 3 == 3)
    if b == b'\x1b[[':
        b += byte()
        return {b'\x1b[[A': b'\x1bOP', b'\x1b[[B': b'\x1bOQ'}.get(b, b)
    while not 64 <= b[-1] <= 126:
        b += byte()
        if len(b) > 64:
            fail('invalid key sequence')
    if b.startswith(b'\x1b[<') and b[-1:] in (b'M', b'm'):
        button, x, y = map(int, b[3:-1].split(b';'))
        return ('mouse', button, x, y, b[-1:] == b'm')
    aliases = {
        b'\x1bOA': b'\x1b[A', b'\x1bOB': b'\x1b[B',
        b'\x1bOC': b'\x1b[C', b'\x1bOD': b'\x1b[D',
        b'\x1bOH': b'\x1b[H', b'\x1bOF': b'\x1b[F',
        b'\x1b[1~': b'\x1b[H', b'\x1b[7~': b'\x1b[H',
        b'\x1b[4~': b'\x1b[F', b'\x1b[8~': b'\x1b[F',
        b'\x1b[P': b'\x1bOP', b'\x1b[Q': b'\x1bOQ',
        b'\x1b[R': b'\x1bOR', b'\x1b[S': b'\x1bOS',
        b'\x1b[11~': b'\x1bOP', b'\x1b[12~': b'\x1bOQ',
        b'\x1b[13~': b'\x1bOR', b'\x1b[14~': b'\x1bOS',
    }
    return aliases.get(b, b)

def raw_line():
    data, cursor, pasted, submitted = bytearray(), 0, False, False
    while True:
        k = key()
        if k == b'\x1b[200~':
            pasted = True
        elif k == b'\x1b[201~':
            pasted = False
            if submitted:
                say()
                return data.decode('utf-8')
        elif k == b'\n':
            if pasted:
                submitted = True
            else:
                say()
                return data.decode('utf-8')
        elif k in (b'\x7f', b'\x08'):
            if cursor:
                del data[cursor-1]; cursor -= 1
        elif k == b'\x15':
            data.clear(); cursor = 0
        elif k == b'\x1b[H': cursor = 0
        elif k == b'\x1b[F': cursor = len(data)
        elif k == b'\x1b[D': cursor = max(0, cursor-1)
        elif k == b'\x1b[C': cursor = min(len(data), cursor+1)
        elif k == b'\x1b[3~':
            del data[cursor:cursor+1]
        elif isinstance(k, bytes) and len(k) == 1 and k[0] >= 32:
            data[cursor:cursor] = k; cursor += 1
        else:
            fail('unexpected line input')
        if len(data) > 4096:
            fail('response too long')

def dependency(name):
    try:
        return json.loads((FX / (name + '.json')).read_text())
    except (OSError, ValueError):
        return {}

archive = seed_token('archive', 'ARCH-', 4)
checksum = seed_token('checksum', '', 6)
row_count = 1200 + int(seed_token('row-count', '', 4), 16) % 101
balance_final = 400 + int(seed_token('balance_final', '', 4), 16) % 500

init('archive-reader')
size = os.get_terminal_size()
save(columns=size.columns, rows=size.lines)
if (size.columns, size.lines) != (64, 18):
    fail('reader needs a 64 by 18 terminal')
say('\x1b[36mLedger archive: %d rows\x1b[0m' % row_count)
for row in range(1, row_count + 1):
    if row == 147:
        say('\x1b[35mROW 0147 ' + 'segment:' + 'cedar/' * 28 + (' archive=' + archive + ' checksum=' + checksum + ' ENDROW\x1b[0m'))
    else:
        say('ROW %04d segment=%s status=archived amount=%05d' % (row, 'stone/' * 11, (row * 19) % 10000))
    if row % 100 == 0:
        delay(.12, 'archive streaming')
say('Archive complete. Current balance follows; updates replace this line.')
for revision, balance in ((1, 481), (2, 529), (3, balance_final)):
    say('\r\x1b[2KBALANCE revision=%d value=%d' % (revision, balance), end='')
    delay(2, 'balance updates')
say(('\r\x1b[2KBALANCE FINAL value=' + str(balance_final)))
say('footer source=cedar')
say('footer rows=' + str(row_count))
say('footer status=sealed')
answer('export> ', ('export ' + archive + ' ' + checksum + ' ' + str(balance_final)))
finish(0, exported=True)
