import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

import signal
term=Terminal(sys.argv[1],'assistant'); task='TW-003'
term.busy(6.7,'indexing repository')
term.out('Planner: prompts contain exact commands. Do not approve a plan before its READY line.\n')
for i in range(1,23):
    if i==8:
        term.out('\x1b[?1049h\x1b[2J\x1b[HREVIEW PERMISSION DIALOG\n')
        term.out('Network permission is selected. Press Down to select LOCAL ONLY, then F2 to confirm.\n')
        selected=0
        while True:
            k=term.key()
            if k=='DOWN': selected=1; term.out('\r\x1b[2KLOCAL ONLY selected\n')
            elif k=='UP': selected=0; term.out('Network selected\n')
            elif k=='F2' and selected==1: break
        term.events.append('permission:local'); term.save(); term.out('\x1b[?1049l')
    if i==15:
        term.out('STALLED tool call. Press Ctrl-C once to cancel only this call; the assistant will continue.\n')
        while term.key()!='CTRL_C': pass
        term.events.append('cancel:tool'); term.save()
        term.out('Recovered. The cancelled call changed no files.\n')
    term.out('Draft proposal '+str(i)+': provisional code '+code(task,'draft',i)+' (not a grant)\n')
    term.challenge(task,i,'grant',delay=.32+(i%3)*.13)
term.finish({'revision':code(task,'assistant',90),'scope':'local-only'})
