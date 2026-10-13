import os, sys, json, time, select, signal, hashlib
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _terminal import TTY, key, text_key, edit, line, meaningful

work = Path(sys.argv[1]).resolve()
fx = work / '.fx'
fx.mkdir(parents=True, exist_ok=True)
name = Path(__file__).stem
(fx / (name + '.pid')).write_text(str(os.getpid()) + '\n')

def emit(s):
    print(s, flush=True)

def save(filename, obj):
    if isinstance(obj, dict):
        obj = dict(obj, **TTY)
    target = fx / filename
    temp = fx / (filename + '.tmp')
    temp.write_text(json.dumps(obj, sort_keys=True) + '\n')
    temp.replace(target)

def finish(code=0, **fields):
    save(name + '.json', dict(status=code, **fields))
    sys.exit(code)

def fail(message):
    emit('FATAL: ' + message)
    finish(41, error=message)

def token(label):
    return hashlib.sha256(label.encode()).hexdigest()[:12]

def delay(seconds):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        readable, _, _ = select.select([sys.stdin], [], [], max(0, min(.1, end-time.monotonic())))
        if readable:
            data = os.read(sys.stdin.fileno(), 4096)
            if not data:
                fail('input closed before completion')
            if meaningful(data):
                fail('input arrived before the prompt was ready')

def ask(prompt, expected):
    emit(prompt)
    try:
        actual = line()
    except (EOFError, ValueError) as exc:
        fail(str(exc))
    if actual != expected:
        fail('unexpected response: ' + repr(actual))
    return actual

def state(role):
    p = fx / (role + '-state.json')
    return json.loads(p.read_text()) if p.exists() else {}
def mark(role, phase, **values):
    save(role + '-state.json', dict(phase=phase, **values))
def live(role):
    try:
        os.kill(int((fx / (role + '.pid')).read_text()), 0)
        return state(role).get('phase') == 'ready'
    except (OSError, ValueError):
        return False

artifact, proof = sys.argv[2:4]
if not live('build') or not live('verify') or state('build').get('artifact') != artifact or state('verify').get('proof') != proof:
    fail('matching live build and verifier leases required')
mark('server', 'starting')
emit('Binding local console endpoint...')
delay(3)
if not live('build') or not live('verify'): fail('dependencies vanished during startup')
mark('server', 'ready')
ask('SERVER READY\nrequest:', 'GET /health')
nonce = token('health nonce 36:' + proof)
emit('200 HEALTH nonce=' + nonce)
ask('request:', 'POST /publish ' + nonce + ' ' + proof)
if not live('build') or not live('verify'): fail('dependencies must stay running through publication')
manifest = {'artifact': artifact, 'proof': proof, 'health': nonce, 'profile': 'silver'}
(work / 'manifest.json').write_text(json.dumps(manifest, sort_keys=True) + '\n')
emit('201 PUBLISHED')
ask('request (quit closes server):', 'quit')
mark('server', 'closed')
emit('SERVER CLOSED')
finish(**manifest, published=True, closed=True)
