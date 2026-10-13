import atexit
import json
import os
from pathlib import Path
import select
import sys
import time

def init(work, name):
    global root, key, state
    root = Path(work).resolve() / '.fx'
    root.mkdir(parents=True, exist_ok=True)
    key = name
    try:
        with (root / (key + '.pid')).open('x') as pid_file:
            pid_file.write(str(os.getpid()))
    except FileExistsError:
        (root / (key + '.restart')).write_text('duplicate invocation')
        (root / (key + '-restart.pid')).write_text(str(os.getpid()))
        print('FAILED: duplicate invocation', flush=True)
        raise SystemExit(91)
    state = dict(pid=os.getpid(), parent=os.getppid(), tty=os.ttyname(0) if os.isatty(0) else '',
                 stdout_tty=os.ttyname(1) if os.isatty(1) else '',
                 cwd=os.getcwd(), start_ns=time.monotonic_ns(), events=[])
    save()
    if not state['tty'] or not state['stdout_tty']:
        fail('A terminal is required')
    return state

def save():
    tmp = root / (key + '.tmp')
    tmp.write_text(json.dumps(state, sort_keys=True))
    tmp.replace(root / (key + '.json'))

def event(name, **fields):
    state['events'].append(dict(name=name, ns=time.monotonic_ns(), **fields))
    state.update(fields)
    save()

def out(text):
    print(text, flush=True)

def fail(message):
    event('failed', failure=message, exit_code=90)
    out('FAILED: ' + message)
    raise SystemExit(90)

def finish(code=0, message=None, **fields):
    event('exit', exit_code=code, finished=True, **fields)
    if message is not None:
        out(message)
    raise SystemExit(code)

def ask(prompt, expected=None):
    from terminal import Terminal
    with Terminal() as term:
        event('prompt', prompt=prompt)
        out(prompt + '\r')
        try:
            line = term.line()
        except (EOFError, ValueError) as exc:
            fail(str(exc))
    event('input', received=line)
    if expected is not None and line != expected:
        fail('Incorrect response: ' + repr(line))
    return line


def load(name):
    return json.loads((root / (name + '.json')).read_text())

def barrier(names):
    out('Waiting for all participants to register...')
    while not all((root / (n + '.json')).exists() for n in names):
        time.sleep(0.1)
    event('barrier')
