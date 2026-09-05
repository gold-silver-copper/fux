"""Isolated observer failure and real sidecar checks; never uses personal sessions."""
import json, os, signal, socket, subprocess, sys, tempfile, time
from pathlib import Path
FUX = str(Path(sys.argv[1] if len(sys.argv)>1 else 'target/debug/fux').resolve())
ZOR = str(Path(sys.argv[2]).resolve()) if len(sys.argv)>2 else None

def rpc(path, value):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(3); stream.connect(str(path))
        stream.sendall(b'FUXCTL1\n')
        preface = b''
        while len(preface) < 8:
            chunk = stream.recv(8 - len(preface))
            if not chunk: raise EOFError(preface)
            preface += chunk
        assert preface == b'FUXCTL1\n', preface
        stream.sendall(json.dumps(value).encode()+b'\n')
        output=b''
        while b'\n' not in output:
            chunk=stream.recv(8192)
            if not chunk:raise EOFError(output)
            output+=chunk
            assert len(output)<=1024*1024
        reply=json.loads(output)
        assert reply['status']=='completed',reply
        return reply['result'].get('value')

def until(function, description, timeout=8):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        value=function()
        if value:return value
        time.sleep(.05)
    raise AssertionError(description)

def run_case(mode):
    with tempfile.TemporaryDirectory(prefix='fo-',dir='/tmp') as directory:
        root=Path(directory);config=root/'config/fux';config.mkdir(parents=True)
        observer=root/'observer';pidfile=root/'observer.pid'
        script='#!/bin/sh\nprintf "%s" "$$" > '+str(pidfile)+'\n'
        if mode=='crash':script+='exit 9\n'
        elif mode=='stall':script+='sleep 60\n'
        elif mode=='malformed':script+='printf "garbage\\n"\nsleep 60\n'
        elif mode=='oversized':script+="printf '%02000d\\n' 0\nsleep 60\n"
        elif mode=='partial':script+="printf 'partial'\nsleep 60\n"
        else:
            rules=root/'rules';rules.mkdir()
            (rules/'test.toml').write_text("id='test'\nprompt_marker='>'\nblock_markers=[]\n[[rules]]\nid='working'\nstate='working'\nregion='progress'\ncontains=['1:50']\nvisible_working=true\n[[rules]]\nid='idle'\nstate='idle'\nregion='title'\ncontains=['OBS_IDLE']\nvisible_idle=true\n")
            import shlex
            script+='exec '+shlex.quote(ZOR)+' --rules '+str(rules)+' --agent test "$@"\n'
        observer.write_text(script);observer.chmod(0o700)
        command="stty raw -echo; printf 'READY\\033]9;4;1;50\\007'; dd bs=1 count=1 >/dev/null 2>&1; printf '\\033[2J\\033[HIDLE\\033]9;4;0;0\\007\\033]2;OBS_IDLE\\007'; sleep 60"
        (config/'config.toml').write_text('zor-path = '+json.dumps(str(observer))+'\ndefault-command = { argv = '+json.dumps(['/bin/sh','-c',command])+' }\n')
        env=os.environ.copy();env.update(HOME=directory,XDG_RUNTIME_DIR=directory,XDG_STATE_HOME=directory+'/state',XDG_CONFIG_HOME=directory+'/config',SHELL='/bin/sh')
        server=subprocess.Popen([FUX,'serve'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        control=root/'fux/default.sock'
        observer_pid=None
        try:
            until(lambda:control.exists(),'control socket')
            until(lambda:pidfile.exists(),'observer launch')
            observer_pid=int(pidfile.read_text())
            def pane():return rpc(control,{'command':'list','id':1})['workspaces'][0]['tabs'][0]['panes'][0]
            original_pid=pane()['pid']
            def capture():return rpc(control,{'command':'capture','id':1,'pane':1,'attrs':False,'scrollback':0,'max_bytes':4096})['text']
            until(lambda:'READY' in capture(),'pane runs independently')
            if mode=='real':until(lambda:pane()['state']=='working','real observer working report')
            rpc(control,{'command':'send-keys','id':2,'pane':1,'keys':'x'})
            until(lambda:'IDLE' in capture(),'pane continues after observer failure')
            assert pane()['pid']==original_pid
            if mode=='real':
                until(lambda:pane()['state']=='idle','real observer idle report')
                os.kill(observer_pid,signal.SIGKILL)
                until(lambda:pane()['state']=='none','observer loss clears stale state')
                assert 'IDLE' in capture()
                assert pane()['pid']==original_pid
            print('PASS observer case:',mode,flush=True)
        finally:
            server.terminate()
            try:server.wait(timeout=10)
            except subprocess.TimeoutExpired:server.kill();server.wait();raise
            stderr=server.stderr.read().decode()
            assert 'Passphrase' not in stderr,stderr
            if observer_pid:
                try:os.kill(observer_pid,0)
                except ProcessLookupError:pass
                else:raise AssertionError('observer was not reaped')

for mode in ['crash','stall','malformed','oversized','partial']+(['real'] if ZOR else []):run_case(mode)
