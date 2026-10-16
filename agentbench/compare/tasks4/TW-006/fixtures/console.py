import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

import os, json
role=sys.argv[2]; term=Terminal(sys.argv[1],role); task='TW-006'
if role=='journal':
    term.out('\x1b]2;journal-writer\x07')
    term.busy(5.9,'opening durable journal')
    for i in range(1,7): term.challenge(task,i,'checkpoint')
    term.atomic(term.fx/'journal-ready.json',json.dumps({'seal':code(task,role,90)}))
    term.out('PARKED journal: waiting for migration and audit; do not type yet.\n')
    while True:
        try:
            completed=json.loads((term.fx/'audit.json').read_text())
            if completed.get('success'): break
        except (OSError,ValueError): pass
        term.busy(.3,'audit dependency')
    term.out('READY shutdown: submit exactly: shutdown '+code(task,role,91)+'\n')
    term.accept('shutdown '+code(task,role,91)); term.finish({'journal':code(task,role,90)})
elif role=='command':
    # The shell itself supplies $-; shell invocation flags are not used as evidence.
    flags=sys.argv[3]
    if 'i' not in flags: term.out('An interactive shell is required.\n'); raise SystemExit(3)
    term.record['shell_pid']=os.getppid()
    term.events.append('interactive')
    while not (term.fx/'journal-ready.json').exists(): term.busy(.2,'journal dependency')
    term.wait_live(['journal'])
    term.busy(3.8,'loading migration')
    for i in range(1,11): term.challenge(task,i,'apply')
    status=int(code(task,role,90)[:4],16)%70+20
    term.record.update(success=not term.bad,result={'status':status,'migration':code(task,role,91)})
    term.save(); term.out('RESULT migration='+code(task,role,91)+'\n')
    term.out('Migration deliberately failed after the signed checkpoint; read its exit status from your shell.\n')
    raise SystemExit(status)
elif role=='audit':
    term.wait_live(['journal'])
    r=json.loads((term.fx/'command.json').read_text())
    if not r.get('success'): raise SystemExit(7)
    term.out('READY status audit: submit exactly: status <the previous command exit status>\n')
    term.accept('status '+str(r['result']['status']))
    for i in range(1,7): term.challenge(task,i,'audit')
    term.finish({'audit':code(task,role,90)})
