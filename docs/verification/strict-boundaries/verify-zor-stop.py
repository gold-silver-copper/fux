#!/usr/bin/env python3
"""Prove an already-connected fux attachment survives stopping the zor service.

Usage: python3 verify-zor-stop.py /absolute/fux /absolute/zor
Uses isolated XDG directories, owns/cleans only its spawned services, and requires both binaries.
"""
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid


def exact(sock, length):
    data = bytearray()
    while len(data) < length:
        chunk = sock.recv(length - len(data))
        if not chunk:
            raise RuntimeError('attachment closed before frame completed')
        data.extend(chunk)
    return bytes(data)


def frame(sock):
    length = struct.unpack('!I', exact(sock, 4))[0]
    assert length <= 16 * 1024 * 1024, 'oversized attachment frame'
    return json.loads(exact(sock, length))


def send(sock, value):
    payload = json.dumps(value).encode()
    sock.sendall(struct.pack('!I', len(payload)) + payload)


def wait_text(sock, marker):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        sock.settimeout(max(.001, deadline - time.monotonic()))
        value = frame(sock)
        panes = value.get('state', {}).get('state', {}).get('panes', {})
        text = ''.join(cell.get('text', '') for pane in panes.values()
                       for cell in pane.get('cells', []))
        if marker in text:
            return
    raise AssertionError('attachment did not render ' + marker)


def main():
    fux, zor = [str(Path(p).resolve(strict=True)) for p in sys.argv[1:]]
    children = []
    with tempfile.TemporaryDirectory(prefix='bz-', dir='/tmp') as directory:
        root = Path(directory)
        env = dict(os.environ, XDG_RUNTIME_DIR=directory,
                   XDG_CONFIG_HOME=str(root / 'config'), XDG_STATE_HOME=str(root / 'state'),
                   SHELL='/bin/sh', TERM='xterm-256color')
        try:
            for binary in (fux, zor):
                children.append(subprocess.Popen([binary, 'serve'], cwd=root, env=env,
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
            deadline = time.monotonic() + 8
            while not all((root / p).exists() for p in ('fux/default.attach.sock', 'zor/control.sock')):
                assert all(p.poll() is None for p in children), 'service exited during startup'
                assert time.monotonic() < deadline, 'service startup timed out'
                time.sleep(.02)
            with socket.socket(socket.AF_UNIX) as terminal:
                terminal.settimeout(10)
                terminal.connect(str(root / 'fux/default.attach.sock'))
                send(terminal, {'type': 'hello', 'rows': 24, 'columns': 100})
                assert frame(terminal) == {'hello': {}}, 'attachment negotiation failed'
                token = uuid.uuid4().hex
                command = f"FUX_BOUNDARY_TOKEN={token}; printf 'BEFORE_%s\\n' \"$FUX_BOUNDARY_TOKEN\"\n"
                send(terminal, {'type': 'input', 'bytes': list(command.encode())})
                wait_text(terminal, 'BEFORE_' + token)
                children[1].terminate()
                children[1].wait(timeout=8)
                assert children[0].poll() is None, 'stopping zor stopped fux'
                command = "printf 'AFTER_%s\\n' \"$FUX_BOUNDARY_TOKEN\"\n"
                send(terminal, {'type': 'input', 'bytes': list(command.encode())})
                wait_text(terminal, 'AFTER_' + token)
            print('PASS: stopping zor serve preserves the existing local attachment and shell state')
        finally:
            for process in reversed(children):
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


if __name__ == '__main__':
    main()
