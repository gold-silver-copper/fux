import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

term=Terminal(sys.argv[1],'catalog'); task='TW-004'
term.size(78,24); term.out('\x1b[?1049h\x1b[?1000h\x1b[?1006h')
for i in range(1,9):
    term.busy(.42,'loading catalog page '+str(i))
    term.out('\x1b[2J\x1b[HCATALOG page='+str(i)+'\n')
    term.out('Select ARCHIVE using Down, then Enter. Current selection: LIVE\n')
    selected=0
    while True:
        k=term.key()
        if k=='DOWN': selected=1; term.out('Selected ARCHIVE\n')
        elif k=='UP': selected=0; term.out('Selected LIVE\n')
        elif k=='ENTER' and selected: break
    term.events.append('select:archive:'+str(i)); term.save()
    value=code(task,'catalog',i)
    term.out('READY form page='+str(i)+' value='+value+'\n')
    term.out('Type exactly the value above and press Enter (Backspace and Ctrl-U edit).\n')
    term.accept(value)
    if i==4:
        term.out('VALIDATION SERVICE UNAVAILABLE. Press F3 to retry validation; do not re-enter the form.\n')
        while term.key()!='F3': pass
        term.events.append('retry:validation'); term.save(); term.busy(4.3,'reconnecting validator')
    term.out('\x1b[10;1HREADY save button: click inside columns 5..16, row 12.\x1b[12;5H[ SAVE PAGE ]\x1b[14;1H')
    while True:
        k=term.key()
        if isinstance(k,tuple) and k[0]=='MOUSE' and 5<=k[1]<=16 and k[2]==12 and k[3]==0: break
    term.events.append('save:'+str(i)); term.save()
    if i==5:
        term.out('Resize requested: at least 96 columns and 28 rows before next page.\n'); term.size(96,28)
term.out('\x1b[?1000l\x1b[?1006l\x1b[?1049l')
term.finish({'catalog':code(task,'catalog',90),'pages':8})
