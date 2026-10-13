import atexit, hashlib, json, os, select, signal, sys, termios, time, tty
from pathlib import Path
WORK = Path(sys.argv[1]).resolve()
FX = WORK / '.fx'
FX.mkdir(parents=True, exist_ok=True)
NAME = Path(__file__).stem
if NAME == 'worker':
    NAME += '_' + sys.argv[2].zfill(2)
elif NAME in ('launch', 'shell_job'):
    NAME += '_' + sys.argv[2]
(FX / (NAME + '.pid')).write_text(str(os.getpid()))
DATA = {'pid': os.getpid(), 'status': 'starting', 'actions': [],
        'started': time.monotonic(), 'stdin_tty': os.isatty(0), 'stdout_tty': os.isatty(1)}

def seeded(label, modulus, base=0):
    seed = (str(WORK) + ':' + label).encode()
    return base + int.from_bytes(hashlib.sha256(seed).digest()[:8], 'big') % modulus

def save(**values):
    DATA.update(values)
    temporary = FX / (NAME + '.tmp')
    temporary.write_text(json.dumps(DATA, sort_keys=True))
    temporary.replace(FX / (NAME + '.json'))

def say(message, end='\n'):
    print(message, end=end, flush=True)

def finish(code=0, **values):
    save(exit_code=code, finished=time.monotonic(), **values)
    raise SystemExit(code)

def fail(reason):
    say('\x1b[31mREJECTED: ' + reason + '\x1b[0m')
    finish(64, status='failed', reason=reason)

save()
if not (DATA['stdin_tty'] and DATA['stdout_tty']):
    fail('stdin and stdout must both be terminals')

