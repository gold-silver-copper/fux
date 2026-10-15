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

import threading
p = Terminal('migration')
p.say('Migration console. Wait for BEGIN, then enter load.')
p.delay(3.1)
p.output('BEGIN> ')
p.expect('load')
p.output('PASTE READY: submit recipe.txt verbatim, including its blank line and final dot line.\r\n')
lines = []
for i in range(5):
    s = p.line(); lines.append(s); p.say('You said: ' + s)
expected = ['route north', 'message You said: COMMIT', '', 'limit 017', '.']
if lines != expected: p.fail('recipe differs from the displayed recipe.txt contract')
def notice():
    p.say('\aNOTICE: period received; validation has started')
t = threading.Thread(target=notice); t.start(); t.join()
p.say('Validation will show a challenge, then a separate FINALIZE prompt. Wait for that prompt.')
p.say('At FINALIZE enter finalize <challenge>.')
p.delay(4.9)
p.output('CHALLENGE=' + p.token('challenge') + '\r\x1b[2KCHALLENGE=(filed)\r\n')
p.delay(3.4)
p.output('FINALIZE> ')
q = p.expect('finalize ' + p.token('challenge'))
p.output(q + ' | APPLIED checksum=' + p.token('checksum') + ' | ' + q + '\r\x1b[2KAPPLIED (receipt filed)\r\n')
p.record('success', {'recipe': lines, 'finalize': q, 'checksum': p.token('checksum')})
p.say('MIGRATION CLOSED')
