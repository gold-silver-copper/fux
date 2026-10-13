
import atexit, hashlib, json, os, select, signal, sys, time
import termios, tty
from pathlib import Path
work = Path(sys.argv[1]).resolve()
fx = work / '.fx'
fx.mkdir(parents=True, exist_ok=True)
name = 'archive_' + sys.argv[2]
terminal = {'stdin_tty': os.isatty(sys.stdin.fileno()),
            'stdout_tty': os.isatty(sys.stdout.fileno())}
if all(terminal.values()):
    original_terminal = termios.tcgetattr(sys.stdin.fileno())
    tty.setcbreak(sys.stdin.fileno())
    attributes = termios.tcgetattr(sys.stdin.fileno())
    attributes[0] &= ~(termios.ICRNL | termios.INLCR | termios.IGNCR | termios.IXON)
    termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, attributes)
    def restore_input():
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, original_terminal)
    atexit.register(restore_input)
def record(key, data):
    target = fx / (key + '.json')
    temp = fx / (key + '.tmp')
    temp.write_text(json.dumps(dict(data, terminal=terminal), sort_keys=True))
    temp.replace(target)
def emit(s):
    print(s, flush=True)
def fail(message):
    record(name + '.error', {'error': message})
    emit('ERROR ' + message)
    raise SystemExit(9)
def pid():
    (fx / (name + '.pid')).write_text(str(os.getpid()) + '\n')
# Isolated Python deliberately excludes the script directory from sys.path.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from terminal_io import TerminalInput
input_reader = TerminalInput(fail)
delay = input_reader.delay
line = input_reader.line
keypress = input_reader.keypress
rawline = input_reader.rawline
if not all(terminal.values()):
    fail('stdin and stdout must both be terminals')
def digest(s):
    return hashlib.sha256(s.encode()).hexdigest()[:12]
pid()

role = sys.argv[2]
ticket = digest('archive-watch:epoch-3')
maint = digest(ticket + ':maintenance-complete')
resume = digest(maint + ':same-process')
if role == 'watch':
    for key in ('parked', 'maintenance', 'resumed', 'stopped'):
        (fx / ('archive.' + key + '.json')).unlink(missing_ok=True)
    emit('\x1b]0;archive-watch\x07Archive watcher — restoring journal')
    delay(3)
    emit('WATCH READY ticket=' + ticket)
    received = line('park ' + ticket)
    record('archive.parked', {'ticket': ticket, 'received': received, 'pid': os.getpid()})
    emit('PARKED archive-watch; keep this process running')
    n = 0
    while not (fx / 'archive.maintenance.json').exists():
        delay(1)
        n += 1
        emit(f'heartbeat archive-watch cycle={n}')
    data = json.loads((fx / 'archive.maintenance.json').read_text())
    if data['watch_pid'] != os.getpid() or data['receipt'] != maint:
        fail('maintenance belongs to a different watcher')
    emit('RESUME AVAILABLE code=' + resume)
    actual = line('resume ' + resume)
    record('archive.resumed', {'ticket': ticket, 'maintenance': maint, 'resume': resume,
                              'received': actual, 'pid': os.getpid()})
    emit('RESUMED ticket=' + ticket)
    emit('STOP PROMPT type stop ' + ticket)
    final = line('stop ' + ticket)
    record('archive.stopped', {'received': final, 'ticket': ticket, 'pid': os.getpid()})
    emit('STOPPED archive-watch')
elif role == 'maintain':
    if not (fx / 'archive.parked.json').exists():
        fail('watcher must be parked before maintenance')
    parked = json.loads((fx / 'archive.parked.json').read_text())
    watch_pid = parked['pid']
    try:
        os.kill(watch_pid, 0)
    except ProcessLookupError:
        fail('watcher is no longer running')
    emit('Maintenance running while archive watcher is unattended')
    delay(9)
    try:
        os.kill(watch_pid, 0)
    except ProcessLookupError:
        fail('watcher was lost during maintenance')
    record('archive.maintenance', {'receipt': maint, 'watch_pid': watch_pid, 'state': 'complete'})
    emit('MAINTENANCE COMPLETE receipt=' + maint)
else:
    fail('unknown archive role')
