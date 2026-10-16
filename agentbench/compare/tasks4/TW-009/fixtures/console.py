import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

import json
role=sys.argv[2]; term=Terminal(sys.argv[1],role); task='TW-009'
term.out('\x1b]2;'+role+'-ledger\x07')
if role=='runner':
    term.wait_live(['inspector','repairer'])
    for i in range(1,9): term.challenge(task,i,'run')
    term.out('READY leave: submit exactly: continue-unwatched\n'); term.accept('continue-unwatched')
    term.busy(11.3,'unattended checkpoint')
    ticket=code(task,role,80)
    term.atomic(term.fx/'unattended.json',json.dumps({'ticket':ticket}))
    term.out('\x1b]2;runner-checkpoint-ledger\x07UNATTENDED CHECKPOINT ticket='+ticket+'\n')
    term.out('Runner suspended: wait for inspector and repairer; do not restart it.\n')
    while True:
        try:
            r=json.loads((term.fx/'repairer.json').read_text())
            if r.get('success'): break
        except (OSError,ValueError): pass
        term.busy(.3,'repair dependency')
    for i in range(9,15): term.challenge(task,i,'run')
    term.finish({'ledger':code(task,role,90)})
elif role=='inspector':
    while not (term.fx/'unattended.json').exists(): term.busy(.3,'unattended dependency')
    r=json.loads((term.fx/'unattended.json').read_text())
    term.out('READY inspect: submit exactly: inspect <runner checkpoint ticket>\n'); term.accept('inspect '+r['ticket'])
    for i in range(1,5): term.challenge(task,i,'inspect')
    term.finish({'diagnosis':code(task,role,90)})
elif role=='repairer':
    while True:
        try:
            r=json.loads((term.fx/'inspector.json').read_text())
            if r.get('success'): break
        except (OSError,ValueError): pass
        term.busy(.3,'inspection dependency')
    term.out('READY repair: submit exactly: repair <inspector diagnosis>\n'); term.accept('repair '+r['result']['diagnosis'])
    for i in range(1,5): term.challenge(task,i,'repair')
    term.finish({'repair':code(task,role,90)})
