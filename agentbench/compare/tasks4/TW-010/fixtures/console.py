import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

import json
index=int(sys.argv[2]); role='worker'+str(index); term=Terminal(sys.argv[1],role); task='TW-010'
resume=(len(sys.argv)>3 and sys.argv[3]=='resume')
if resume:
    prior=json.loads((term.fx/(role+'-crash.json')).read_text())
    if index!=3 or not prior.get('tty') or sys.argv[4]!=prior['receipt']: raise SystemExit(9)
    term.events=prior['events']; term.out('Recovered worker3 checkpoint\n')
    saved=json.loads((term.fx/(role+'-round-1.json')).read_text())
    term.out('ROUND result round=1 value='+saved['token']+'\n')
if not resume:
    term.atomic(term.fx/(role+'-started.json'),json.dumps({'tty':term.record['tty'],'pid':term.record['pid']}))
    for peer in range(8):
        while not (term.fx/('worker'+str(peer)+'-started.json')).exists(): term.busy(.2,'startup barrier')
start=2 if resume else 1
for round_ in range(start,4):
    if round_>1:
        for peer in range(8):
            p=term.fx/('worker'+str(peer)+'-round-'+str(round_-1)+'.json')
            while not p.exists(): term.busy(.2,'round barrier')
        prev=(index+7)%8
        data=json.loads((term.fx/('worker'+str(prev)+'-round-'+str(round_-1)+'.json')).read_text())
        term.out('READY dependency round='+str(round_)+'; submit exactly: previous <worker'+str(prev)+' ROUND result for round '+str(round_-1)+'>\n')
        term.accept('previous '+data['token'])
    term.challenge(task,round_,'commit',delay=.45+index*.17)
    seal=code(task,role,100+round_)
    term.atomic(term.fx/(role+'-round-'+str(round_)+'.json'),json.dumps({'token':seal,'tty':term.record['tty']}))
    term.out('ROUND result round='+str(round_)+' value='+seal+'\n')
    if index==3 and round_==1:
        receipt=code(task,role,77)
        term.atomic(term.fx/(role+'-crash.json'),json.dumps({'events':term.events,'tty':term.record['tty'],'receipt':receipt}))
        term.out('CRASH checkpoint saved. Restart console.py WORK 3 resume '+receipt+'\n'); raise SystemExit(46)
term.finish({'result':code(task,role,90)})
