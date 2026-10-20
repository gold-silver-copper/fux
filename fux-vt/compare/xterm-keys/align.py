import sys,os
run=sys.argv[1]
MODE={'legacy':'legacy','app':'legacy app','bkm':'legacy bkm','mok1':'legacy mok=1','mok2':'legacy mok=2','mok3':'legacy mok=3'}
CODE={'grave':96,'minus':45,'equal':61,'bracketleft':91,'bracketright':93,'backslash':92,'semicolon':59,'apostrophe':39,'comma':44,'period':46,'slash':47,'space':32,'Return':13,'Tab':9,'Escape':27,'BackSpace':127}
def esc(b):
    o=''
    for x in b:
        if x==0x1b: o+='\\e'
        elif x==0x5c: o+='\\\\'
        elif 0x21<=x<=0x7e or x==0x20: o+=chr(x)
        else: o+='\\x%02x'%x
    return o
out=["# Real xterm 411's bytes for each key fux names, typed through XTest into",
"# xterm on a private Xvfb display (fux-vt/compare/xterm-keys/run.sh), with the",
"# configuration fux-vt models: Alt as Meta, metaSendsEscape and eightBitInput",
"# off (so Meta sends an ESC before the key), the backarrow key DEL, a PC",
"# keyboard (sunKeyboard off). xterm decides legacy key bytes where ctlseqs",
"# is silent (README): source, mode, key, xterm's bytes, verdict."]
for m in ['legacy','app','bkm','mok1','mok2','mok3']:
    kf=os.path.join(run,m+'.keys')
    if not os.path.exists(kf): continue
    keys=[(float(l.split()[0]),l.split()[1],l.split()[2]) for l in open(kf) if l.strip()]
    names=[l.strip('\n') for l in open(os.path.join(run,m+'.names'))]
    reads=[(float(l.split()[0]),bytes.fromhex(l.split()[1])) for l in open(os.path.join(run,m+'.reads')) if l[0].isdigit()]
    got=[b'' for _ in keys]
    j=0
    for t,data in reads:
        while j+1<len(keys) and keys[j+1][0]<=t: j+=1
        if keys[j][0]<=t: got[j]+=data
    if len(names)!=len(keys): print('mismatch',m,len(names),len(keys)); continue
    for (t,mods,sym),name,b in zip(keys,names,got):
        bits=(1 if 'S' in mods else 0)|(2 if 'M' in mods else 0)|(4 if 'C' in mods else 0)
        code=CODE.get(sym, ord(sym) if len(sym)==1 else None)
        spec=name+(' code=%d'%code if code else '')+' mods=%d'%bits
        out.append('\t'.join(['xterm 411 %s %s'%(mods,sym),MODE[m],spec,esc(b) if b else 'none','agree']))
open(sys.argv[2],'w').write('\n'.join(out)+'\n')
print(len(out)-6)
