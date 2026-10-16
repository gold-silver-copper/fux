TASK='TW-011'
import os, sys, json, time, hashlib, select, termios, tty, re, signal
from pathlib import Path
W=Path(sys.argv[1]).resolve()
if not (sys.stdin.isatty() and sys.stdout.isatty()):
    print('A terminal on both stdin and stdout is required.', file=sys.stderr)
    sys.exit(2)
F=W/'.fx'; F.mkdir(parents=True,exist_ok=True)
R={}; ROLE=''; OLD=None; SKIP_LF=False

def code(label, n=0):
    return hashlib.sha256((TASK+':'+label+':'+str(n)).encode()).hexdigest()[:10]
def atom(name,obj):
    p=F/name; q=F/(name+'.tmp.'+str(os.getpid()))
    q.write_text(json.dumps(obj,sort_keys=True)); q.replace(p)
def get(name):
    try: return json.loads((F/name).read_text())
    except FileNotFoundError: return {}
def save(): atom(ROLE+'.json',R)
def say(s='',end='\n'):
    sys.stdout.write(s+end); sys.stdout.flush()
def boot(role):
    global ROLE,R,OLD
    ROLE=role
    R={'tty':True,'ttyname':os.ttyname(0),'pid':os.getpid(),'start':time.time(),'events':[],'early':False,'ok':False}
    atom(role+'.pid',os.getpid()); atom(role+'.'+str(os.getpid())+'.pid',os.getpid()); save()
    OLD=termios.tcgetattr(0); tty.setraw(0)
    say('\x1b]2;'+TASK+'-'+role+'\x07',end='')
def close():
    if OLD is not None:
        say('\x1b[?1000l\x1b[?1006l'+('' if R.get('ok') else '\x1b[?1049l'),end='')
        termios.tcsetattr(0,termios.TCSANOW,OLD)
import atexit
atexit.register(close)
def fail(msg):
    R['error']=msg; R['end']=time.time(); save(); say('\r\nFAILED '+msg); sys.exit(3)
def finish(lines):
    R['ok']=True; R['lines']=lines; R['end']=time.time(); save()
    say('\r')
    for s in lines: say('RESULT '+s+'\r')
    say('COMPLETE\r')
def busy(seconds):
    global SKIP_LF
    end=time.monotonic()+seconds
    pending=b''
    markers=(b'\x1b[200~',b'\x1b[201~')
    while time.monotonic()<end or pending:
        delay=min(.08,max(0,end-time.monotonic())) if not pending else .08
        if select.select([0],[],[],delay)[0]:
            b=os.read(0,4096)
            if not b: fail('terminal disconnected during BUSY')
            if SKIP_LF:
                SKIP_LF=False
                if b.startswith(b'\n'): b=b[1:]
            pending+=b
            while pending:
                found=next((m for m in markers if pending.startswith(m)),None)
                if found: pending=pending[len(found):]; continue
                if any(m.startswith(pending) for m in markers): break
                R['early']=True; fail('input arrived during BUSY; restart this program')
def key():
    global SKIP_LF
    while True:
        b=os.read(0,1)
        if not b: fail('terminal disconnected')
        if SKIP_LF:
            SKIP_LF=False
            if b==b'\n': continue
        if b==b'\r': SKIP_LF=True; return 'ENTER'
        if b==b'\n': return 'ENTER'
        if b in (b'\x08',b'\x7f'): return 'BACKSPACE'
        if b==b'\x15': return 'CLEAR'
        if b==b'\x03': return 'INTERRUPT'
        if b!=b'\x1b': return b.decode('utf-8','replace')
        seq=b
        while True:
            part=os.read(0,1)
            if not part: fail('terminal disconnected within a key')
            seq+=part
            if seq==b'\x1b[M':
                while len(seq)<6: seq+=os.read(0,6-len(seq))
                break
            if len(seq)>=3 and (seq[-1:] in b'~ABCDPQRSZHF' or (seq.startswith(b'\x1b[<') and seq[-1:] in b'Mm')): break
        if seq in (b'\x1b[200~',b'\x1b[201~'): continue
        keys={b'\x1b[A':'UP',b'\x1bOA':'UP',b'\x1b[B':'DOWN',b'\x1bOB':'DOWN',b'\x1b[C':'RIGHT',b'\x1bOC':'RIGHT',b'\x1b[D':'LEFT',b'\x1bOD':'LEFT',b'\x1bOQ':'F2',b'\x1b[12~':'F2',b'\x1b[[B':'F2',b'\x1bOP':'F1',b'\x1b[11~':'F1',b'\x1b[6~':'PAGEDOWN',b'\x1b[H':'HOME',b'\x1bOH':'HOME',b'\x1b[1~':'HOME',b'\x1b[F':'END',b'\x1bOF':'END',b'\x1b[4~':'END',b'\x1b[3~':'DELETE'}
        if seq in keys: return keys[seq]
        m=re.fullmatch(rb'\x1b\[<(\d+);(\d+);(\d+)([Mm])',seq)
        if m:
            if m[4]==b'm' or int(m[1])&3==3: continue
            return ('CLICK',int(m[2]),int(m[3]))
        if seq.startswith(b'\x1b[M') and len(seq)==6:
            if (seq[3]-32)&3==3: continue
            return ('CLICK',seq[4]-32,seq[5]-32)
        return 'ESCAPE'
