import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _term import Terminal, code

term=Terminal(sys.argv[1],'archive'); task='TW-008'
term.out('Archive retrieval. The real vault records are between ARCHIVE-BEGIN/ARCHIVE-END; preview records are decoys.\n')
term.out('PREVIEW vault=ZZZZZZZZZZ (not authoritative)\n')
term.busy(7.1,'decompressing history')
term.out('ARCHIVE-BEGIN\n')
for n in range(14500):
    if n%128==0:
        import select, os
        if select.select([0],[],[],0)[0]:
            if term.neutral(os.read(0,65536)):
                term.bad=True; term.save()
                term.out('EARLY INPUT during archive output: restart this attempt.\n')
    if n==177:
        term.out('VAULT-RECORD '+('segment/'*42)+' key='+code(task,'archive',80)+' END-VAULT\n')
    if n>=600 and (n-600)%517==0 and (n-600)//517<20:
        slot=(n-600)//517+1
        term.out('BOOKMARK slot='+str(slot)+' key='+code(task,'archive',slot)+' END-BOOKMARK\n')
    if n==421: term.out('\x1b[32mAUDIT-COUNT='+str(int(code(task,'archive',81)[:5],16)%9000+1000)+'\x1b[0m\n')
    term.out('audit '+str(n).zfill(5)+' '+('stable-diagnostic-field/'*4)+' checksum=unrelated\n')
if select.select([0],[],[],0)[0]:
    if term.neutral(os.read(0,65536)): term.bad=True; term.save()
term.out('ARCHIVE-END\n')
term.out('READY retrieval vault: submit exactly: vault <the authoritative key>\n'); term.accept('vault '+code(task,'archive',80))
count=int(code(task,'archive',81)[:5],16)%9000+1000
term.out('READY retrieval count: submit exactly: count <the coloured AUDIT-COUNT value, without colours>\n'); term.accept('count '+str(count))
for i in range(1,21):
    slot=((i-1)*7)%20+1
    term.busy(.29,'retrieving bookmark '+str(slot))
    term.out('PREVIEW bookmark slot='+str(slot)+' key='+code(task,'preview',slot)+' (expired)\n')
    term.out('READY bookmark slot='+str(slot)+'; submit exactly: bookmark <the archive BOOKMARK key for this slot>\n')
    term.accept('bookmark '+code(task,'archive',slot))
    # The result of one retrieval is deliberately replaced by a normal progress line.
    term.out('\r\x1b[2Kbookmark '+str(i)+' verified\n')
term.finish({'vault':code(task,'archive',80),'count':count,'index':code(task,'archive',90)})
