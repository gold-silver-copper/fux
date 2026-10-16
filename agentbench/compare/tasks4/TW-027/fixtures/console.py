import atexit
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import sys
import termios
import time
import tty

CFG = {'id': 'TW-027', 'title': 'Route a delayed deployment outcome after the log drain', 'category': 'wait', 'roles': {'runner': ['open', 'plan', 'stage', 'compile', 'package', 'audit', 'arm', 'launch'], 'gate': ['attach', 'review', 'policy', 'route', 'settle', 'approve', 'activate', 'seal'], 'tail': ['open', 'follow', 'mark', 'checkpoint', 'filter', 'confirm', 'summarize', 'seal']}, 'imports': {'gate:0': 'runner', 'tail:3': 'runner', 'gate:3': 'tail'}, 'order': ['runner', 'tail', 'gate'], 'busy': {'runner:3': 8, 'runner:7': 17, 'tail:4': 6}, 'quiet_step': 'gate:4', 'recovery': {'runner:6': 'A recoverable preflight failure was logged. It is not the final outcome; arm the retry and wait for the later RESULT.'}, 'kinds': ['several-programs', 'long-horizon', 'recovery', 'echo-traps', 'overwritten-text', 'sessions'], 'needs': ['start-program', 'type-text', 'wait-text', 'wait-quiet', 'wait-exit', 'read-screen', 'several-panes', 'pane-names'], 'story': 'Run runner, gate, and tail concurrently, naming the tail terminal deploy-tail. Keep that original tail alive while attending to the other programs, and return to it by its name. Complete runner including the retry: preflight FAILURE and spinner values are provisional; wait for its final RESULT before submitting any receipt derived from it. Complete tail using that RESULT at checkpoint, then complete gate using runner at attach and tail at route. At gate settle, wait until the displayed drain has been quiet for at least three seconds before submitting the displayed command. There is no completion deadline; every program waits for the next valid input. Finish all stages and close the same three processes.'}

def digest(s):
    return hashlib.sha256((CFG['id']+'|'+s).encode()).hexdigest()[:12]

def code(role, step):
    return digest('challenge|'+role+'|'+str(step))

def job_status(role):
    return 1+int(digest('exit|'+role)[:4],16)%47

def expected_token(role, step):
    source = CFG.get('imports',{}).get(role+':'+str(step))
    if source:
        r = receipt(source)
        if CFG.get('shell') and source.startswith('j'):
            return str(job_status(source))+' '+r
        return r
    return code(role,step)

def transcript(role):
    return [verb+' '+expected_token(role,i) for i,verb in enumerate(CFG['roles'][role])]

def receipt(role):
    return digest('receipt|'+role+'|'+'|'.join(transcript(role)))

if len(sys.argv)<3:
    raise SystemExit('usage: console.py WORK ROLE [--shell-flags FLAGS]')
work=Path(sys.argv[1]).resolve()
role=sys.argv[2]
if role not in CFG['roles']:
    raise SystemExit('unknown role')
fx=work/'.fx'
fx.mkdir(parents=True,exist_ok=True)
pid=os.getpid()

def atomic(path, data):
    temp=path.with_name(path.name+'.'+str(pid)+'.tmp')
    temp.write_text(data)
    os.replace(temp,path)

state={'role':role,'pid':pid,'tty':os.isatty(0) and os.isatty(1), 'received':[], 'early':False,'phase':'starting','success':False}
if state['tty']:
    state['terminal']=os.ttyname(0)
atomic(fx/(role+'.pid'),str(pid)+'\n')

def save(**kw):
    state.update(kw)
    atomic(fx/(role+'.json'),json.dumps(state,sort_keys=True)+'\n')

save()
if not state['tty']:
    save(phase='failed',error='stdin and stdout must both be terminals')
    raise SystemExit(2)
old=termios.tcgetattr(0)
tty.setraw(0)
is_tui=role in CFG.get('tui',[])

def output(s='',end='\r\n'):
    sys.stdout.write(s+end)
    sys.stdout.flush()

def restore():
    if is_tui:
        output('\x1b[?1049l',end='')
    termios.tcsetattr(0,termios.TCSANOW,old)
atexit.register(restore)