def line(prompt='',redraw=False):
    say(prompt,end=''); s=''; pos=0
    while True:
        k=key()
        if k=='ENTER': say('\r'); return s
        if k=='BACKSPACE':
            if pos: s=s[:pos-1]+s[pos:]; pos-=1
        elif k=='CLEAR': s=''; pos=0
        elif k=='LEFT': pos=max(0,pos-1)
        elif k=='RIGHT': pos=min(len(s),pos+1)
        elif k=='HOME': pos=0
        elif k=='END': pos=len(s)
        elif k=='DELETE': s=s[:pos]+s[pos+1:]
        elif isinstance(k,str) and len(k)==1 and k.isprintable(): s=s[:pos]+k+s[pos:]; pos+=1
        elif k=='INTERRUPT': fail('interrupted')
        if redraw: say('\r\x1b[2K'+prompt+s,end='')
def exact(wanted,prompt='> '):
    got=line(prompt,True)
    if got!=wanted: fail('incorrect submitted input')
    return got
def event(s): R['events'].append(s); save()
def screen(s): say('\x1b[H\x1b[2J'+s.replace('\n','\r\n')+'\r\n',end='')
def size(cols,rows):
    c,r=os.get_terminal_size(0)
    R.setdefault('sizes',[]).append([c,r]); save()
    if len(R['sizes'])==1 and c>110: fail('initial width must be at most 110 columns; got '+str(c))
    if c<cols or r<rows: fail('terminal needs at least %d columns and %d rows'%(cols,rows))
def waitfile(name,pred):
    while not pred(get(name)): busy(.1)
def flood(label,count=5300):
    for i in range(count):
        if i==29: say('ARCHIVE '+label+'\r')
        say(('log %05d '%i)+('discarded diagnostic padding '*9)+'\r')
    say('ARCHIVE END\r')

ROUNDS=24
ARCHIVE=False
LAYOUT=True
SESSION=False
RECOVERY=True

boot('desk')
N=ROUNDS
if LAYOUT: size(90,24)
if ARCHIVE:
    flood('vault='+code('vault'))
    exact('open','Archive copied? Enter open: ')
if SESSION:
    say('This desk is titled '+TASK+'-desk. Keep it running while visiting the two journals.\r')
lines=[]
for n in range(1,N+1):
    if LAYOUT and n==13:
        say('RESIZE REQUIRED: at least 120 columns and 30 rows; enter resized\r')
        exact('resized'); size(120,30); R['resized']=True; save()
    say('BUSY promotion '+str(n)+'\r'); busy(.12+(n%3)*.07)
    if RECOVERY and n==9:
        c=code('repair',n)
        say('Promotion blocked: lease expired. Enter renew '+c+'\r')
        exact('renew '+c); event('renew')
    c=code('desk',n)
    say('DESK %02d %s -- first run fetch %d in source, then fetch %d in audit.\r'%(n,c,n,n))
    suffix=' '+code('vault') if ARCHIVE else ''
    say('Submit: promote %d <source token> <audit token> %s%s\r'%(n,c,' <archive vault>' if ARCHIVE else ''))
    got=line('PROMOTE %02d> '%n,True)
    a=get('source.json'); b=get('audit.json')
    if a.get('published',0)!=n or b.get('published',0)!=n: fail('journals were not ready')
    expected='promote %d %s %s %s%s'%(n,code('source',n),code('audit',n),c,suffix)
    if got!=expected: fail('wrong promotion receipt')
    event('promote '+str(n)); R['published']=n; save()
    lines.append('release=%02d receipt=%s'%(n,code('receipt',n)))
    say('COMMITTED %02d\r'%n)
say('Close both journals with quit, then enter finish here.\r')
exact('finish')
if not get('source.json').get('ok') or not get('audit.json').get('ok'): fail('journals still open')
finish(lines)
