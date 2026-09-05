"""Isolated real-terminal viewer ownership and resize scenarios."""
import base64
import codecs
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

binary = str(Path(sys.argv[1]).resolve())

class Screen:
    def __init__(self, rows, columns):
        self.row = self.column = 0
        self.escape = ''
        self.clipboards = []
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.resize(rows, columns)

    def resize(self, rows, columns):
        self.rows, self.columns = rows, columns
        self.cells = [[' '] * columns for _ in range(rows)]
        self.row = min(self.row, rows - 1)
        self.column = min(self.column, columns - 1)

    def text(self):
        return '\n'.join(''.join(row) for row in self.cells)

    def feed(self, data):
        for char in self.decoder.decode(data):
            if self.escape:
                self.escape += char
                assert len(self.escape) < 2 * 1024 * 1024, 'unbounded terminal escape'
                if self.escape.startswith('\x1b]'):
                    if char == '\a' or self.escape.endswith('\x1b\\'):
                        if self.escape.startswith('\x1b]52;c;'):
                            self.clipboards.append(self.escape[7:].rstrip('\a').removesuffix('\x1b\\'))
                        self.escape = ''
                    continue
                if self.escape == '\x1b[' or self.escape == '\x1b':
                    continue
                if self.escape.startswith('\x1b['):
                    if not ('@' <= char <= '~'):
                        continue
                    values = self.escape[2:-1]
                    args = [int(value or 0) for value in values.split(';')] if all(c.isdigit() or c == ';' for c in values) else []
                    if char in 'Hf':
                        self.row = min(self.rows - 1, max(0, (args[0] if args else 1) - 1))
                        self.column = min(self.columns - 1, max(0, (args[1] if len(args) > 1 else 1) - 1))
                    elif char == 'J' and args and args[0] in (2, 3):
                        self.cells = [[' '] * self.columns for _ in range(self.rows)]
                    elif char == 'K':
                        for x in range(self.column, self.columns): self.cells[self.row][x] = ' '
                self.escape = ''
                continue
            if char == '\x1b': self.escape = char
            elif char == '\r': self.column = 0
            elif char == '\n': self.row = min(self.rows - 1, self.row + 1)
            elif char == '\b': self.column = max(0, self.column - 1)
            elif not unicodedata.category(char).startswith('C'):
                width = 0 if unicodedata.combining(char) else (2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1)
                if width == 0:
                    if self.column > 0: self.cells[self.row][self.column - 1] += char
                else:
                    self.cells[self.row][self.column] = char
                    if width == 2 and self.column + 1 < self.columns: self.cells[self.row][self.column + 1] = ''
                    self.column = min(self.columns - 1, self.column + width)

class Viewer:
    def __init__(self, env):
        self.screen = Screen(24, 80)
        self.status = None
        self.eof = False
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
            os.execve(binary, [binary, 'default'], env)

    def send(self, data): os.write(self.fd, data)

    def resize(self, rows, columns):
        self.screen.resize(rows, columns)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, columns, 0, 0))
        os.kill(self.pid, signal.SIGWINCH)

    def exited(self):
        if self.status is None:
            child, status = os.waitpid(self.pid, os.WNOHANG)
            if child: self.status = os.waitstatus_to_exitcode(status)
        return self.status is not None

    def close(self):
        if not self.exited():
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
            self.status = -signal.SIGKILL
        os.close(self.fd)

