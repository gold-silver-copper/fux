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

mark('build', 'building')
ask('Build profile? (enter build silver)', 'build silver')
emit('\x1b[36mCompiling silver target...\x1b[0m')
delay(6)
artifact = token('silver build 36')
mark('build', 'ready', artifact=artifact)
emit('ARTIFACT ' + artifact)
ask('Build lease held. release closes it:', 'release')
if state('verify').get('phase') != 'closed' or state('server').get('phase') != 'closed':
    fail('release build only after server and verifier close')
mark('build', 'closed', artifact=artifact)
emit('BUILD CLOSED')
finish(artifact=artifact, released=True)
