import os, sys, time, json, hashlib, select, signal, termios, tty, atexit
from pathlib import Path
W = Path(sys.argv[1]).resolve()
FX = W / '.fx'
FX.mkdir(parents=True, exist_ok=True)
NAME = Path(__file__).stem
STATE = {}
TTY = {'stdin_tty': os.isatty(0), 'stdout_tty': os.isatty(1)}
def record(name, value):
    if name == NAME:
        value = dict(value, **TTY)
    p = FX / (name + '.json')
    q = FX / (name + '.tmp')
    q.write_text(json.dumps(value, sort_keys=True) + '\n')
    q.replace(p)
def out(s=''):
    sys.stdout.write(s.replace('\n', '\r\n') + '\r\n')
    sys.stdout.flush()
def fail(reason):
    STATE.update(ok=False, error=reason)
    record(NAME, STATE)
    out('\x1b[31mERROR: ' + reason + '\x1b[0m')
    raise SystemExit(2)
if not all(TTY.values()):
    fail('stdin and stdout must both be terminals')
def raw():
    old = termios.tcgetattr(0)
    tty.setraw(0)
    atexit.register(lambda: termios.tcsetattr(0, termios.TCSANOW, old))
_skip_lf = False
def pause(seconds):
    # A startup delay, not an input deadline. Prompts wait indefinitely.
    global _skip_lf
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if select.select([0], [], [], max(0, end-time.monotonic()))[0]:
            data = os.read(0, 4096)
            if not data:
                fail('input closed')
            if _skip_lf:
                _skip_lf = False
                if data.startswith(b'\n'):
                    data = data[1:]
            if data:
                fail('input arrived before the next prompt')
def byte():
    global _skip_lf
    while True:
        b = os.read(0, 1)
        if not b:
            fail('input closed')
        if _skip_lf:
            _skip_lf = False
            if b == b'\n':
                continue
        _skip_lf = b == b'\r'
        return b
def key():
    b = byte()
    if b == b'\n': return b'\r'
    if b != b'\x1b': return b
    # Only a standalone Escape is ambiguous. Once a sequence has started,
    # read its terminator rather than imposing an inter-byte deadline.
    if not select.select([0], [], [], 0.5)[0]: return b
    tail = byte()
    if tail in (b'[', b'O'):
        while True:
            final = byte()
            tail += final
            if b'@' <= final <= b'~': break
            if len(tail) > 64: fail('oversized key sequence')
    k = b + tail
    if k == b'\x1b[M':
        # Legacy mouse encoding: button, column and row, each offset by 32.
        button, x, y = (byte()[0] - 32 for _ in range(3))
        if (button & 3) == 3: return key()  # Button release is not a new click.
        return ('mouse', button, x, y, False)
    if tail.startswith(b'[<') and tail[-1:] in (b'M', b'm'):
        button, x, y = map(int, tail[2:-1].split(b';'))
        if tail.endswith(b'm'): return key()
        return ('mouse', button, x, y, False)
    aliases = {b'\x1bOA': b'\x1b[A', b'\x1bOB': b'\x1b[B',
               b'\x1bOC': b'\x1b[C', b'\x1bOD': b'\x1b[D',
               b'\x1bOP': b'\x1b[11~', b'\x1b[P': b'\x1b[11~',
               b'\x1bOQ': b'\x1b[12~', b'\x1b[Q': b'\x1b[12~',
               b'\x1bOR': b'\x1b[13~', b'\x1b[R': b'\x1b[13~',
               b'\x1bOS': b'\x1b[14~', b'\x1b[S': b'\x1b[14~'}
    return aliases.get(k, k)
def pasted():
    data = bytearray()
    while not data.endswith(b'\x1b[201~'):
        data.extend(byte())
    return bytes(data[:-6]).replace(b'\r', b'\n')
def line(prompt, secret=False, initial=''):
    sys.stdout.write(prompt + ('' if secret else initial))
    sys.stdout.flush()
    buf = bytearray(initial.encode())
    while True:
        k = key()
        if k == b'\x1b[200~':
            text = pasted()
            # A newline inside a paste submits the same final line as Enter.
            if b'\n' in text:
                head, tail = text.split(b'\n', 1)
                if tail: fail('multiple lines at a single-line prompt')
                buf.extend(head)
                out()
                return buf.decode('utf-8')
            buf.extend(text)
        elif k == b'\r':
            out()
            return buf.decode('utf-8')
        elif k == b'\x15':
            buf.clear()
        elif k in (b'\x7f', b'\x08'):
            # Erase one UTF-8 character, without recording editing gestures.
            if buf:
                text = buf.decode('utf-8', errors='replace')[:-1]
                buf[:] = text.encode()
        elif k == b'\x03':
            fail('unexpected interrupt')
        elif isinstance(k, bytes) and len(k) == 1:
            buf.extend(k)
        else:
            fail('unsupported line input')
        if not secret:
            sys.stdout.write('\r\x1b[2K' + prompt + buf.decode('utf-8', errors='replace'))
            sys.stdout.flush()
def expect(prompt, wanted, secret=False, initial=''):
    got = line(prompt, secret, initial)
    if got != wanted: fail('unexpected response to ' + prompt)
    return got

(FX/(NAME+'.pid')).write_text(str(os.getpid()))
a=json.loads((FX/'review_agent.json').read_text()) if (FX/'review_agent.json').exists() else {}
if a.get('stage')!='needs-evidence' or not a.get('status_inspected'): fail('agent has not requested recovery evidence')
original='case=manifest-review\nalpha=6\nbeta=11\nkeep-source=yes\n'
if (W/'case.txt').read_text()!=original: fail('source was modified')
digest=hashlib.sha256(original.encode()).hexdigest()
raw()
out('offline verifier: checking bundled schema and manifest')
pause(2.7)
evidence='proof-'+hashlib.sha256((digest+':offline-schema-v2').encode()).hexdigest()[:12]
record(NAME,dict(ok=True,evidence=evidence,source_sha256=digest,exit=0))
out('OFFLINE_EVIDENCE='+evidence)
