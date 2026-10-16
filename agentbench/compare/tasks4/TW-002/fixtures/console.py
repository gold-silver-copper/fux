import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

import json, os
role=sys.argv[2]; term=Terminal(sys.argv[1],role); task='TW-002'
roles=['north','east','west']; index=roles.index(role)
term.out('\x1b]2;replica-'+role+'\x07')
term.size(44,12)
for epoch in range(1,9):
    if epoch==5:
        term.out('MIDPOINT: widen this terminal to at least 58 columns and 16 rows.\n'); term.size(58,16)
    term.challenge(task,epoch,'seal',delay=.41 if epoch%2 else .67)
    cert=code(task,role,100+epoch)
    term.atomic(term.fx/(role+'-epoch-'+str(epoch)+'.json'),json.dumps({'certificate':cert,'tty':term.record['tty']}))
    term.out('CERT epoch='+str(epoch)+' owner='+role+' value='+cert+'\n')
    other=roles[(index+1)%3]
    while not (term.fx/(other+'-epoch-'+str(epoch)+'.json')).exists(): term.busy(.2,'waiting for '+other)
    data=json.loads((term.fx/(other+'-epoch-'+str(epoch)+'.json')).read_text())
    term.out('READY exchange epoch='+str(epoch)+'; submit exactly: import '+other+' <that peer certificate for this epoch>\n')
    term.accept('import '+other+' '+data['certificate'])
term.out('READY shutdown: submit exactly: stop '+code(task,role,200)+'\n')
term.accept('stop '+code(task,role,200))
term.finish({'certificate':code(task,role,208)})
