# The keys to type for one mode: lines of "MODS KEYSYM\tFUXNAME".
import sys
mode=sys.argv[1]
SHIFTED={'`':'~','1':'!','2':'@','3':'#','4':'$','5':'%','6':'^','7':'&','8':'*','9':'(','0':')','-':'_','=':'+','[':'{',']':'}','\\':'|',';':':',"'":'"',',':'<','.':'>','/':'?'}
SYM={'`':'grave','-':'minus','=':'equal','[':'bracketleft',']':'bracketright','\\':'backslash',';':'semicolon',"'":'apostrophe',',':'comma','.':'period','/':'slash',' ':'space'}
chars=[chr(c) for c in range(ord('a'),ord('z')+1)]+[str(i) for i in range(10)]+list("`-=[]\\;',./ ")
named=[('Return','Enter'),('Tab','Tab'),('Escape','Escape'),('BackSpace','BSpace'),('Delete','Delete'),('Insert','Insert'),('Up','Up'),('Down','Down'),('Left','Left'),('Right','Right'),('Home','Home'),('End','End'),('Prior','PageUp'),('Next','PageDown')]+[('F%d'%i,'F%d'%i) for i in range(1,13)]
MODS=['-','S','C','M','CS','CM','MS','CMS']
def fux(mods,base,named_key):
    pre=('C-' if 'C' in mods else '')+('M-' if 'M' in mods else '')
    if named_key:
        return pre+('S-' if 'S' in mods else '')+base
    ch=base
    if 'S' in mods: ch=ch.upper() if ch.isalpha() else SHIFTED.get(ch,ch)
    return pre+('Space' if ch==' ' else ch)
keys=[]
if mode in ('legacy','mok1','mok2','mok3'):
    for ch in chars:
        for m in MODS: keys.append((m,SYM.get(ch,ch),fux(m,ch,False)))
    for sym,name in named:
        for m in MODS: keys.append((m,sym,fux(m,name,True)))
elif mode=='app':
    for sym,name in named[6:12]:
        for m in MODS: keys.append((m,sym,fux(m,name,True)))
elif mode=='bkm':
    for m in MODS: keys.append((m,'BackSpace',fux(m,'BSpace',True)))
for m,s,f in keys: print('%s %s\t%s'%(m,s,f))
