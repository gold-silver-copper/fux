import os, pty, select, time, tempfile, signal, subprocess, fcntl, termios, struct
from pathlib import Path
import sys
binary=str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/fux').resolve())
with tempfile.TemporaryDirectory(prefix='ft-',dir='/tmp') as d:
 env=os.environ.copy();env.update(HOME=d,XDG_RUNTIME_DIR=d,XDG_CONFIG_HOME=d+'/config',XDG_STATE_HOME=d+'/state',SHELL='/bin/sh',TERM='xterm-256color')
 pid,fd=pty.fork()
 if pid==0:
  fcntl.ioctl(0,termios.TIOCSWINSZ,struct.pack("HHHH",24,80,0,0))
  os.execve(binary,[binary],env)
 output=b'';done=False
 try:
  end=time.monotonic()+15
  while time.monotonic()<end:
   if select.select([fd],[],[],.1)[0]:
    try:output+=os.read(fd,65536)
    except OSError:break
   if b'\x1b[?1049h' in output and b'?2026h' in output:break
  assert b'\x1b[?1049h' in output,output
  assert b'Passphrase' not in output,output
  os.write(fd,b'\x01d')
  end=time.monotonic()+5
  while time.monotonic()<end:
   child,status=os.waitpid(pid,os.WNOHANG)
   if child:
    assert os.waitstatus_to_exitcode(status)==0,status
    done=True;break
   if select.select([fd],[],[],.1)[0]:
    try:output+=os.read(fd,65536)
    except OSError:pass
  assert done,output
  assert Path(d+'/fux/default.attach.sock').exists()
  assert not list(Path(d).rglob('*.key'))
  print('PASS: real TTY cold startup and detach, no credential prompts, persistent local server')
 finally:
  if not done:
   os.kill(pid,signal.SIGKILL);os.waitpid(pid,0)
  os.close(fd)
  subprocess.run([binary,'workspace','kill','default'],env=env,capture_output=True,timeout=5)
