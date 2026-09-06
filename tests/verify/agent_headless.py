"""A full agent-shaped session driven headlessly: no pty anywhere, only the control protocol.

Run as: agent_headless.py FUX_BIN. In a disposable HOME/XDG root it starts a server through
`fux workspace new`, creates a pane with an environment and a headless size, waits for a pattern,
reads the changed rows with `capture --since`, reacts with `send-keys` in key notation, waits for
exit and checks the status, then kills the workspace and confirms the server removed its sockets.
Nothing here touches personal sessions.
"""
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

FUX = str(Path(sys.argv[1]).resolve())


def control(sock_path, *requests, timeout=15):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(timeout)
        stream.connect(str(sock_path))
        stream.sendall(b'FUX\n')
        preface = b''
        while len(preface) < 4:
            chunk = stream.recv(4 - len(preface))
            assert chunk, 'server closed during preface'
            preface += chunk
        assert preface == b'FUX\n', preface
        replies, buffer = [], b''
        for request in requests:
            stream.sendall(json.dumps(request).encode() + b'\n')
            while b'\n' not in buffer:
                chunk = stream.recv(65536)
                assert chunk, 'server closed before a reply'
                buffer += chunk
                assert len(buffer) <= 4 * 1024 * 1024
            line, buffer = buffer.split(b'\n', 1)
            replies.append(json.loads(line))
        return replies


def until(predicate, description, timeout=15):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(0.03)
    raise AssertionError(description)


with tempfile.TemporaryDirectory(prefix='fah-', dir='/tmp') as directory:
    root = Path(directory)
    (root / 'config/fux').mkdir(parents=True)
    (root / 'config/fux/config.toml').write_text('default-command = { argv = ["/bin/sh"] }\n')
    env = os.environ.copy()
    env.update(HOME=directory, XDG_RUNTIME_DIR=directory, XDG_CONFIG_HOME=directory + '/config',
               XDG_STATE_HOME=directory + '/state', SHELL='/bin/sh', TERM='xterm-256color')
    control_sock = root / 'fux/agent.sock'
    manager_sock = root / 'fux/manager.sock'

    created = subprocess.run([FUX, 'workspace', 'new', 'agent'], env=env, capture_output=True, timeout=20)
    assert created.returncode == 0, created.stderr
    until(control_sock.exists, 'control socket appeared')

    info = control(control_sock, {'command': 'info', 'id': 1})[0]
    assert info['status'] == 'completed', info
    limits = info['result']['value']['info']['limits']
    assert limits['panes'] == 128 and limits['viewers'] == 64, limits

    split = control(control_sock, {
        'command': 'split', 'id': 2, 'axis': 'horizontal',
        'argv': ['/bin/sh', '-c', 'printf "%s\\n" "$ROLE"; read x; printf "got:%s\\n" "$x"; exit 7'],
        'env': [['ROLE', 'agent-pane']], 'rows': 12, 'columns': 50,
    })[0]
    assert split['status'] == 'completed', split
    pane = split['result']['value']['pane']

    waited = control(control_sock, {
        'command': 'wait', 'id': 3, 'pane': pane,
        'until': {'kind': 'pattern', 'regex': 'agent-pane'}, 'timeout_ms': 10000,
    })[0]
    assert waited['status'] == 'completed' and waited['result']['value']['fired'] == 'pattern', waited
    seq = waited['result']['value']['seq']

    rows = control(control_sock, {
        'command': 'capture', 'id': 4, 'pane': pane, 'max_bytes': 65536, 'format': 'rows', 'since': 0,
    })[0]
    text = '\n'.join(row['text'] for row in rows['result']['value']['rows'])
    assert 'agent-pane' in text, text
    assert rows['result']['value']['since_applied'] is True, rows

    # React in key notation (single-char tokens plus the named Enter), then wait for exit.
    reacted = control(control_sock, {
        'command': 'send-keys', 'id': 5, 'pane': pane, 'keys': 'o k Enter', 'notation': 'keys',
    })[0]
    assert reacted['status'] == 'completed', reacted
    exited = control(control_sock, {
        'command': 'wait', 'id': 6, 'pane': pane, 'until': {'kind': 'exit'}, 'timeout_ms': 10000,
    })[0]
    assert exited['status'] == 'completed', exited
    assert exited['result']['value']['exit_status'] == 7, exited

    subprocess.run([FUX, 'workspace', 'kill', 'agent'], env=env, capture_output=True, timeout=10)
    until(lambda: not manager_sock.exists(), 'server exited and removed its sockets')
    print('PASS agent_headless: workspace new, info, split env+size, wait pattern, capture --since, '
          'send-keys --keys, wait exit status, kill and cleanup')
