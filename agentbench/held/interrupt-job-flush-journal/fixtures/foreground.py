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

mode = sys.argv[2]
if mode == 'run':
    armed = False
    def interrupt(signum, frame):
        if not armed: fail('job interrupted before it was armed')
        save('interrupted.json', {'status': 130, 'signal': 'SIGINT', 'armed': True, 'terminal': os.ttyname(sys.stdin.fileno())})
        emit('JOB INTERRUPTED; exit 130')
        finish(130, mode='run', signal='SIGINT')
    signal.signal(signal.SIGINT, interrupt)
    emit('Preparing foreground job...')
    delay(4)
    armed = True
    emit('JOB ARMED; Ctrl-C cancels this job')
    while True: time.sleep(.1)
elif mode == 'recover':
    if not (fx / 'interrupted.json').exists(): fail('interrupt the original job first')
    terminal = os.ttyname(sys.stdin.fileno())
    if json.loads((fx / 'interrupted.json').read_text()).get('terminal') != terminal:
        fail('recovery must run in the original terminal')
    emit('Recovering abandoned foreground transaction...')
    delay(2)
    (work / 'recovery.txt').write_text('rollback=complete\n')
    emit('RECOVERY COMPLETE')
    finish(mode='recover', recovered=True, terminal=terminal)
else: fail('unknown mode')
