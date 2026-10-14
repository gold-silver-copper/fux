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

artifact = sys.argv[2]
if not live('build') or state('build').get('artifact') != artifact:
    fail('live ready build artifact required')
mark('verify', 'verifying')
emit('Running integration tests for ' + artifact)
delay(5)
if not live('build'): fail('build lease vanished')
proof = token('test proof 36:' + artifact)
mark('verify', 'ready', artifact=artifact, proof=proof)
emit('PROOF ' + proof)
ask('Verifier lease held. release closes it:', 'release')
if state('server').get('phase') != 'closed': fail('server must close before verifier')
mark('verify', 'closed', proof=proof)
emit('VERIFIER CLOSED')
finish(artifact=artifact, proof=proof, released=True)
