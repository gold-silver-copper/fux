import os, sys, time, json, hashlib, select, termios, tty, re, signal
from pathlib import Path

def code(task, role, stage):
    return hashlib.sha256((task+'|'+role+'|'+str(stage)+'|terminal-lab-26').encode()).hexdigest()[:10].upper()

class Terminal:
    def __init__(self, work, role):
        self.work=Path(work); self.role=role
        self.fx=self.work/'.fx'; self.fx.mkdir(parents=True,exist_ok=True)
        self.events=[]; self.bad=False; self.buf=b''; self.after_cr=False
        self.record={'tty':bool(sys.stdin.isatty() and sys.stdout.isatty()),'pid':os.getpid(),'events':[], 'success':False}
        self.atomic(self.fx/(role+'.pid'),str(os.getpid()))
        self.save()
        if not self.record['tty']:
            raise SystemExit('A terminal is required on stdin and stdout')
        self.old=termios.tcgetattr(0); tty.setraw(0)
        import atexit
        atexit.register(self.restore)
    def restore(self):
        if hasattr(self,'old'):
            try: termios.tcsetattr(0,termios.TCSANOW,self.old)
            except termios.error: pass
    def atomic(self,p,s):
        temp=p.with_name(p.name+'.'+str(os.getpid())+'.tmp')
        temp.write_text(s); os.replace(temp,p)
    def save(self):
        self.record.update(events=self.events,invalid=self.bad)
        self.atomic(self.fx/(self.role+'.json'),json.dumps(self.record,sort_keys=True))
    def out(self,s):
        sys.stdout.write(s.replace('\n','\r\n')); sys.stdout.flush()
    def neutral(self,data):
        data=data.replace(b'\x1b[200~',b'').replace(b'\x1b[201~',b'')
        data=re.sub(rb'\x1b\[<\d+;\d+;\d+m',b'',data)
        data=re.sub(rb'\x1b\[M[\x23\x63]..',b'',data)
        if self.after_cr and data.startswith(b'\n'): data=data[1:]; self.after_cr=False
        return data
    def busy(self,seconds,label='working'):
        if self.neutral(self.buf): self.bad=True; self.save()
        self.buf=b''
        self.out('\x1b[33mBUSY '+label+'; do not type until READY\x1b[0m\n')
        end=time.monotonic()+seconds; tick=0
        while time.monotonic()<end:
            if select.select([0],[],[],min(.17,max(0,end-time.monotonic())))[0]:
                data=self.neutral(os.read(0,65536))
                if data:
                    self.bad=True; self.save()
                    self.out('\nEARLY INPUT: this attempt cannot complete; restart this program.\n')
            self.out('\r\x1b[2K'+label+' '+('|/-\\'[tick%4])); tick+=1
        self.out('\r\x1b[2K')
    def byte(self):
        if not self.buf: self.buf=os.read(0,4096)
        if not self.buf: raise EOFError()
        b=self.buf[:1]; self.buf=self.buf[1:]; return b
    def key(self):
        b=self.byte()
        if b==b'\n' and self.after_cr:
            self.after_cr=False; return self.key()
        self.after_cr=(b==b'\r')
        if b in (b'\r',b'\n'): return 'ENTER'
        if b in (b'\x7f',b'\x08'): return 'BACKSPACE'
        if b==b'\x15': return 'CLEAR'
        if b==b'\x03': return 'CTRL_C'
        if b!=b'\x1b': return b.decode('utf-8','replace')
        seq=b
        # Escape sequences are read to their grammatical end, not split by timing.
        nxt=self.byte(); seq+=nxt
        if nxt not in (b'[',b'O'): return 'ALT_'+nxt.decode('ascii','replace')
        c=self.byte(); seq+=c
        if nxt==b'[' and c==b'M':
            raw=self.byte()+self.byte()+self.byte()
            return ('MOUSE',raw[1]-32,raw[2]-32,(raw[0]-32)&3)
        while not (64<=seq[-1]<=126): seq+=self.byte()
        s=seq.decode('ascii','replace')
        mapping={'A':'UP','B':'DOWN','C':'RIGHT','D':'LEFT','P':'F1','Q':'F2','R':'F3','S':'F4'}
        if s[-1] in mapping and (s[1]=='O' or s[2:-1] in ('','1','1;2','1;3','1;5')): return mapping[s[-1]]
        if s in ('\x1b[200~','\x1b[201~'): return 'PASTE'
        nums={'11':'F1','12':'F2','13':'F3','14':'F4','15':'F5','17':'F6','18':'F7','19':'F8','20':'F9','21':'F10','23':'F11','24':'F12','5':'PAGEUP','6':'PAGEDOWN','1':'HOME','4':'END'}
        if s.endswith('~'): return nums.get(s[2:-1].split(';')[0],'UNKNOWN')
        m=re.fullmatch(r'\x1b\[<(\d+);(\d+);(\d+)([Mm])',s)
        if m:
            return ('MOUSE',int(m[2]),int(m[3]),int(m[1])&3) if m[4]=='M' else 'RELEASE'
        return 'UNKNOWN'
    def line(self,prompt='> '):
        self.out(prompt); chars=[]
        while True:
            k=self.key()
            if k=='ENTER': self.out('\n'); return ''.join(chars)
            if k=='BACKSPACE':
                if chars: chars.pop(); self.out('\b \b')
            elif k=='CLEAR':
                self.out('\r\x1b[2K'+prompt); chars=[]
            elif isinstance(k,str) and len(k)==1 and k>=' ':
                chars.append(k); self.out(k)
    def accept(self,expected):
        while True:
            got=self.line()
            if got==expected:
                self.events.append(got); self.save(); return
            self.bad=True; self.save()
            self.out('REJECTED: wrong submission invalidated this attempt; restart this program.\n')
    def challenge(self,task,stage,verb='ack',delay=.23):
        token=code(task,self.role,stage)
        self.busy(delay,'stage '+str(stage))
        self.out('OLD token='+code(task,self.role,stage-1)+' (expired)\n')
        self.out('READY role='+self.role+' stage='+str(stage)+' token='+token+'\n')
        self.out('Submit exactly: '+verb+' '+token+'\n')
        self.accept(verb+' '+token)
        return token
    def size(self,cols,rows):
        while True:
            sz=os.get_terminal_size(0)
            if sz.columns>=cols and sz.lines>=rows:
                self.out('SIZE OK '+str(sz.columns)+'x'+str(sz.lines)+'\n'); return
            self.out('RESIZE REQUIRED: at least '+str(cols)+' columns and '+str(rows)+' rows.\n')
            # No input is requested until the terminal is large enough.
            if select.select([0],[],[],.2)[0]:
                if self.neutral(os.read(0,4096)): self.bad=True; self.save()
    def wait_live(self,names):
        self.out('Startup requires concurrent terminal programs: '+', '.join(names)+'\n')
        while True:
            ready=True
            for name in names:
                try:
                    r=json.loads((self.fx/(name+'.json')).read_text())
                    if not r.get('tty') or r.get('success'): ready=False; break
                    os.kill(r['pid'],0)
                except (OSError,ValueError,KeyError): ready=False; break
            if ready: return
            self.busy(.2,'startup barrier')
    def finish(self,result):
        self.record.update(success=not self.bad,result=result); self.save()
        for key,value in result.items(): self.out('RESULT '+key+'='+str(value)+'\n')
        self.out('FINISHED '+self.role+'\n')

if __name__ == '__main__':
    if not (sys.stdin.isatty() and sys.stdout.isatty()):
        raise SystemExit('A terminal is required on stdin and stdout')
    raise SystemExit('This is a support module. Start console.py instead.')
