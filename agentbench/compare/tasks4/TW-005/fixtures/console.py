import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

term=Terminal(sys.argv[1],'patchdesk'); task='TW-005'
term.size(72,22); term.out('\x1b[?1049h')
for i in range(1,8):
    term.busy(.55,'rendering diff '+str(i))
    term.out('\x1b[2J\x1b[HPATCHDESK revision='+str(i)+'\n')
    term.out('Diff viewer: press PageDown twice, then F4 to open the patch editor.\n')
    pages=0
    while True:
        k=term.key()
        if k=='PAGEDOWN':
            pages+=1; term.out('Diff page '+str(pages)+'\n')
        elif k=='F4' and pages>=2: break
    term.events.append('view:'+str(i)); term.save()
    term.out('REFERENCE '+('context/'*31)+' seal='+code(task,'patchdesk',i)+' end-reference\n')
    term.out('READY editor revision='+str(i)+'\n')
    term.out('Submit three lines (paste is supported):\nBEGIN '+str(i)+'\nseal <the reference seal above>\nEND\n')
    term.accept('BEGIN '+str(i)); term.accept('seal '+code(task,'patchdesk',i)); term.accept('END')
    term.out('READY review revision='+str(i)+': press F2 to review, then Right and Enter to commit.\n')
    while term.key()!='F2': pass
    term.out('\x1b[2J\x1b[HCOMMIT MODAL: Cancel selected; Right selects Commit.\n')
    selected=False
    while True:
        k=term.key()
        if k=='RIGHT': selected=True; term.out('Commit selected\n')
        elif k=='LEFT': selected=False
        elif k=='ENTER' and selected: break
    term.events.append('commit:'+str(i)); term.save()
    if i==3:
        term.out('Widen diff viewer to at least 100 columns and 26 rows.\n'); term.size(100,26)
term.out('\x1b[?1049l'); term.finish({'patchset':code(task,'patchdesk',90),'commits':7})
