import runpy
import sys
from pathlib import Path
_lib = runpy.run_path(str(Path(__file__).with_name('common.py')))
Fixture, token, read_record = (_lib[k] for k in ('Fixture', 'token', 'read_record'))

import os
f = Fixture(sys.argv[1], 'audit')
if len(sys.argv) != 5:
    f.fail('syntax: audit.py WORK "$?" "$-" "$$"')
try:
    status, shell_pid = int(sys.argv[2]), int(sys.argv[4])
except ValueError:
    f.fail('status and shell PID must be decimal')
flags = sys.argv[3]
d = read_record(f.work, 'dispatch')
expected_status = 17 + int(token('dispatch-status', 4), 16) % 90
if d.get('state') != 'done' or status != expected_status or d.get('exit_status') != status:
    f.fail('audit did not receive dispatch exit status after dispatch ended')
if 'i' not in flags or shell_pid != os.getppid() or shell_pid != d.get('shell_pid') or Path.cwd().resolve() != f.work:
    f.fail('audit must run in the same interactive shell and working directory')
checkpoint = token('dispatch-audit:' + str(status) + ':' + d['receipt'], 18)
f.done(reported_status=status, shell_flags=flags, shell_pid=shell_pid, cwd=str(Path.cwd().resolve()), checkpoint=checkpoint)
f.out('AUDIT status=' + str(status) + ' checkpoint=' + checkpoint)
f.out('AUDIT CLOSED')
