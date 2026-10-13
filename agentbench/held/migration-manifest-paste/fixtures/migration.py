
import atexit, hashlib, json, os, select, signal, sys, termios, time, tty
from pathlib import Path

def token(s, n=8):
    return hashlib.sha256(s.encode()).hexdigest()[:n].upper()

class Terminal:
    def __init__(self, work, name):
        self.work = Path(work).resolve()
        self.fx = self.work / '.fx'
        self.fx.mkdir(parents=True, exist_ok=True)
        self.name = name
        self.path = self.fx / (name + '.json')
        # A startup reservation makes rejected attempts persist across restarts.
        try:
            fd = os.open(self.fx / (name + '.started'), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            os.close(fd)
        except FileExistsError:
            pid = os.getpid()
            (self.fx / (name + f'.duplicate-{pid}.pid')).write_text(str(pid) + '\n')
            (self.fx / (name + '.duplicate.json')).write_text(json.dumps({'pid':pid, 'exit_code':90}) + '\n')
            print('ERROR: this fixture instance was already started; restart is invalid', flush=True)
            raise SystemExit(90)
        self.state = {'name': name, 'pid': os.getpid(), 'events': [], 'stage': 'starting', 'stdin_tty': os.isatty(0), 'stdout_tty': os.isatty(1)}
        (self.fx / (name + '.pid')).write_text(str(os.getpid()) + '\n')
        self.save()
        self.old = None
        self.after_cr = False
        if not self.state['stdin_tty'] or not self.state['stdout_tty']:
            self.fail('stdin and stdout must both be terminals')
        self.old = termios.tcgetattr(0)
        tty.setcbreak(0)
        mode = termios.tcgetattr(0)
        mode[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR)
        termios.tcsetattr(0, termios.TCSANOW, mode)
        atexit.register(self.restore)

    def restore(self):
        self.save(exited=True)
        if self.old is not None:
            try: termios.tcsetattr(0, termios.TCSANOW, self.old)
            except OSError: pass

    def save(self, **kw):
        self.state.update(kw)
        tmp = self.path.with_suffix('.tmp')
        tmp.write_text(json.dumps(self.state, sort_keys=True) + '\n')
        tmp.replace(self.path)

    def event(self, event, value):
        self.state['events'].append([event, value])
        self.save()

    def out(self, s='', end='\n'):
        print(s, end=end, flush=True)

    def fail(self, reason):
        self.save(stage='failed', error=reason, exit_code=90)
        self.out('\x1b[31mERROR: ' + reason + '\x1b[0m')
        raise SystemExit(90)

    def _read(self, timeout=None):
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            remaining = None if deadline is None else max(0, deadline-time.monotonic())
            if not select.select([0], [], [], remaining)[0]: return None
            b = os.read(0, 1)
            if not b: self.fail('unexpected end of input')
            if self.after_cr and b == b'\n':
                self.after_cr = False
                continue
            self.after_cr = b == b'\r'
            return b

    def guard(self, seconds):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            b = self._read(min(.1, max(0, until-time.monotonic())))
            if b is not None: self.fail('input before readiness')

    def byte(self, timeout=None):
        b = self._read(timeout)
        if b is None: self.fail('input deadline expired')
        return b

    def paste(self):
        data = bytearray()
        while not data.endswith(b'\x1b[201~'):
            data.extend(self.byte())
            if len(data) > 65536: self.fail('paste too long')
        return bytes(data[:-6]).replace(b'\r', b'\n')

    def edited(self, initial=''):
        # Only the submitted value is recorded, never its editing history.
        data = initial
        while True:
            b = self.byte()
            if b == b'\x1b':
                seq = b + b''.join(self.byte() for _ in range(5))
                if seq != b'\x1b[200~': self.fail('unsupported text input key')
                pasted = self.paste().decode('utf-8')
                submitted = pasted.endswith('\n')
                data += pasted[:-1] if submitted else pasted
                if submitted: return data
            elif b in (b'\r', b'\n'): return data
            elif b in (b'\x7f', b'\b'): data = data[:-1]
            elif b == b'\x15': data = ''
            else:
                # Read one complete UTF-8 character so Backspace removes a character.
                count = 1 if b[0] < 128 else (2 if b[0] < 224 else 3 if b[0] < 240 else 4)
                data += (b + b''.join(self.byte() for _ in range(count-1))).decode('utf-8')

    def line(self, prompt, expected=None, hidden=False):
        before = termios.tcgetattr(0)
        if hidden:
            after = termios.tcgetattr(0)
            after[3] &= ~termios.ECHO
            termios.tcsetattr(0, termios.TCSANOW, after)
        # Set input mode before advertising readiness.
        self.out(prompt, end='')
        try: value = self.edited()
        finally:
            if hidden: termios.tcsetattr(0, termios.TCSANOW, before)
        self.out()
        self.event('input', value)
        if expected is not None and value != expected: self.fail('unexpected response')
        return value

    def finish(self, code=0, **kw):
        self.save(stage='done', exit_code=code, **kw)
        raise SystemExit(code)

def load(path):
    try: return json.loads(Path(path).read_text())
    except (OSError, ValueError): return None

def alive(pid):
    try: os.kill(int(pid), 0); return True
    except (OSError, ValueError, TypeError): return False

fx = Terminal(sys.argv[1], 'migration')
fx.out('Migration console: opening encrypted local workspace...')
fx.guard(4)
password = fx.line('Passphrase (hidden): ', hidden=True)
if password != (fx.work/'credentials.txt').read_text().splitlines()[0]: fx.fail('bad passphrase')
fx.save(password=password, hidden=True)
options = ['Inspect', 'Apply', 'Quit']; pos=0; arrows=0
fx.out('\x1b[?25lSELECT OPERATION: [Inspect] Apply Quit')
while True:
    b=fx.byte()
    if b in (b'\r', b'\n'): break
    if b != b'\x1b': fx.fail('use arrow keys')
    second=fx.byte(); third=fx.byte()
    if second not in (b'[', b'O') or third not in (b'A', b'B', b'C', b'D'): fx.fail('unsupported selector key')
    pos=max(0,min(2,pos+(1 if third in (b'B',b'C') else -1))); arrows+=1
    fx.out('\r\x1b[2KSELECT OPERATION: ' + ' '.join(('['+v+']') if j==pos else v for j,v in enumerate(options)), end='')
fx.out('\x1b[?25h')
if pos != 1 or arrows == 0: fx.fail('Apply must be selected with arrows')
fx.save(operation='Apply', arrows_used=True)
fx.out('\x1b[?2004hPASTE MANIFEST:')
expected=(fx.work/'payload.txt').read_bytes().replace(b'\r\n',b'\n').replace(b'\r',b'\n')
first=fx.byte()
if first==b'\x1b':
    if first+b''.join(fx.byte() for _ in range(5)) != b'\x1b[200~': fx.fail('unsupported paste marker')
    payload=fx.paste()
else:
    data=bytearray(first.replace(b'\r',b'\n'))
    while len(data)<len(expected): data.extend(fx.byte().replace(b'\r',b'\n'))
    payload=bytes(data)
fx.out('\x1b[?2004l')
if payload != expected: fx.fail('manifest content changed')
fx.save(payload=payload.decode())
fx.out('Validating manifest; please wait...')
fx.guard(3)
fx.out('COMMAND EDITOR> apply --unsafe', end='')
command=fx.edited('apply --unsafe')
if command!='apply --safe': fx.fail('safe command required')
fx.save(command=command)
fx.out('\nEOF TO COMMIT (Ctrl-D on empty input):')
value=bytearray()
while True:
    b=fx.byte()
    if b==b'\x04' and not value: break
    if b==b'\x15': value.clear()
    elif b in (b'\x7f',b'\b'):
        if value: value.pop()
    elif b==b'\x04': continue
    else: value.extend(b)
receipt='M-'+token(payload.decode()+'|apply --safe')
fx.out('MIGRATION COMMITTED receipt='+receipt)
fx.finish(0, eof_commit=True, receipt=receipt)
