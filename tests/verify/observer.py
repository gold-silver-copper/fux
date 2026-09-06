"""Independent zor observation of a real fux pane, plus failure isolation of control clients.

Run as: observer.py FUX_BIN ZOR_BIN. Zor is started explicitly by this harness (as a user would),
consumes fux's documented control surface (`FUXCTL2` preface, `list`, `capture`) and emits OSC
7877 reports on its stdout. Fux never spawns, supervises or parses observers.
"""
import json
import os
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

FUX = str(Path(sys.argv[1]).resolve())
ZOR = str(Path(sys.argv[2]).resolve())


def rpc(path, value, preface=b'FUXCTL2\n'):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(3)
        stream.connect(str(path))
        stream.sendall(preface)
        received = b''
        while len(received) < 8:
            chunk = stream.recv(8 - len(received))
            if not chunk:
                raise EOFError(received)
            received += chunk
        assert received == b'FUXCTL2\n', received
        stream.sendall(json.dumps(value).encode() + b'\n')
        output = b''
        while b'\n' not in output:
            chunk = stream.recv(8192)
            if not chunk:
                raise EOFError(output)
            output += chunk
            assert len(output) <= 1024 * 1024
        return json.loads(output)


def until(function, description, timeout=10):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = function()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError(description)


with tempfile.TemporaryDirectory(prefix='fo-', dir='/tmp') as directory:
    root = Path(directory)
    config = root / 'config/fux'
    config.mkdir(parents=True)
    rules = root / 'rules'
    rules.mkdir()
    (rules / 'test.toml').write_text(
        "id='test'\nprompt_marker='>'\nblock_markers=[]\n"
        "[[rules]]\nid='working'\nstate='working'\nregion='progress'\ncontains=['1:50']\nvisible_working=true\n"
        "[[rules]]\nid='idle'\nstate='idle'\nregion='title'\ncontains=['OBS_IDLE']\nvisible_idle=true\n")
    command = ("stty raw -echo; printf 'READY\\033]9;4;1;50\\007'; dd bs=1 count=1 >/dev/null 2>&1; "
               "printf '\\033[2J\\033[HIDLE\\033]9;4;0;0\\007\\033]2;OBS_IDLE\\007'; sleep 60")
    (config / 'config.toml').write_text('default-command = { argv = ' + json.dumps(['/bin/sh', '-c', command]) + ' }\n')
    env = os.environ.copy()
    env.update(HOME=directory, XDG_RUNTIME_DIR=directory, XDG_STATE_HOME=directory + '/state',
               XDG_CONFIG_HOME=directory + '/config', SHELL='/bin/sh')
    server = subprocess.Popen([FUX, 'serve'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    control = root / 'fux/default.sock'
    observer = None
    try:
        until(control.exists, 'control socket')

        def pane():
            return rpc(control, {'command': 'list', 'id': 1})['result']['value']['workspaces'][0]['tabs'][0]['panes'][0]

        def capture():
            return rpc(control, {'command': 'capture', 'id': 1, 'pane': 1, 'attrs': False, 'scrollback': 0, 'max_bytes': 4096})['result']['value']['text']

        first = pane()
        pid = first['pid']
        until(lambda: 'READY' in capture(), 'pane output visible through capture')
        until(lambda: pane().get('progress') == [1, 50], 'progress report surfaces in the listing')

        # A malformed control client is rejected without disturbing the pane.
        with socket.socket(socket.AF_UNIX) as bad:
            bad.settimeout(3)
            bad.connect(str(control))
            bad.sendall(b'FUXCTL1\n')
            try:
                reply = bad.recv(8)
            except (ConnectionResetError, BrokenPipeError):
                reply = b''
            assert reply in (b'FUXCTL2\n', b''), reply
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(3)
            stream.connect(str(control))
            stream.sendall(b'FUXCTL2\n')
            stream.recv(8)
            stream.sendall(b'{"command":"popup","id":5,"argv":["x"]}\n')
            reply = json.loads(stream.recv(65536).split(b'\n')[0])
            assert reply['status'] == 'failed' and reply['error']['code'] == 'unknown-command', reply
        assert pane()['pid'] == pid

        # Real zor observes the pane through the control socket only.
        observer = subprocess.Popen(
            [ZOR, '--rules', str(rules), '--agent', 'test', 'observe', '--socket', str(control), '--pane', '1', '--pid', str(pid)],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        os.set_blocking(observer.stdout.fileno(), False)
        reports = []

        def read_reports():
            try:
                chunk = observer.stdout.read()
            except (TypeError, BlockingIOError):
                chunk = None
            if chunk:
                reports.extend(line for line in chunk.splitlines() if line)
            return reports

        until(lambda: any('state=working' in line for line in read_reports()), 'zor reported working from fux capture')
        assert all(len(line.encode()) <= 1024 for line in reports)
        before = pane()
        rpc(control, {'command': 'send-keys', 'id': 2, 'pane': 1, 'keys': 'x'})
        until(lambda: 'IDLE' in capture(), 'pane keeps working while observed')
        until(lambda: any('state=idle' in line for line in read_reports()), 'zor reported idle from fux capture and title')
        after = pane()
        assert after['pid'] == pid, 'observation changed the pane process'
        assert after['focused'] == before['focused'], 'observation changed focus'
        assert after['title'] == 'OBS_IDLE'
        # Killing the observer never touches the pane.
        observer.kill()
        observer.wait(timeout=5)
        assert pane()['pid'] == pid
        assert 'IDLE' in capture()
        print('PASS zor observes fux through the control protocol; observer loss and bad clients leave panes untouched', flush=True)
    finally:
        if observer is not None and observer.poll() is None:
            observer.kill()
            observer.wait()
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait()
            raise
        stderr = server.stderr.read().decode()
        assert 'panicked' not in stderr, stderr