def fail(reason):
    save(phase='failed',error=reason)
    output('\x1b[31mRESTART REQUIRED: '+reason+'\x1b[0m')
    raise SystemExit(3)

def on_signal(sig,frame):
    fail('interrupted before clean close')
signal.signal(signal.SIGTERM,on_signal)
signal.signal(signal.SIGINT,on_signal)

def pending_fail():
    global last_was_cr
    if select.select([0],[],[],0)[0]:
        b=os.read(0,4096)
        # A CRLF pair is one Enter, even when the LF arrives a little later.
        if last_was_cr and b.startswith(b'\n'):
            b=b[1:]
            last_was_cr=False
        b=b.replace(b'\x1b[200~',b'').replace(b'\x1b[201~',b'')
        if b:
            save(early=True)
            fail('input arrived while busy; wait for the named input prompt')

def pause(seconds,label=None):
    end=time.monotonic()+seconds
    tick=0
    while time.monotonic()<end:
        pending_fail()
        if label and tick%5==0:
            output('\r\x1b[2K\x1b[33m'+label+' '+('/-\\|'[(tick//5)%4])+' provisional='+digest(label+'|'+str(tick))+'\x1b[0m',end='')
        time.sleep(min(.10,max(0,end-time.monotonic())))
        tick+=1
    pending_fail()
    if label:
        output('\r\x1b[2K'+label+' complete; provisional values discarded')

def alive(p):
    try:
        os.kill(p,0)
        return True
    except (ProcessLookupError,PermissionError):
        return False

def read_state(name):
    try:
        return json.loads((fx/(name+'.json')).read_text())
    except (FileNotFoundError,json.JSONDecodeError):
        return {}

def peers():
    if CFG.get('shell'):
        names=[] if role=='monitor' else ['monitor']
    else:
        names=[x for x in CFG['roles'] if x!=role]
    while True:
        records={name:read_state(name) for name in names}
        if all(v.get('tty') and alive(v.get('pid',0)) and v.get('phase') not in ('failed','closed') for v in records.values()):
            terminals=[state['terminal']]+[v['terminal'] for v in records.values()]
            if len(terminals)!=len(set(terminals)):
                fail('concurrent programs need separate terminals')
            save(concurrent={k:{'pid':v['pid'],'terminal':v['terminal']} for k,v in records.items()})
            return
        pending_fail()
        time.sleep(.12)

last_was_cr=False
def event():
    global last_was_cr
    while True:
        b=os.read(0,1)
        if not b:
            fail('terminal input closed')
        if last_was_cr and b==b'\n':
            last_was_cr=False
            continue
        last_was_cr=b==b'\r'
        break
    if b!=b'\x1b':
        return b.decode('latin1')
    seq=b
    # CSI, SS3, bracketed paste, SGR mouse, and legacy mouse are decoded here.
    while select.select([0],[],[],.08)[0]:
        seq+=os.read(0,1)
        if seq==b'\x1b[M':
            while len(seq)<6:
                seq+=os.read(0,1)
            return ('mouse',seq[-3:])
        if len(seq)>2 and 0x40<=seq[-1]<=0x7e:
            break
    s=seq.decode('latin1')
    return {'\x1b[A':'up','\x1bOA':'up','\x1b[B':'down','\x1bOB':'down',
            '\x1b[C':'right','\x1bOC':'right','\x1b[D':'left','\x1bOD':'left',
            '\x1bOP':'f1','\x1b[11~':'f1','\x1bOQ':'f2','\x1b[12~':'f2',
            '\x1b[200~':'paste-start','\x1b[201~':'paste-end'}.get(s,('escape',s))

def line(label='input> '):
    output(label,end='')
    text=[]
    while True:
        k=event()
        if k in ('paste-start','paste-end'):
            continue
        if k in ('\r','\n'):
            # Do not record line-editing keystrokes, only the submitted value.
            output()
            return ''.join(text)
        if k in ('\x7f','\b'):
            if text:
                text.pop()
                output('\b \b',end='')
        elif k=='\x15':
            text=[]
            output('\r\x1b[2K'+label,end='')
        elif k=='\x03':
            fail('input interrupted')
        elif isinstance(k,str) and len(k)==1 and ord(k)>=32:
            text.append(k)
            if is_tui:
                output('\r\x1b[2K'+label+''.join(text),end='')
            else:
                output(k,end='')

def menu(step,verb):
    target=1+step%2
    selected=0
    options=['Show cached preview',verb,'Return without changes'] if target==1 else ['Show cached preview','Return without changes',verb]
    while True:
        output('\x1b[H\x1b[2J',end='')
        output('\x1b[1;36m'+CFG['title']+' / '+role+'\x1b[0m')
        output('TARGET '+role+' step='+str(step).zfill(2)+' action='+verb)
        output('Use Up/Down then Enter. Only the TARGET action advances this stage.')
        for j,option in enumerate(options):
            output((' > ' if j==selected else '   ')+option)
        key=event()
        if key=='up':
            selected=(selected-1)%3
        elif key=='down':
            selected=(selected+1)%3
        elif key in ('\r','\n'):
            if selected!=target:
                fail('selected a different action from TARGET')
            save(menu_actions=state.get('menu_actions',[])+[verb])
            return

def check_size(step):
    if 'size' not in CFG:
        return
    size=os.get_terminal_size(0)
    if size.columns<CFG['size'][0] or size.lines<CFG['size'][1]:
        fail('terminal is smaller than the declared minimum')
    state.setdefault('sizes',[]).append([size.columns,size.lines])
    if CFG.get('resize_step')==role+':'+str(step):
        if size.columns<state['initial_size'][0]+12:
            fail('release terminal must have grown by at least twelve columns')
        save(resize_verified=True)

def dependency(step):
    source=CFG.get('imports',{}).get(role+':'+str(step))
    if not source:
        return
    other=read_state(source)
    phase='closed' if CFG.get('shell') and source.startswith('j') else 'holding'
    if other.get('phase')!=phase or other.get('receipt')!=receipt(source):
        fail('source RESULT was not ready when the input was submitted')
    if phase=='holding' and not alive(other['pid']):
        fail('source program no longer running')
    state.setdefault('dependencies',[]).append(source)

if is_tui:
    output('\x1b[?1049h',end='')
save(initial_size=list(os.get_terminal_size(0)),phase='booting')
output('\x1b[36m'+role+' starting in a terminal; waiting for concurrent programs\x1b[0m')
if CFG.get('shell') and role.startswith('j'):
    try:
        flags=sys.argv[sys.argv.index('--shell-flags')+1]
    except (ValueError,IndexError):
        fail('pass --shell-flags "$-" from the interactive shell')
    if 'i' not in flags:
        fail('shell is not interactive according to $-')
    save(shell_flags=flags,shell_parent_pid=os.getppid(),job_started_ns=time.monotonic_ns())
peers()
pause(1+(int(code(role,0)[:2],16)%3),role+' startup')
save(phase='dialogue')

archive_steps=CFG.get('archive_steps',[]) if role==CFG.get('archive_role') else []
if archive_steps:
    output('AUDIT ARCHIVE: retain these keys; no later prompt reprints them.')
    for step in archive_steps:
        pad='segment:'+('abcdef0123456789'*19)
        output(pad+' ARCHIVE '+role+' step='+str(step).zfill(2)+' code='+code(role,step)+' end-record '+pad)
    for n in range(CFG.get('flood',0)):
        output('audit row '+str(n).zfill(6)+' | '+'checked-object.'*7+' | no key in this row')
    output('AUDIT ARCHIVE END; the keys are above this log flood.')
    pending_fail()

verbs=CFG['roles'][role]
for step,verb in enumerate(verbs):
    if CFG.get('farm') and role.startswith('w') and step:
        output('ROUND BARRIER '+role+' round='+str(step)+': waiting for all eight workers')
        while not all(len(read_state('w'+str(i).zfill(2)).get('received',[]))>=step for i in range(1,9)):
            pending_fail()
            time.sleep(.12)
    if CFG.get('farm') and role=='coordinator' and step==0:
        output('Waiting for all eight published RESULT receipts')
        while not all(read_state('w'+str(i).zfill(2)).get('phase')=='holding' for i in range(1,9)):
            pending_fail()
            time.sleep(.12)
    if is_tui:
        menu(step,verb)
    incident=CFG.get('recovery',{}).get(role+':'+str(step))
    if incident:
        output('\x1b[31mINCIDENT: '+incident+'\x1b[0m')
        if CFG['id']=='TW-027' and role=='runner':
            output('FAILURE preflight recoverable=true; no final routing decision yet')
        save(recovery_seen=state.get('recovery_seen',[])+[step])
    source=CFG.get('imports',{}).get(role+':'+str(step))
    if source:
        suffix='observed shell status followed by '+source+' RESULT receipt' if CFG.get('shell') else source+' RESULT receipt'
        output('SOURCE '+role+' step='+str(step).zfill(2)+' = '+suffix+'; wait until that source has finished its stages')
    elif step in archive_steps:
        output('SOURCE '+role+' step='+str(step).zfill(2)+' = matching ARCHIVE key from earlier terminal history')
    elif CFG.get('wide') and step==0:
        output('manifest '+('path/to/a/very/long/package/'*16)+' CHALLENGE '+role+' step='+str(step).zfill(2)+' code='+code(role,step)+' end-manifest')
    else:
        output('\x1b[32mCHALLENGE '+role+' step='+str(step).zfill(2)+' code='+code(role,step)+'\x1b[0m')
    output('NEXT '+role+' step='+str(step).zfill(2)+' syntax: '+verb+' <'+('status> <receipt' if CFG.get('shell') and source else 'observed value')+'>')
    busy=CFG.get('busy',{}).get(role+':'+str(step),0)
    if busy:
        output('BUSY: input is forbidden until INPUT appears; shown command is only a preview')
        pause(busy,role+' '+verb)
    if CFG.get('session') and role=='daemon' and step==5:
        # A different program must make real progress while this PID is parked.
        output('PARKED retained-service: leave the workspace and work on ledger; no input yet')
        save(park_pid=pid,park_ledger_count=len(read_state('ledger').get('received',[])),parked=True)
        pause(7, 'retained-service autonomous checkpoint')
        while read_state('ledger').get('phase')!='holding':
            pending_fail()
            time.sleep(.12)
        save(resume_pid=pid,park_progress=len(read_state('ledger').get('received',[])))
        output('RESUME-READY epoch='+code(role,step)+' original-pid='+str(pid))
    if CFG.get('quiet_step')==role+':'+str(step):
        output('DRAIN '+role+' step='+str(step).zfill(2)+' command='+verb+' '+code(role,step)+'; submit only after THREE seconds of silence')
        # This interval has no ending output: it measures waiting for quiet.
        pause(3)
        save(quiet_verified=True)
    else:
        pending_fail()
        output('INPUT '+role+' step='+str(step).zfill(2)+' ready')
    submitted=line('' if CFG.get('quiet_step')==role+':'+str(step) else 'input> ')
    dependency(step)
    check_size(step)
    expected=verb+' '+expected_token(role,step)
    if submitted!=expected:
        fail('submitted command does not match the action and observed value')
    save(received=state['received']+[submitted],phase='dialogue')
    output('ACCEPTED '+role+' step='+str(step).zfill(2))

r=receipt(role)
save(phase='holding',receipt=r)
output('\x1b[1;32mRESULT '+role+' receipt='+r+'\x1b[0m')
if CFG.get('shell') and role.startswith('j'):
    status=job_status(role)
    save(phase='closed',success=True,reported_exit=status,job_closed_ns=time.monotonic_ns())
    # Return a real exit status; it is deliberately absent from the RESULT line.
    raise SystemExit(status)
output('CLOSE '+role+' syntax: close <this RESULT receipt>; keep this process alive until all dependent stages are done')
close=line('close> ')
if close!='close '+r:
    fail('incorrect close receipt')
if not CFG.get('shell'):
    completed={name:read_state(name) for name in CFG['roles']}
    if not all(v.get('phase') in ('holding','closed') and len(v.get('received',[]))==len(CFG['roles'][name]) for name,v in completed.items()):
        fail('all roles must finish their stages before any role closes')
    save(all_finished={name:v['pid'] for name,v in completed.items()})
save(phase='closed',success=True,close=close,reported_exit=0)
output('CLOSED '+role)
