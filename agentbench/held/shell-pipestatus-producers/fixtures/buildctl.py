import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *


def workflow_shell():
    """Find the persistent interactive bash, including through a PTY wrapper."""
    pid = os.getppid()
    while pid > 1:
        proc = Path('/proc') / str(pid)
        argv = (proc / 'cmdline').read_bytes().split(b'\0')
        if Path(os.fsdecode(argv[0])).name.lstrip('-') == 'bash':
            args = [arg for arg in argv[1:] if arg]
            flags = [arg for arg in args if arg.startswith(b'-') and not arg.startswith(b'--')]
            explicit_interactive = any(b'i' in arg for arg in flags)
            # Bash with terminal stdin and no command/script is also interactive,
            # as in a normal tmux login shell; an explicit -i is not required.
            has_command = any(b'c' in arg for arg in flags)
            has_script = any(not arg.startswith(b'-') for arg in args)
            terminal = os.readlink(proc / 'fd/0').startswith(('/dev/pts/', '/dev/tty'))
            if explicit_interactive or (terminal and not has_command and not has_script):
                return pid
        status = (proc / 'status').read_text()
        pid = int(next(line.split()[1] for line in status.splitlines() if line.startswith('PPid:')))
    f.fail('use one persistent interactive bash shell')


work, stage, *args = sys.argv[1:]
f = Fixture(work, 'build-' + stage, interactive=False)
if Path.cwd() != f.work / 'project' or os.getenv('BUILD_FLAVOR') != 'strict':
    f.fail('wrong shell directory or BUILD_FLAVOR')
parent = workflow_shell()
f.save(parent=parent, process_parent=os.getppid(), cwd=str(Path.cwd()), flavor=os.getenv('BUILD_FLAVOR'))
seed = (f.work / 'project' / 'manifest.txt').read_text().strip()
artifact = digest(seed + ':strict-build')
lint_status = 1 + int(digest(seed + ':lint-status', 2), 16) % 31
compile_status = 32 + int(digest(seed + ':compile-status', 2), 16) % 31
if stage == 'lint':
    print('\x1b[33mLINT E217: obsolete include in manifest\x1b[0m', flush=True)
    f.pause(1.4, guard=False)
    print('Lint finished with findings.', flush=True)
    f.done(exit_code=lint_status)
    raise SystemExit(lint_status)
elif stage == 'record-lint':
    lint = json.loads((f.fx / 'build-lint.json').read_text())
    if args != [str(lint_status)] or lint['phase'] != 'complete' or lint['parent'] != parent:
        f.fail('lint status or shell identity incorrect')
    f.done(reported_status=lint_status)
    print('LINT STATUS RECORDED', flush=True)
elif stage == 'compile':
    prior = json.loads((f.fx / 'build-record-lint.json').read_text())
    if prior['phase'] != 'complete' or prior['parent'] != parent:
        f.fail('record lint in this shell before compiling')
    for value, delay in [(11,1.3),(68,3.8),(100,.8)]:
        print(f'\r\x1b[2KCompiling [{value:3d}%]', end='', flush=True)
        f.pause(delay, guard=False)
    print('\nARTIFACT=' + artifact, flush=True)
    print(f'Build usable; warning policy exit={compile_status}', flush=True)
    f.done(exit_code=compile_status, artifact=artifact)
    raise SystemExit(compile_status)
elif stage == 'finalize':
    records = [json.loads((f.fx / ('build-' + s + '.json')).read_text()) for s in ('lint','record-lint','compile')]
    if args != [str(lint_status), str(compile_status)] or any(r['phase'] != 'complete' or r['parent'] != parent for r in records):
        f.fail('wrong statuses, order, or parent shell')
    f.done(lint_status=lint_status, compile_status=compile_status, artifact=artifact)
    print('SHELL WORKFLOW FINALIZED artifact=' + artifact, flush=True)
else:
    f.fail('unknown subcommand')
