import os, socket, subprocess, tempfile, time, json, struct
from pathlib import Path
import sys
binary=str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/fux').resolve())
with tempfile.TemporaryDirectory(prefix='fl-',dir='/tmp') as d:
    root=Path(d)
    env=os.environ.copy();env.update(HOME=d,XDG_RUNTIME_DIR=d,XDG_CONFIG_HOME=d+'/config',XDG_STATE_HOME=d+'/state',SHELL='/bin/sh')
    child=subprocess.Popen([binary,'serve','--name','default'],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    peers=[]
    def send(s,v):
        b=json.dumps(v).encode();s.sendall(struct.pack('>I',len(b))+b)
    def exact(s,n):
        b=b''
        while len(b)<n:
            part=s.recv(n-len(b))
            if not part:raise EOFError()
            b+=part
        return b
    def recv(s):return json.loads(exact(s,struct.unpack('>I',exact(s,4))[0]))
    def attach():
        s=socket.socket(socket.AF_UNIX);s.settimeout(5);s.connect(str(root/'fux/default.attach.sock'));peers.append(s)
        send(s,dict(type='hello',rows=24,columns=80));assert recv(s)=={'hello': {}}
        assert 'bindings' in recv(s)
        assert 'state' in recv(s)
        return s
    try:
        for i in range(200):
            if (root/'fux/default.attach.sock').exists():break
            if child.poll() is not None:raise RuntimeError(child.stderr.read().decode())
            time.sleep(.02)
        wrong=socket.socket(socket.AF_UNIX);wrong.settimeout(5);wrong.connect(str(root/'fux/default.attach.sock'));peers.append(wrong)
        send(wrong,dict(type='resize',rows=24,columns=80))
        assert 'hello' in recv(wrong)['error']['message']
        wrong.close()
        one=attach();two=attach()
        send(one,dict(type='input',bytes=list(b'printf "LOCAL_OK_%s\\n" "$$"\n')))
        def marker(s):
            for i in range(20):
                msg=recv(s)
                if 'state' in msg:
                    text=''.join(c.get('text', '') for p in msg['state']['state']['panes'].values() for c in p['cells'])
                    import re
                    match=re.search(r'LOCAL_OK_(\d+)',text)
                    if match:return match.group(1)
            raise AssertionError('missing shell output')
        pid=marker(one);assert marker(two)==pid
        one.close();two.close();time.sleep(.05)
        three=attach();send(three,dict(type='input',bytes=list(b'printf "LOCAL_OK_%s\\n" "$$"\n')));assert marker(three)==pid
        keys=list(root.rglob('*.key'));assert not keys,keys
        assert subprocess.run(['lsof','-nP','-a','-p',str(child.pid),'-i'],capture_output=True).returncode==1
        print('PASS: two viewers, verbatim input, same shell PID after detach/reattach, no keys and no server network sockets')
    finally:
        for s in peers:s.close()
        child.terminate()
        try:child.wait(timeout=10)
        except subprocess.TimeoutExpired:child.kill();child.wait();raise
        stderr=child.stderr.read().decode()
        assert 'Passphrase' not in stderr,stderr
