"""An older, protocol-incompatible session server: fux explains, never kills it on its own, and
offers the operator an interactive choice. A fake old manager (answering FUXCTL1) stands in for
0.2.x; everything lives in a disposable runtime directory."""
import fcntl
import json
import os
import pty
import re
import select
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

FUX = str(Path(sys.argv[1]).resolve())
ESCAPES = re.compile(rb'\x1b\][^\x07\x1b]*(?:\x07|\x1b\\\\)|\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b[()][A-Za-z0-9]|\x1b[=>]')


def until(function, description, timeout=10):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = function()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError(description)


def fake_old_server(runtime, ready):
    """Child process: a manager socket that speaks FUXCTL1 and exits cleanly on SIGTERM."""
    manager = runtime / 'fux/manager.sock'
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(manager))
    os.chmod(manager, 0o600)
    listener.listen(8)

    def stop(*_):
        try:
            manager.unlink()
        finally:
            os._exit(0)
    signal.signal(signal.SIGTERM, stop)
    os.write(ready, b'1')
    os.close(ready)
    while True:
        try:
            stream, _ = listener.accept()
        except InterruptedError:
            continue
        with stream:
            stream.settimeout(2)
            try:
                stream.recv(8)
                stream.sendall(b'FUXCTL1\n')
            except OSError:
                pass


class Terminal:
    """fux running on a pty, with raw output accumulated for string checks."""

    def __init__(self, env):
        self.raw = b''
        self.status = None
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
            os.execve(FUX, [FUX], env)

    def pump(self, seconds=.05):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([self.fd], [], [], max(0, end - time.monotonic()))
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            self.raw += data

    def text(self):
        """Raw output with escape sequences removed; painted cells stay in order."""
        return ESCAPES.sub(b'', self.raw)

    def wait_for(self, needle, description, timeout=10):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.pump()
            if needle in self.text():
                return
            if self.exited():
                break
        raise AssertionError(f'{description}\n--- output ---\n{self.raw.decode(errors="replace")}')

    def send(self, data):
        os.write(self.fd, data)

    def drain(self):
        """Reads whatever fux has written so far: a pty buffer left full blocks the viewer in its
        frame write, and a blocked viewer cannot see the keys sent afterwards."""
        while True:
            ready, _, _ = select.select([self.fd], [], [], 0)
            if not ready:
                return
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            self.raw += data

    def exited(self):
        self.drain()
        if self.status is None:
            child, status = os.waitpid(self.pid, os.WNOHANG)
            if child:
                self.status = os.waitstatus_to_exitcode(status)
        return self.status is not None

    def close(self):
        if not self.exited():
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        try:
            os.close(self.fd)
        except OSError:
            pass


with tempfile.TemporaryDirectory(prefix='fmig-', dir='/tmp') as directory:
    root = Path(directory)
    (root / 'fux/workspaces').mkdir(parents=True)
    os.chmod(root / 'fux', 0o700)
    os.chmod(root / 'fux/workspaces', 0o700)
    config = root / 'config/fux'
    config.mkdir(parents=True)
    (config / 'config.toml').write_text(
        'default-command = { argv = ["/bin/sh", "-c", "printf NEW_SERVER_PANE; exec cat"] }\n')
    env = os.environ.copy()
    env.update(HOME=directory, XDG_RUNTIME_DIR=directory, XDG_CONFIG_HOME=str(root / 'config'),
               XDG_STATE_HOME=str(root / 'state'), TERM='xterm-256color', SHELL='/bin/sh')

    read_end, write_end = os.pipe()
    old = os.fork()
    if old == 0:
        os.close(read_end)
        fake_old_server(root, write_end)
    os.close(write_end)
    assert os.read(read_end, 1) == b'1'
    os.close(read_end)
    (root / 'fux/workspaces/default.json').write_text(json.dumps({
        'name': 'default', 'pid': old, 'instance_nonce': 'old', 'socket_path': str(root / 'fux/default.attach.sock'),
        'protocol': 2}))

    def old_alive():
        try:
            os.kill(old, 0)
        except ProcessLookupError:
            return False
        return os.waitpid(old, os.WNOHANG)[0] == 0

    terminal = None
    try:
        # Without a terminal: explain and leave the old server alone.
        result = subprocess.run([FUX], env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=20)
        assert result.returncode != 0
        assert b'older protocol' in result.stderr and b'default (pid' in result.stderr, result.stderr
        assert b'XDG_RUNTIME_DIR' in result.stderr
        assert old_alive(), 'non-interactive fux must not touch the old server'

        # Interactive: quit keeps it; a refused confirmation keeps it.
        terminal = Terminal(env)
        terminal.wait_for(b'[k/s/q]', 'dialog prompt missing')
        terminal.send(b'q\n')
        until(terminal.exited, 'fux did not exit after q')
        assert terminal.status != 0 and old_alive()
        terminal.close()
        terminal = Terminal(env)
        terminal.wait_for(b'[k/s/q]', 'dialog prompt missing (2)')
        terminal.send(b'k\n')
        terminal.wait_for(b'Type "stop"', 'confirmation prompt missing')
        terminal.send(b'no\n')
        until(terminal.exited, 'fux did not exit after a refused confirmation')
        assert old_alive(), 'refused confirmation must not stop the old server'
        terminal.close()

        # Interactive: stop, confirm, and the new server takes over.
        terminal = Terminal(env)
        terminal.wait_for(b'[k/s/q]', 'dialog prompt missing (3)')
        terminal.send(b'k\n')
        terminal.wait_for(b'Type "stop"', 'confirmation prompt missing (2)')
        terminal.send(b'stop\n')
        until(lambda: not old_alive(), 'old server was not stopped', timeout=15)
        terminal.wait_for('NEW_SERVER_PANE'.encode(), 'new server pane did not render', timeout=20)
        terminal.send(b'\x01d')
        until(terminal.exited, 'viewer did not detach')
        assert terminal.status == 0, terminal.status
        listing = subprocess.run([FUX, 'default', 'list'], env=env, capture_output=True, timeout=10, check=True)
        assert json.loads(listing.stdout)['result']['value']['workspaces'][0]['name'] == 'default'
        print('PASS incompatible-server dialog: explains, keeps the old server unless confirmed, then replaces it', flush=True)
    finally:
        if terminal is not None:
            terminal.close()
        if old_alive():
            os.kill(old, signal.SIGKILL)
            os.waitpid(old, 0)
        subprocess.run([FUX, 'workspace', 'kill', 'default'], env=env, capture_output=True, timeout=10, check=False)
