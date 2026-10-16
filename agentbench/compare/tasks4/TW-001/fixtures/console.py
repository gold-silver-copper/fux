import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

import json, os
role=sys.argv[2]; term=Terminal(sys.argv[1],role); task='TW-001'
def wait_record(name):
    term.out('Waiting for '+name+' to finish its signed stage.\n')
    while True:
        try:
            r=json.loads((term.fx/(name+'.json')).read_text())
            if r.get('success'): return r
        except (OSError,ValueError): pass
        term.busy(.31,'dependency '+name)
term.size(65,16)
if role=='build':
    term.wait_live(['server','canary'])
    term.busy(8.1,'compiling schema')
    for i in range(1,9): term.challenge(task,i,'approve')
    receipt=code(task,'build',80)
    term.record.update(crashed=True,receipt=receipt,success=not term.bad,result={}); term.save()
    term.out('Compiler crashed after durable checkpoint. Restart as console.py WORK rebuild '+receipt+'\n')
    raise SystemExit(37)
elif role=='rebuild':
    r=json.loads((term.fx/'build.json').read_text())
    if not r.get('crashed') or sys.argv[3]!=r['receipt']: raise SystemExit(4)
    for i in range(9,13): term.challenge(task,i,'approve')
    term.finish({'artifact':code(task,role,90)})
elif role=='server':
    r=wait_record('rebuild')
    term.out('READY artifact-input: submit exactly: load <artifact reported by rebuilt compiler>\n')
    term.accept('load '+r['result']['artifact'])
    for i in range(1,7): term.challenge(task,i,'deploy')
    term.finish({'endpoint':code(task,role,90)})
elif role=='canary':
    r=wait_record('server')
    term.out('READY endpoint-input: submit exactly: target <endpoint reported by server>\n')
    term.accept('target '+r['result']['endpoint'])
    for i in range(1,7): term.challenge(task,i,'verify')
    term.finish({'release':code(task,role,90)})