# Normalize text ourselves while retaining terminal signals and restoring Bash.
ORIGINAL = termios.tcgetattr(0)
attributes = termios.tcgetattr(0)
attributes[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
attributes[3] &= ~(termios.ICANON | termios.ECHO)
attributes[6][termios.VMIN] = 1
attributes[6][termios.VTIME] = 0
termios.tcsetattr(0, termios.TCSANOW, attributes)
atexit.register(lambda: termios.tcsetattr(0, termios.TCSANOW, ORIGINAL))
SKIP_LF = False
PENDING = []

def gate(seconds):
    # Simulated work duration, never an input deadline.
    global SKIP_LF
    deadline = time.monotonic() + seconds
    if PENDING:
        fail('input arrived before readiness')
    while True:
        left = deadline - time.monotonic()
        ready, _, _ = select.select([sys.stdin], [], [], max(0, min(left, .1)))
        if ready:
            incoming = os.read(0, 4096)
            if SKIP_LF and incoming.startswith(b'\n'):
                incoming = incoming[1:]
            SKIP_LF = False
            if incoming:
                fail('input arrived before readiness')
        if left <= 0:
            break

def read_byte():
    byte = os.read(0, 1)
    if not byte:
        fail('terminal input closed')
    return byte

def read_key():
    first = read_byte()
    if first != b'\x1b':
        return first.decode('latin1')
    second = read_byte()
    if second == b'O':
        return (first + second + read_byte()).decode('latin1')
    if second == b'[':
        sequence = first + second
        while True:
            byte = read_byte()
            sequence += byte
            if 0x40 <= byte[0] <= 0x7e:
                if sequence == b'\x1b[[':
                    return (sequence + read_byte()).decode('latin1')
                if sequence == b'\x1b[M':
                    b, x, y = (read_byte()[0] - 32 for _ in range(3))
                    return ('mouse', b & 3, x, y, (b & 3) != 3)
                if sequence.startswith(b'\x1b[<'):
                    b, x, y = map(int, sequence[3:-1].split(b';'))
                    return ('mouse', b & 3, x, y, sequence[-1:] == b'M')
                return sequence.decode('latin1')
    return (first + second).decode('latin1')

def key():
    global SKIP_LF
    while True:
        sequence = read_key()
        if sequence in ('\x1b[200~', '\x1b[201~'):
            continue
        if SKIP_LF and sequence == '\n':
            SKIP_LF = False
            continue
        SKIP_LF = sequence == '\r'
        break
    aliases = {'\x1b[[A': '\x1bOP', '\x1b[[B': '\x1bOQ',
               '\x1b[[C': '\x1bOR', '\x1b[[D': '\x1bOS',
               '\x1bOA': '\x1b[A', '\x1bOB': '\x1b[B',
               '\x1bOC': '\x1b[C', '\x1bOD': '\x1b[D',
               '\x1b[Q': '\x1bOQ', '\x1b[12~': '\x1bOQ',
               '\x1b[S': '\x1bOS', '\x1b[14~': '\x1bOS',
               '\x1b[P': '\x1bOP', '\x1b[11~': '\x1bOP',
               '\x1b[R': '\x1bOR', '\x1b[13~': '\x1bOR'}
    return aliases.get(sequence, sequence)

def paste_text():
    payload = bytearray()
    while not payload.endswith(b'\x1b[201~'):
        payload.extend(read_byte())
        if len(payload) > 16384:
            fail('paste too long')
    return payload[:-6].decode('utf-8').replace('\r\n', '\n').replace('\r', '\n')

def edited(text):
    line = []
    for char in text:
        if char in ('\x08', '\x7f'):
            if line:
                line.pop()
        elif char == '\x15':
            line.clear()
        else:
            line.append(char)
    return ''.join(line)

def terminal_line():
    global SKIP_LF
    if PENDING:
        return PENDING.pop(0)
    line = ''
    while True:
        pressed = read_key()
        if SKIP_LF and pressed == '\n':
            SKIP_LF = False
            continue
        SKIP_LF = False
        if pressed == '\x1b[200~':
            parts = paste_text().split('\n')
            line += parts[0]
            if len(parts) > 1:
                PENDING.extend(edited(part) for part in parts[1:-1])
                if parts[-1]:
                    fail('paste contains an unterminated extra line')
                return edited(line)
        elif pressed in ('\r', '\n'):
            SKIP_LF = pressed == '\r'
            return edited(line.encode('latin1').decode('utf-8'))
        elif isinstance(pressed, str) and len(pressed) == 1:
            line += pressed
        else:
            fail('unexpected key in submitted text')
        if len(line) > 4096:
            fail('input line too long')

def take(prompt, expected):
    say(prompt, end='')
    value = terminal_line()
    DATA['actions'].append(value)
    save()
    if value != expected:
        fail('unexpected response')
    say('')
    return value

def read_record(name):
    try:
        return json.loads((FX / (name + '.json')).read_text())
    except (OSError, ValueError):
        return {}

def alive(pid):
    try:
        os.kill(int(pid), 0)
        return True
    except (OSError, TypeError, ValueError):
        return False

def raw_terminal(alternate=False):
    tty.setraw(0)
    def restore_display():
        say('\x1b[?2004l' + ('\x1b[?1049l' if alternate else ''), end='')
    atexit.register(restore_display)
    if alternate:
        say('\x1b[?1049h', end='')

INGEST_RC = seeded('ingest-status', 30, 3)

mode = sys.argv[2]
save(mode=mode, cwd=str(Path.cwd()), tag=os.getenv('BATCH_TAG'), terminal=os.isatty(0) and os.isatty(1))
if Path.cwd() != WORK / 'project' or os.getenv('BATCH_TAG') != 'tidal batch' or (not os.isatty(0)):
    fail('interactive project shell context required')
if mode == 'ingest':
    say('Ingest: scanning 18 journal entries...')
    gate(1.3)
    say('\r\x1b[2KIngest 12/18; reconciling journals...', end='')
    gate(4.3)
    say('\n\x1b[33mPARTIAL: 6 entries need repair\x1b[0m')
    finish(INGEST_RC, status='partial', remaining=6)
if mode != 'repair' or len(sys.argv) != 4 or sys.argv[3] != str(INGEST_RC) or (read_record('shell_job_ingest').get('exit_code') != INGEST_RC):
    fail('repair needs the actual ingest exit status')
save(prior_status=int(sys.argv[3]))
say('Repair: validating retained shell status...')
gate(1.5)
take('Repair action > ', f'retry {INGEST_RC}')
gate(2.1)
say('REPAIRED all 18 entries')
finish(0, status='done', repaired=6)