with tempfile.TemporaryDirectory(prefix='fcv-', dir='/tmp') as directory:
    root = Path(directory)
    config = root / 'config/fux'
    config.mkdir(parents=True)
    (config / 'config.toml').write_text('default-command = { argv = ["/bin/sh", "-c", "printf LOCAL_VIEW; exec cat"] }\nclipboard = "write-only"\n')
    env = os.environ.copy()
    env.update(HOME=directory, XDG_RUNTIME_DIR=directory, XDG_CONFIG_HOME=str(root / 'config'), XDG_STATE_HOME=str(root / 'state'), TERM='xterm-256color', SHELL='/bin/sh')
    viewers = []

    def pump(seconds=.05):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            active = [viewer for viewer in viewers if not viewer.eof]
            ready, _, _ = select.select([viewer.fd for viewer in active], [], [], max(0, end - time.monotonic()))
            for viewer in active:
                if viewer.fd not in ready: continue
                try: data = os.read(viewer.fd, 65536)
                except OSError as error:
                    if error.errno != errno.EIO: raise
                    data = b''
                if data: viewer.screen.feed(data)
                else: viewer.eof = True

    def wait(check, message, timeout=5):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            pump()
            if check(): return
        raise AssertionError(message + '\n' + '\n--- viewer ---\n'.join(viewer.screen.text() for viewer in viewers))

    def tabs():
        result = subprocess.run([binary, 'default', 'list'], env=env, capture_output=True, timeout=3, check=True)
        return json.loads(result.stdout)['result']['value']['workspaces'][0]['tabs']

    try:
        first = Viewer(env); viewers.append(first)
        wait(lambda: 'LOCAL_VIEW' in first.screen.text(), 'first viewer did not render')
        second = Viewer(env); viewers.append(second)
        wait(lambda: 'LOCAL_VIEW' in second.screen.text(), 'second viewer did not render')
        first.send(b'\x01')
        wait(lambda: 'Commands' in first.screen.text(), 'silent-pane help did not repaint')
        assert 'Commands' not in second.screen.text(), 'help crossed viewers'
        second.send(b'SECOND_INPUT\r')
        wait(lambda: 'SECOND_INPUT' in second.screen.text(), 'other viewer input blocked by help')
        assert 'Commands' in first.screen.text()
        first.send(b'\x1b'); pump(.15)
        first.send(b'\x01,' + b'\x15uncommitted')
        wait(lambda: 'Rename tab' in first.screen.text(), 'rename dialog missing')
        assert 'Rename tab' not in second.screen.text()
        assert tabs()[0]['name'] == 'main'
        first.send(b'\x1b'); pump(.1); first.send(b'\x1b'); pump(.1)
        first.send(b'\x01[' + b'k' * 100 + b'h' * 100 + b' ' + b'l' * 9)
        wait(lambda: 'Copy selection' in first.screen.text(), 'copy selection missing')
        assert 'Copy selection' not in second.screen.text()
        assert not tabs()[0]['panes'][0]['copy']['active']
        second.send(b'\x01')
        wait(lambda: 'Commands' in second.screen.text(), 'independent second prefix mode missing')
        assert 'Copy selection' in first.screen.text()
        second.send(b'\x1b'); pump(.1)
        second.send(b'COPY_OTHER\r')
        wait(lambda: 'COPY_OTHER' in second.screen.text(), 'copy hijacked second viewer input')
        first.send(b'y')
        expected = base64.b64encode(b'LOCAL_VIEW').decode()
        wait(lambda: expected in first.screen.clipboards, 'first viewer copy bytes changed under shared output')
        assert not second.screen.clipboards, 'clipboard crossed viewers'
        first.send(b'\x01[')
        wait(lambda: 'Copy ' in first.screen.text(), 'copy reentry missing')
        first.resize(3, 18)
        wait(lambda: first.screen.text().splitlines()[-1].startswith('Copy'), 'tiny copy hints missing')
        first.send(b'q\x01')
        wait(lambda: 'Commands' in first.screen.text(), 'tiny command help missing')
        pages = set()
        for _ in range(36):
            pages.add(first.screen.text().splitlines()[1])
            first.send(b'\x1b[B'); pump(.03)
        assert len(pages) > 10, 'tiny help pagination stopped exposing commands'
        first.resize(1, 1); pump(.2)
        assert not first.exited(), 'one-cell terminal crashed viewer'
        first.resize(12, 48)
        wait(lambda: 'Commands' in first.screen.text(), 'resize lost command context')
        first.send(b'\x1b'); pump(.1); first.send(b'BEFORE_DETACH\r\x01d\x01t')
        wait(first.exited, 'first viewer did not detach')
        assert first.status == 0
        assert len(tabs()) == 1, 'bytes after detach executed another command'
        wait(lambda: 'BEFORE_DETACH' in second.screen.text(), 'detach dropped preceding pane input')
        second.send(b'SURVIVOR\r')
        wait(lambda: 'SURVIVOR' in second.screen.text(), 'detach killed persistent pane')
        assert 'Commands' not in second.screen.text(), 'detached viewer left stale help'
        second.send(b'\x01d')
        wait(second.exited, 'second viewer did not detach')
        assert second.status == 0
        print('PASS same-workspace viewer isolation, private clipboard, resize and tiny help')
    finally:
        for viewer in viewers: viewer.close()
        subprocess.run([binary, 'workspace', 'kill', 'default'], env=env, capture_output=True, timeout=5, check=False)
