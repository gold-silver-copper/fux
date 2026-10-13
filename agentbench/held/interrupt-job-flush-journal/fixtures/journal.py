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

interrupts = 0; armed = False; terminating = False
def interrupt(signum, frame):
    global interrupts
    if not armed or terminating: fail('interrupt at wrong lifecycle stage')
    interrupts += 1
    emit('IGNORED SIGINT count=' + str(interrupts))
def terminate(signum, frame):
    global terminating
    if not armed or interrupts != 1: fail('one acknowledged interrupt required before SIGTERM')
    terminating = True
signal.signal(signal.SIGINT, interrupt)
signal.signal(signal.SIGTERM, terminate)
emit('Journal opening, pid=' + str(os.getpid()))
delay(7)
armed = True
emit('JOURNAL ARMED pid=' + str(os.getpid()))
while not terminating:
    time.sleep(.1)
emit('FLUSHING buffered journal; do not force kill')
delay(3)
(work / 'journal.txt').write_text('entry=alpha\nentry=beta\ncommitted=2\n')
emit('JOURNAL CLOSED')
finish(interrupts=interrupts, signal='SIGTERM', flushed=True)
