import os, sys, time, json, hashlib, termios, tty, atexit, select
from pathlib import Path

class Terminal:
    def __init__(self, name):
        self.work = Path(sys.argv[1]); self.fx = self.work / '.fx'; self.fx.mkdir(exist_ok=True)
        self.name = name; self.skip_lf = False; self.escape = b''
        self.base = {'terminal': bool(sys.stdin.isatty() and sys.stdout.isatty()), 'pid': os.getpid()}
        self.put(name + '.pid', str(os.getpid()))
        if self.base['terminal']:
            self.base['tty'] = os.ttyname(0)
        self.record('started', {})
        if not self.base['terminal']:
            raise SystemExit('A terminal on stdin and stdout is required')
        self.saved = termios.tcgetattr(0); tty.setraw(0, termios.TCSANOW)
        atexit.register(termios.tcsetattr, 0, termios.TCSANOW, self.saved)
        self.seed = (self.work / 'seed.txt').read_text().strip()
    def put(self, name, text):
        p = self.fx / name; t = self.fx / (name + '.' + str(os.getpid()) + '.tmp')
        t.write_text(text); os.replace(t, p)
    def record(self, kind, data):
        self.put(self.name + '.' + kind + '.json', json.dumps(dict(self.base, **data), sort_keys=True))
    def token(self, tag):
        return hashlib.sha256((self.seed + '|' + tag).encode()).hexdigest()[:12].upper()
    def raw(self, text):
        os.write(1, text.encode())
    def say(self, text=''):
        self.raw(text.replace('\n', '\r\n') + '\r\n')
    def fail(self, reason):
        self.record('failure', {'reason': reason}); self.say('\nFAILED: ' + reason); raise SystemExit(2)
    def key(self, timeout=None):
        end = None if timeout is None else time.monotonic() + timeout
        while True:
            left = None if end is None else max(0, end - time.monotonic())
            if not select.select([0], [], [], left)[0]: return None
            b = os.read(0, 1)
            if not b: self.fail('terminal input closed')
            if self.escape:
                self.escape += b
                # Both CSI and SS3 keys, including bracketed paste delimiters, are ignored.
                if len(self.escape) == 2 and b not in (b'[', b'O'):
                    self.escape = b''
                elif len(self.escape) >= 3 and (64 <= b[0] <= 126):
                    self.escape = b''
                continue
            if b == b'\x1b': self.escape = b; continue
            if self.skip_lf:
                self.skip_lf = False
                if b == b'\n': continue
            if b == b'\r': self.skip_lf = True; return '\n'
            if b == b'\n': return '\n'
            if b == b'\x03': self.fail('interrupted')
            return b.decode('ascii', 'replace')
    def delay(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            c = self.key(max(0, end - time.monotonic()))
            if c is not None: self.fail('input arrived before the next input prompt')
    def output(self, text):
        if self.key(0) is not None: self.fail('input arrived before the next input prompt')
        self.raw(text)
    def line(self, echo=True):
        chars = []
        while True:
            c = self.key()
            if c == '\n':
                if echo: self.raw('\r\n')
                return ''.join(chars)
            if c in ('\x08', '\x7f'):
                if chars:
                    chars.pop()
                    if echo: self.raw('\b \b')
            elif c == '\x15':
                if echo: self.raw('\b \b' * len(chars))
                chars = []
            elif c >= ' ':
                chars.append(c)
                if echo: self.raw(c)
    def expect(self, expected, echo=True):
        got = self.line(echo)
        if got != expected: self.fail('submitted input did not match the displayed instruction')
        return got
    def state(self, name):
        try: return json.loads((self.fx / (name + '.state.json')).read_text())
        except (OSError, ValueError): return {}

import os
p = Terminal('compile')
p.say('Compiler opening. At COMPILER INPUT, enter build READY only after both PROBE READY and SERVER READY.')
p.delay(2.1)
p.output('COMPILER INPUT> ')
q = p.expect('build READY')
probe, serve = p.state('probe'), p.state('serve')
for s in (probe, serve):
    if not s.get('terminal') or s.get('phase') != 'ready': p.fail('both services must already be ready')
    try: os.kill(s['pid'], 0)
    except (KeyError, OSError): p.fail('a service is no longer running')
if len({p.base['tty'], probe['tty'], serve['tty']}) != 3: p.fail('three simultaneous separate terminals required')
p.say('You said: ' + q)
p.say('Compiling; no input is accepted.')
p.delay(5.2)
a = p.token('artifact')
p.output('\aBUILD artifact=' + a + '\r\x1b[2KBUILD artifact=(filed)\r\n')
p.record('success', {'command': q, 'artifact': a, 'probe_pid': probe['pid'], 'serve_pid': serve['pid'], 'ttys': [p.base['tty'], probe['tty'], serve['tty']]})
p.say('COMPILER CLOSED')
