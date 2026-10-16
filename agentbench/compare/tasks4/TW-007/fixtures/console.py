import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

import json, os
role=sys.argv[2]; term=Terminal(sys.argv[1],role); task='TW-007'
if role in ('primary','backup'):
    # Both providers must be present before the race is released.
    term.atomic(term.fx/(role+'-present.json'),json.dumps({'tty':term.record['tty']}))
    term.out('READY arm '+role+': submit exactly: arm\n'); term.accept('arm')
    term.atomic(term.fx/(role+'-armed.json'),json.dumps({'armed':True}))
    while not all((term.fx/(p+'-armed.json')).exists() for p in ('primary','backup')): term.busy(.2,'race barrier')
    if role=='primary':
        term.busy(6.4,'primary warmup')
        term.out('PRIMARY DEGRADED: it will not become ready. Submit exactly: retire\n'); term.accept('retire')
        term.finish({'outcome':'degraded'})
    else:
        term.busy(9.7,'backup replay')
        for i in range(1,9): term.challenge(task,i,'replay')
        term.atomic(term.fx/'winner.json',json.dumps({'provider':'backup','token':code(task,role,90)}))
        term.out('READY provider=backup ticket='+code(task,role,90)+'\n')
        term.out('Ignore Ctrl-C at this parked stage; it only requests a status redraw. Wait for the consumer to finish.\n')
        while True:
            k=term.key()
            if k=='CTRL_C':
                term.events.append('interrupt:redraw'); term.save(); term.out('Still running; input shutdown as one line only after consumer completion.\n'); break
        while True:
            try:
                c=json.loads((term.fx/'consumer.json').read_text())
                if c.get('success'): break
            except (OSError,ValueError): pass
            term.busy(.25,'consumer dependency')
        term.out('READY shutdown: submit exactly: stop '+code(task,role,91)+'\n')
        term.accept('stop '+code(task,role,91)); term.finish({'ticket':code(task,role,90)})
else:
    while not (term.fx/'winner.json').exists(): term.busy(.3,'waiting for a viable provider')
    winner=json.loads((term.fx/'winner.json').read_text())
    term.out('READY choose: submit exactly: choose <viable provider name> <its READY ticket>\n')
    term.accept('choose '+winner['provider']+' '+winner['token'])
    for i in range(1,9): term.challenge(task,i,'consume')
    term.finish({'digest':code(task,role,90)})
