import sys, os, json, time, select, hashlib, re, tty, termios, atexit
from collections import deque
from pathlib import Path
WORK = Path(sys.argv[1]).resolve()
FX = WORK / '.fx'
FX.mkdir(parents=True, exist_ok=True)
NAME = None
EVENTS = []
_INPUT = deque()
_SKIP_LF = False
def put(key, value):
    path = FX / (key + '.json')
    tmp = path.with_suffix('.tmp')
    tmp.write_text(json.dumps(value, sort_keys=True))
    tmp.replace(path)
def get(key):
    return json.loads((FX / (key + '.json')).read_text())
def begin(name):
    global NAME
    NAME = name
    # A new attempt supersedes its own earlier completion record.
    (FX / (name + '.result.json')).unlink(missing_ok=True)
    (FX / (name + '.pid')).write_text(str(os.getpid()))
    terminals = {'stdin_tty': os.isatty(0), 'stdout_tty': os.isatty(1)}
    put(name + '.meta', dict(terminals, pid=os.getpid(), ppid=os.getppid(),
        tty=os.ttyname(0) if terminals['stdin_tty'] else None,
        started=time.monotonic(), cwd=os.getcwd()))
    if not all(terminals.values()):
        bad('stdin and stdout must both be terminals')
    original = termios.tcgetattr(0)
    atexit.register(termios.tcsetattr, 0, termios.TCSANOW, original)
    settings = termios.tcgetattr(0)
    settings[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
    settings[3] &= ~(termios.ICANON | termios.ECHO)
    settings[6][termios.VMIN] = 1
    settings[6][termios.VTIME] = 0
    termios.tcsetattr(0, termios.TCSANOW, settings)
def emit(s='', end='\n'):
    text = s + end
    if os.isatty(1) and not (termios.tcgetattr(1)[1] & termios.OPOST):
        text = text.replace('\r\n', '\n').replace('\n', '\r\n')
    sys.stdout.write(text)
    sys.stdout.flush()
def finish(code=0, **values):
    put(NAME + '.result', dict(values, code=code, events=EVENTS, finished=time.monotonic()))
    sys.exit(code)
def bad(why):
    emit('\033[31mERROR: ' + why + '\033[0m')
    finish(41, error=why)
def delay(seconds, reject=True):
    global _SKIP_LF
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        ready, _, _ = select.select([0] if reject else [], [], [], min(.1, max(0, until-time.monotonic())))
        if ready and _SKIP_LF:
            # CRLF is one Enter: its trailing LF is not premature input.
            if os.read(0, 1) != b'\n':
                bad('input arrived before the prompt was ready')
            _SKIP_LF = False
            ready = []
        if ready or (reject and _INPUT):
            bad('input arrived before the prompt was ready')
def token(s):
    return hashlib.sha256(s.encode()).hexdigest()[:12]
def byte():
    value = os.read(0, 1)
    if not value:
        bad('input closed')
    return value
def raw_key():
    global _SKIP_LF
    if _INPUT:
        return _INPUT.popleft()
    b = byte()
    if _SKIP_LF and b == b'\n':
        _SKIP_LF = False
        return raw_key()
    _SKIP_LF = b == b'\r'
    if b in (b'\r', b'\n'):
        return '\r'
    if b != b'\x1b':
        length = 1 if b[0] < 128 else 2 if b[0] < 224 else 3 if b[0] < 240 else 4
        return (b + b''.join(byte() for _ in range(length-1))).decode('utf-8')
    sequence = b + byte()
    if sequence == b'\x1bO':
        sequence += byte()
    elif sequence == b'\x1b[':
        while True:
            b = byte(); sequence += b
            if 0x40 <= b[0] <= 0x7e:
                break
        if sequence == b'\x1b[M':
            button, x, y = (byte()[0]-32 for _ in range(3))
            return ('mouse', button & 3, x, y, (button & 3) != 3)
    key = sequence.decode()
    if key == '\x1b[200~':
        data = bytearray()
        while not data.endswith(b'\x1b[201~'):
            data.extend(byte())
        text = data[:-6].decode().replace('\r\n', '\n').replace('\r', '\n')
        _INPUT.extend('\r' if c == '\n' else c for c in text)
        return raw_key()
    if key == '\x1b[201~':
        return raw_key()
    mouse = re.fullmatch(r'\x1b\[<(\d+);(\d+);(\d+)([Mm])', key)
    if mouse:
        button, x, y = map(int, mouse.groups()[:3])
        return ('mouse', button & 3, x, y, mouse[4] == 'M')
    aliases = {'\x1bOH':'\x1b[H', '\x1b[1~':'\x1b[H', '\x1b[7~':'\x1b[H',
               '\x1bOF':'\x1b[F', '\x1b[4~':'\x1b[F', '\x1b[8~':'\x1b[F',
               '\x1bOA':'\x1b[A', '\x1bOB':'\x1b[B', '\x1bOC':'\x1b[C', '\x1bOD':'\x1b[D',
               '\x1b[12~':'\x1bOQ', '\x1b[Q':'\x1bOQ', '\x1bA':'\x1ba'}
    return aliases.get(key, key)
def edited(initial='', stop='\r', on_key=None):
    """Return submitted text; editing operations are absent from records."""
    text = list(initial); cursor = len(text)
    while True:
        key = raw_key()
        if key == stop:
            return ''.join(text)
        if on_key and on_key(key):
            continue
        if key == '\x15':
            text.clear(); cursor = 0
        elif key in ('\x08', '\x7f'):
            if cursor:
                cursor -= 1; del text[cursor]
        elif key == '\x1b[3~':
            if cursor < len(text): del text[cursor]
        elif key == '\x1b[H': cursor = 0
        elif key == '\x1b[F': cursor = len(text)
        elif key == '\x1b[D': cursor = max(0, cursor-1)
        elif key == '\x1b[C': cursor = min(len(text), cursor+1)
        elif isinstance(key, str) and len(key) == 1 and key.isprintable():
            text.insert(cursor, key); cursor += 1
        else:
            bad('unexpected input key')
def line(prompt):
    emit(prompt, end='')
    value = edited()
    emit()
    EVENTS.append(value)
    return value
def exact(prompt, expected):
    value = line(prompt)
    if value != expected:
        bad('unexpected response')
    return value

begin('queue')
emit('\033[36mImport queue: opening archive\033[0m')
for message in ['Loading index', 'Checking local cache', 'Verifying generation']:
    delay(1.7)
    emit(message)
exact('CACHE_MISS\nCommand> ', 'retry')
for n in range(4):
    delay(1.8)
    emit(f'\rFetching shard {n+1}/4', end='')
ticket = token('import-queue:retry:four-shards')
emit('\nDRAINED ticket=' + ticket + '\nWaiting for quiet commit> ', end='')
quiet_start = time.monotonic()
delay(2)
quiet = time.monotonic() - quiet_start
emit('QUIET_READY')
response = line('Commit> ')
if quiet < 2: bad('commit arrived before two quiet seconds')
if response != 'commit ' + ticket: bad('wrong commit ticket')
delay(2.2)
receipt = 'import-' + token('queue:accepted:' + ticket)
emit('RECEIPT ' + receipt)
finish(0, quiet=quiet, receipt=receipt)
