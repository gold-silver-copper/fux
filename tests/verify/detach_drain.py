"""A real viewer sends every accepted pane byte before `detach`, waits for the server's exit
frame, and never sends bytes that followed the detach key.

The script plays the server side over a private socket so it can observe the exact frame order
the production viewer emits. A real session server supplies a genuine state frame to paint.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

binary = str(Path(sys.argv[1]).resolve())


def send(peer, message):
    data = json.dumps(message).encode()
    peer.sendall(struct.pack('>I', len(data)) + data)


def receive(peer):
    def exact(size):
        data = b''
        while len(data) < size:
            chunk = peer.recv(size - len(data))
            assert chunk, 'viewer closed before the expected frame'
            data += chunk
        return data
    size = struct.unpack('>I', exact(4))[0]
    assert size <= 16 * 1024 * 1024
    return json.loads(exact(size))


with tempfile.TemporaryDirectory(prefix='fdrain-', dir='/tmp') as directory:
    root = Path(directory)
    env = os.environ.copy()
    env.update(HOME=directory, XDG_RUNTIME_DIR=directory,
               XDG_CONFIG_HOME=directory + '/config', XDG_STATE_HOME=directory + '/state',
               SHELL='/bin/sh', TERM='xterm-256color')
    server = subprocess.Popen([binary, 'serve', '--name', 'default'], env=env,
                              stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    child = None
    master, slave = pty.openpty()
    stopped = threading.Event()

    def drain_terminal():
        while not stopped.is_set():
            if select.select([master], [], [], .05)[0]:
                try:
                    if not os.read(master, 65536):
                        return
                except OSError:
                    return
    drain = threading.Thread(target=drain_terminal)
    drain.start()
    try:
        source = root / 'fux/default.attach.sock'
        deadline = time.monotonic() + 5
        while not source.exists():
            assert server.poll() is None, 'isolated server exited'
            assert time.monotonic() < deadline, 'isolated server did not start'
            time.sleep(.02)
        with socket.socket(socket.AF_UNIX) as peer:
            peer.settimeout(5)
            peer.connect(str(source))
            send(peer, dict(type='hello', version=3, rows=12, columns=48))
            assert receive(peer) == {'hello': {'version': 3}}
            state = receive(peer)
            assert 'state' in state
        with socket.socket(socket.AF_UNIX) as listener:
            path = root / 'probe.sock'
            listener.bind(str(path))
            path.chmod(0o600)
            listener.listen(1)
            listener.settimeout(5)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 12, 48, 0, 0))
            child = subprocess.Popen([binary, 'attach', '--socket', str(path)], env=env,
                                     stdin=slave, stdout=slave, stderr=slave)
            peer, _ = listener.accept()
            with peer:
                peer.settimeout(5)
                assert receive(peer)['type'] == 'hello'
                send(peer, {'hello': {'version': 3}})
                assert receive(peer)['type'] == 'resize'
                send(peer, state)
                os.write(master, b'PREVIOUS_CHUNK')
                data = b''
                while data != b'PREVIOUS_CHUNK':
                    message = receive(peer)
                    assert message['type'] == 'input', message
                    data += bytes(message['bytes'])
                # A separate read: detach followed by a command that must never be sent.
                os.write(master, b'\x01d\x01t')
                message = receive(peer)
                assert message == {'type': 'detach'}, message
                time.sleep(.2)
                assert child.poll() is None, 'viewer exited before the server acknowledged detach'
                peer.setblocking(False)
                try:
                    extra = peer.recv(1)
                except BlockingIOError:
                    extra = None
                assert extra is None, f'trailing bytes after detach: {extra!r}'
                peer.setblocking(True)
                send(peer, {'exited': {'code': None}})
                assert child.wait(timeout=5) == 0
                assert peer.recv(1) == b'', 'viewer kept the connection after exit'
        print('PASS separate-read detach sends preceding input, waits for exit and drops the suffix')
    finally:
        if child is not None and child.poll() is None:
            child.kill(); child.wait()
        stopped.set(); drain.join(timeout=1)
        os.close(master); os.close(slave)
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill(); server.wait()
            raise
