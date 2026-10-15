import runpy
import sys
from pathlib import Path
_lib = runpy.run_path(str(Path(__file__).with_name('common.py')))
Fixture, token, read_record = (_lib[k] for k in ('Fixture', 'token', 'read_record'))

import os
f = Fixture(sys.argv[1], 'dispatch')
flags = os.environ.get('FX_INTERACTIVE_FLAGS', '')
try:
    shell_pid = int(os.environ.get('FX_SHELL_PID', '0'))
except ValueError:
    shell_pid = 0
f.r.update(shell_flags=flags, shell_pid=shell_pid, cwd=str(Path.cwd().resolve()))
f.save()
if 'i' not in flags or shell_pid != os.getppid() or Path.cwd().resolve() != f.work:
    f.fail('use an interactive shell in the work directory and the documented environment assignments')
challenge = token('dispatch-challenge', 14)
receipt = token('dispatch-first-receipt', 22)
status = 17 + int(token('dispatch-status', 4), 16) % 90
f.out('CACHED status=' + str((status + 11) % 127) + ' RECEIPT ' + token('dispatch-cache', 22))
f.out('Connecting to local spool; no input until INPUT READY.')
f.guard(5.4)
command = 'finish RECEIPT ' + challenge
f.expect('type exactly ' + command, command)
f.out('FINISH ACCEPTED')
f.guard(2.3)
f.out('\x1b[32mRECEIPT ' + receipt + '\x1b[0m', end='')
f.guard(.31)
f.out('\r\x1b[2KDispatch receipt display cleared', end='')
f.guard(.8)
f.out('\r\x1b[2KRECEIPT ' + token('dispatch-later-receipt', 22))
for part, delay in enumerate((2.1, 3.8, 2.7), 1):
    f.out('\r\x1b[2KClosing spool [' + '#' * part + '.' * (3 - part) + ']', end='')
    f.guard(delay)
f.out('\r\x1b[2KSPOOL CLOSED; returning control to the shell')
f.done(receipt=receipt, exit_status=status)
raise SystemExit(status)
