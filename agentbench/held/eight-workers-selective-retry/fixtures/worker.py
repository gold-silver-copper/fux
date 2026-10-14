import os, sys, time, json, select, signal, atexit, hashlib, termios
from pathlib import Path
ROOT = Path(sys.argv[1]).resolve()
FX = ROOT / '.fx'
FX.mkdir(exist_ok=True)
SEED = 'TA-069-sealed-v1'
def derived(label, data='', length=4):
    return hashlib.sha256((SEED + ':' + label + ':' + data).encode()).hexdigest()[:length].upper()
def boot(name):
    global NAME, REC, OLD_TTY
    NAME = name
    REC = {'state': 'starting', 'error': None, 'pid': os.getpid(), 'exited': False,
           'stdin_tty': sys.stdin.isatty(), 'stdout_tty': sys.stdout.isatty()}
    (FX / (NAME + '.pid')).write_text(str(os.getpid()))
    save()
    atexit.register(lambda: save(exited=True))
    if not REC['stdin_tty'] or not REC['stdout_tty']:
        fail('stdin and stdout must both be terminals')
    OLD_TTY = termios.tcgetattr(sys.stdin.fileno())
    attrs = termios.tcgetattr(sys.stdin.fileno())
    attrs[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
    attrs[3] &= ~(termios.ICANON | termios.ECHO)
    attrs[6][termios.VMIN], attrs[6][termios.VTIME] = 1, 0
    termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, attrs)
    atexit.register(lambda: termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, OLD_TTY))
def save(**fields):
    REC.update(fields)
    target = FX / (NAME + '.json')
    tmp = target.with_suffix('.tmp')
    tmp.write_text(json.dumps(REC, sort_keys=True))
    tmp.replace(target)
def out(s='', end='\n'):
    print(s, end=end, flush=True)
def fail(message):
    save(state='failed', error=message, exit_code=2)
    out('\033[31mERROR: ' + message + '\033[0m')
    raise SystemExit(2)
# Parse complete keys without timing assumptions; normalize equivalent encodings.
LAST_CR = False
def byte():
    b = os.read(sys.stdin.fileno(), 1)
    if not b:
        fail('unexpected end of input')
    return b
def key(skip_lf=True):
    global LAST_CR
    while True:
        b = byte()
        if LAST_CR and b == b'\n':
            LAST_CR = False
            if not skip_lf:
                return b''
            continue
        LAST_CR = b == b'\r'
        break
    if b in (b'\r', b'\n'):
        return b'\n'
    if b != b'\x1b':
        return b
    seq = b + byte()
    if seq[1:] not in (b'[', b'O'):
        return seq
    seq += byte()
    if seq == b'\x1b[M':
        return ('mouse', *(v - 32 for v in b''.join(byte() for _ in range(3))))
    while not (64 <= seq[-1] <= 126):
        seq += byte()
    if seq.startswith(b'\x1b[<'):
        try:
            button, x, y = map(int, seq[3:-1].split(b';'))
            return ('mouse', button if seq[-1:] == b'M' else 3, x, y)
        except ValueError:
            fail('invalid mouse event')
    seq = b'\x1b[' + seq[2:]
    return {b'\x1b[P': b'\x1b[11~', b'\x1b[Q': b'\x1b[12~',
            b'\x1b[R': b'\x1b[13~', b'\x1b[S': b'\x1b[14~',
            b'\x1b[V': b'\x1b[17~'}.get(seq, seq)
PARTIAL = ''
PASTE = False
def edit(k, value):
    global PASTE
    if k == b'':
        pass
    elif k == b'\x1b[200~':
        PASTE = True
    elif k == b'\x1b[201~':
        PASTE = False
    elif k == b'\x15':
        value = ''
    elif k in (b'\x7f', b'\x08'):
        value = value[:-1]
    elif k == b'\n':
        return value, True
    elif isinstance(k, bytes) and len(k) == 1 and (k == b'\t' or k[0] >= 32):
        value += k.decode('utf-8', errors='replace')
    else:
        fail('unexpected key in text field')
    return value, False
def rawline(prefill='', echo=True):
    global PARTIAL
    value, PARTIAL = prefill + PARTIAL, ''
    if echo:
        out(value, end='')
    while True:
        k = key()
        value, submitted = edit(k, value)
        if submitted:
            if echo:
                out()
            return value
        if echo and isinstance(k, bytes) and len(k) == 1 and k[0] >= 32:
            out(k.decode('utf-8', errors='replace'), end='')
def line(prompt):
    # There is no input deadline. Only a final submitted line is recorded.
    out(prompt, end='')
    return rawline()
def pause(seconds, forbid=False):
    global PARTIAL
    time.sleep(seconds)
    if forbid:
        while select.select([sys.stdin], [], [], 0)[0]:
            PARTIAL, submitted = edit(key(skip_lf=False), PARTIAL)
            if submitted:
                fail('input submitted before readiness was announced')
def record(name):
    try:
        return json.loads((FX / (name + '.json')).read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return {}
def wait_records(names, predicate):
    while True:
        rows = [record(n) for n in names]
        if all(predicate(r) for r in rows):
            return rows
        time.sleep(.08)

wid = int(sys.argv[2])
if not 1 <= wid <= 8:
    raise SystemExit('ID must be 1..8')
name = f'worker_{wid}'
# This task intentionally forbids restarts, and its goal states that restriction.
claim = FX / (name + '.claim')
try:
    claim.open('x').close()
except FileExistsError:
    (FX / (name + '.duplicate.json')).write_text(json.dumps({'id': wid, 'pid': os.getpid()}))
    raise SystemExit('worker ID already launched')
boot(name)
save(state='started', id=wid)
out(f'Worker {wid}: waiting for all eight workers')
wait_records([f'worker_{i}' for i in range(1, 9)], lambda r: bool(r.get('pid')) and r.get('exited') is False and r.get('error') is None)
save(all_started=True)
pause(.5 + (wid % 4) * .45, forbid=True)
code = f'W{wid}-{(wid * 173 + 61) % 1000:03d}'
out(f'BEGIN READY id={wid} code={code}')
if line('begin> ') != 'begin ' + code:
    fail('wrong begin code')
save(state='running', begin=code)
out(f'Working id={wid}; provisional value={wid * 10}')
pause(.6 + (wid % 3) * .6, forbid=True)
attempts = 1
if wid % 3 == 2:
    token = f'R{wid}-{(wid * 97 + 37) % 997:03d}'
    out(f'RETRY REQUIRED id={wid} token={token}')
    if line('retry> ') != 'retry ' + token:
        fail('wrong retry token')
    save(retry=token)
    attempts = 2
    pause(.4 + (wid % 4) * .3, forbid=True)
value = wid * wid * 37 + 113
save(state='done', value=value, attempts=attempts, exit_code=0)
out(f'DONE id={wid} value={value} attempts={attempts}')
