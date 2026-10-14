NAME = 'builder'

import atexit
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import sys
import time

work = Path(sys.argv[1]).resolve()
fx = work / '.fx'
fx.mkdir(parents=True, exist_ok=True)
(fx / (NAME + '.pid')).write_text(str(os.getpid()))
with (fx / (NAME + '.starts')).open('a') as f:
    f.write('start\n')

def digest(s, n=12):
    return hashlib.sha256(s.encode()).hexdigest()[:n]

def save(name, value):
    (fx / name).write_text(json.dumps(value, sort_keys=True))

def emit(s='', end='\n'):
    print(s, end=end, flush=True)

def require(ok, message):
    if not ok:
        raise RuntimeError(message)

def alive(name):
    try:
        os.kill(int((fx / (name + '.pid')).read_text()), 0)
        return True
    except (OSError, ValueError):
        return False

def main():
    source = (work / 'source.txt').read_text()
    require(source == 'widget=quartz\nversion=4\n', 'source changed')
    emit('BUILD: dependency scan')
    for phase in ('compile 1/3', 'compile 2/3', 'compile 3/3', 'link'):
        time.sleep(2.5)
        emit('\x1b[36mBUILD: ' + phase + '\x1b[0m')
    emit('BUILD: waiting for test runner and preview')
    while not (alive('test_runner') and alive('preview') and
               (fx / 'test_runner.terminal.json').exists() and
               (fx / 'preview.terminal.json').exists()):
        time.sleep(.05)
    token = 'B-' + digest(source)
    save('build.json', {'token': token, 'companions_alive': True})
    emit('BUILD READY token=' + token)

try:
    require(os.isatty(0) and os.isatty(1), 'interactive terminal required')
    save(NAME + '.terminal.json', {'stdin': os.ttyname(0), 'stdout': os.ttyname(1)})
    main()
except SystemExit as e:
    save(NAME + '.exit.json', {'code': e.code or 0})
    raise
except BaseException as e:
    save(NAME + '.error.json', {'error': str(e)})
    save(NAME + '.exit.json', {'code': 91})
    emit('FIXTURE ERROR: ' + str(e))
    raise SystemExit(91)
else:
    save(NAME + '.exit.json', {'code': 0})
