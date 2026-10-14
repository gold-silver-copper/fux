
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

fx=Terminal(sys.argv[1],'console')
size=os.get_terminal_size(0)
if size.columns<78 or size.lines<24: fx.fail('terminal too small')
fx.out('Release console: indexing local targets...')
fx.guard(3)
# Full-screen input is raw so text is rendered only by the application.
tty.setraw(0)
fx.out('\x1b[?1049h\x1b[?25l',end='')
atexit.register(lambda: fx.out('\x1b[?25h\x1b[?1049l',end=''))
envs=['local','staging','production']; regions=['east','west']
values=['local','east','1','draft']; focus=0; modal=None; typed=''; preview=None; committed=False
def fingerprint(): return 'FP-'+token('|'.join(values),12)
def draw():
    lines=['\x1b[H\x1b[2J\x1b[36mRELEASE CONSOLE\x1b[0m', 'Tab: next field  Arrows: choose  Ctrl-U: clear text', 'F2: Preview   F4: Commit', '']
    for i,(label,value) in enumerate(zip(['Environment','Region','Replicas','Note'],values)):
        lines.append(('> ' if focus==i else '  ')+label+': '+value)
    if modal=='preview': lines+=['','+--- PREVIEW ---+','Fingerprint: '+fingerprint(),'Values: '+' / '.join(values),'Enter: close preview']
    if modal=='confirm': lines+=['','+--- COMMIT CONFIRMATION ---+','Type preview fingerprint, then Enter: '+typed]
    if modal=='success': lines+=['','COMMIT SUCCESS fingerprint='+fingerprint(),'Press q to quit cleanly']
    fx.out('\r\n'.join(lines),end='')
pending=bytearray()
def key():
    if pending:
        b=bytes(pending[:1]); del pending[:1]; return b
    b=fx.byte()
    if b!=b'\x1b': return b
    seq=b
    sequences={b'\x1b[A':b'up',b'\x1bOA':b'up',b'\x1b[B':b'down',b'\x1bOB':b'down',b'\x1b[C':b'right',b'\x1bOC':b'right',b'\x1b[D':b'left',b'\x1bOD':b'left',b'\x1bOQ':b'f2',b'\x1b[Q':b'f2',b'\x1b[12~':b'f2',b'\x1bOS':b'f4',b'\x1b[S':b'f4',b'\x1b[14~':b'f4',b'\x1b[200~':b'paste'}
    while seq not in sequences:
        if not any(k.startswith(seq) for k in sequences): fx.fail('unsupported special key')
        seq+=fx.byte()
    k=sequences[seq]
    if k==b'paste':
        pending.extend(fx.paste())
        return key()
    return k

draw()
while True:
    k=key()
    if modal=='success':
        if k==b'q':
            fx.finish(0, values=values, preview=preview, confirmed=typed, fingerprint=fingerprint(), quit_key='q', committed=committed)
    elif modal=='preview':
        if k in (b'\r',b'\n'): modal=None
    elif modal=='confirm':
        if k in (b'\r',b'\n'):
            if preview != {'values': values[:], 'fingerprint':fingerprint()} or typed!=fingerprint(): fx.fail('preview confirmation does not match')
            if values!=['staging','west','4','ready']: fx.fail('unsafe release configuration')
            committed=True; modal='success'; fx.save(stage='committed', values=values[:])
        elif k==b'\x15': typed=''
        elif k in (b'\x7f',b'\b'): typed=typed[:-1]
        elif len(k)==1 and 32<=k[0]<127: typed+=k.decode()
    else:
        if k==b'\t': focus=(focus+1)%4
        elif k in (b'up',b'down',b'left',b'right') and focus<2:
            opts=envs if focus==0 else regions
            change=1 if k in (b'down',b'right') else -1
            values[focus]=opts[(opts.index(values[focus])+change)%len(opts)]
        elif k==b'\x15' and focus>=2: values[focus]=''
        elif k in (b'\x7f',b'\b') and focus>=2: values[focus]=values[focus][:-1]
        elif k==b'f2':
            preview={'values':values[:],'fingerprint':fingerprint()}; modal='preview'; fx.state['events'].append(['preview',preview])
        elif k==b'f4': modal='confirm'; typed=''
        elif len(k)==1 and 32<=k[0]<127 and focus>=2: values[focus]+=k.decode()
    draw()
