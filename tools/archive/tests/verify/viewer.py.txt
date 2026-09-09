"""Real-terminal viewer scenarios against an isolated session server.

Covers the interaction contract through the production viewer: fresh launch, immediate popup
without flashing on fast commands, unknown keys, literal prefix, tab creation/rename/choose/close,
splits, focus, repeated resize, confirmed close, history/copy with a private clipboard, shift-drag
selection, workspace creation/switching with a preserved suffix, viewer isolation, tiny screens
and detach that drops trailing commands.
"""
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
    """A tiny terminal model: enough to read painted text and OSC 52 clipboard writes."""

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
                if self.escape in ('\x1b[', '\x1b'):
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
                        for x in range(self.column, self.columns):
                            self.cells[self.row][x] = ' '
                self.escape = ''
                continue
            if char == '\x1b':
                self.escape = char
            elif char == '\r':
                self.column = 0
            elif char == '\n':
                self.row = min(self.rows - 1, self.row + 1)
            elif char == '\b':
                self.column = max(0, self.column - 1)
            elif not unicodedata.category(char).startswith('C'):
                width = 0 if unicodedata.combining(char) else (2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1)
                if width == 0:
                    if self.column > 0:
                        self.cells[self.row][self.column - 1] += char
                else:
                    self.cells[self.row][self.column] = char
                    if width == 2 and self.column + 1 < self.columns:
                        self.cells[self.row][self.column + 1] = ''
                    self.column = min(self.columns - 1, self.column + width)


class Viewer:
    def __init__(self, env, args=(), rows=24, columns=80):
        self.screen = Screen(rows, columns)
        self.status = None
        self.eof = False
        self.raw = b''
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', rows, columns, 0, 0))
            os.execve(binary, [binary, *args], env)

    def send(self, data):
        os.write(self.fd, data)

    def resize(self, rows, columns):
        self.screen.resize(rows, columns)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, columns, 0, 0))
        os.kill(self.pid, signal.SIGWINCH)

    def exited(self):
        if self.status is None:
            child, status = os.waitpid(self.pid, os.WNOHANG)
            if child:
                self.status = os.waitstatus_to_exitcode(status)
        return self.status is not None

    def close(self):
        if not self.exited():
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
            self.status = -signal.SIGKILL
        os.close(self.fd)


with tempfile.TemporaryDirectory(prefix='fview-', dir='/tmp') as directory:
    root = Path(directory)
    config = root / 'config/fux'
    config.mkdir(parents=True)
    (config / 'config.toml').write_text(
        'default-command = { argv = ["/bin/sh", "-c", "printf COPY_TARGET; exec cat"] }\n'
        'clipboard = "write-only"\n')
    env = os.environ.copy()
    env.update(HOME=directory, XDG_RUNTIME_DIR=directory, XDG_CONFIG_HOME=str(root / 'config'),
               XDG_STATE_HOME=str(root / 'state'), TERM='xterm-256color', SHELL='/bin/sh')
    viewers = []

    def pump(seconds=.05):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            active = [viewer for viewer in viewers if not viewer.eof]
            if not active:
                time.sleep(min(.01, max(0, end - time.monotonic())))
                continue
            ready, _, _ = select.select([viewer.fd for viewer in active], [], [], max(0, end - time.monotonic()))
            for viewer in active:
                if viewer.fd not in ready:
                    continue
                try:
                    data = os.read(viewer.fd, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    data = b''
                if data:
                    viewer.raw += data
                    viewer.raw = viewer.raw[-262144:]
                    viewer.screen.feed(data)
                else:
                    viewer.eof = True

    def wait(check, message, timeout=8):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            pump()
            if check():
                return
        raise AssertionError(message + '\n' + '\n--- viewer ---\n'.join(viewer.screen.text() for viewer in viewers))

    def hold(check, message, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            pump()
            assert check(), message + '\n' + '\n--- viewer ---\n'.join(viewer.screen.text() for viewer in viewers)

    def listing(workspace='default'):
        result = subprocess.run([binary, workspace, 'list'], env=env, capture_output=True, timeout=5, check=True)
        return json.loads(result.stdout)['result']['value']['workspaces'][0]

    def tabs(workspace='default'):
        return listing(workspace)['tabs']

    def await_state(check, message, timeout=8):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            state = tabs()
            if check(state):
                return state
            pump(.03)
        raise AssertionError(f'{message}: {tabs()}')

    try:
        # Fresh launch: one workspace, one tab, one pane, no tab strip, no status widgets.
        first = Viewer(env)
        viewers.append(first)
        # The bar is the last row painted; wait for the whole first frame, not just the pane.
        wait(lambda: 'COPY_TARGET' in first.screen.text()
             and first.screen.text().splitlines()[-1].rstrip().endswith('│ 1'),
             'first viewer did not render the pane and the focused pane id in the bar')
        rows = first.screen.text().splitlines()
        assert rows[-1].startswith(' default │ main '), f'bar missing: {rows[-1]!r}'
        assert rows[-1].rstrip().endswith('│ 1'), f'focused pane id missing from the bar: {rows[-1]!r}'
        assert 'split side by side' not in first.screen.text()
        assert not any(ch in first.screen.text() for ch in '┌┐└┘'), 'no pane frames'
        assert all('│' not in row for row in rows[:-1]), 'a single pane has no separators'
        listing_state = listing()
        assert listing_state['name'] == 'default'
        assert len(listing_state['tabs']) == 1 and listing_state['tabs'][0]['name'] == 'main'
        assert len(listing_state['tabs'][0]['panes']) == 1

        # Prefix shows the popup immediately; a fast prefix+command never flashes it.
        first.send(b'\x01')
        wait(lambda: 'split side by side' in first.screen.text(), 'popup missing after prefix')
        # The column hugs the right edge with nothing painted left of it on its rows.
        column_rows = [row for row in first.screen.text().splitlines() if 'split side by side' in row]
        assert column_rows and all(row.index('|') > 40 and row[:40].strip() == '' for row in column_rows), column_rows
        assert 'Panes' in first.screen.text() and 'Session' in first.screen.text(), 'group headings missing'
        first.send(b'\x1b'); pump(.15)
        assert 'split side by side' not in first.screen.text(), 'Esc did not dismiss the popup'
        first.send(b'\x01t')
        await_state(lambda state: len(state) == 2, 'new tab was not created')
        pump(.2)
        assert 'split side by side' not in first.screen.text(), 'fast command flashed the popup'
        wait(lambda: 'tab-2' in first.screen.text().splitlines()[-1] and 'main' in first.screen.text().splitlines()[-1], 'both tabs missing from the bar')

        # Unknown key stays in command mode and reveals the popup; literal prefix is one byte.
        first.send(b'\x01!')
        wait(lambda: 'split side by side' in first.screen.text(), 'unknown key did not reveal the popup')
        first.send(b'\x1b'); pump(.15)
        first.send(b'\x01\x01Q')
        wait(lambda: 'Q' in first.screen.text(), 'literal prefix probe did not echo')

        # New tab then rename in one read targets the new tab, not stale state.
        first.send(b'\x01t\x01,\x15second\r')
        await_state(lambda state: len(state) == 3 and state[2]['name'] == 'second', 'new-tab then rename in one read targeted stale state')
        assert tabs()[0]['name'] == 'main'

        # Choose tab through the popup primitives.
        first.send(b'\x01w')
        wait(lambda: 'Choose tab' in first.screen.text(), 'tab chooser missing')
        first.send(b'k\r')
        await_state(lambda state: state[1]['focused'], 'tab selection not applied')
        # Rename with Unicode, cancel keeps the old name.
        first.send(b'\x01,\x15discarded')
        wait(lambda: 'Rename tab' in first.screen.text(), 'rename prompt missing')
        first.send(b'\x1b'); pump(.15)
        wait(lambda: 'split side by side' in first.screen.text(), 'Esc did not back out to the popup')
        first.send(b'\x1b'); pump(.15)
        assert tabs()[1]['name'] == 'tab-2', 'cancelled rename changed the label'
        first.send(b'\x01,\x15' + 'renamed界'.encode() + b'\r')
        await_state(lambda state: state[1]['name'] == 'renamed界', 'Unicode rename failed')

        # Split, focus, repeated resize kept after finishing.
        first.send(b'\x01|')
        state = await_state(lambda state: len(state[1]['panes']) == 2, 'split failed')
        before = [pane['geometry']['width'] for pane in state[1]['panes']]
        first.send(b'\x01r')
        wait(lambda: 'Resize' in first.screen.text(), 'resize hint missing')
        first.send(b'jj\r')
        await_state(lambda state: [pane['geometry']['width'] for pane in state[1]['panes']] != before, 'repeat resize failed')
        first.send(b'\x01r' + b'jk' * 128 + b'\r\x01,\x15burst-done\r')
        await_state(lambda state: state[1]['name'] == 'burst-done', 'resize burst stalled the viewer', timeout=30)
        first.send(b'\x01h')
        await_state(lambda state: state[1]['panes'][0]['focused'], 'directional focus failed')

        # Confirmed close identifies the target; n cancels; y closes exactly one pane.
        focused = [pane for pane in tabs()[1]['panes'] if pane['focused']][0]['id']
        first.send(b'\x01x')
        wait(lambda: f'Close pane {focused}?' in first.screen.text(), 'close confirmation missing')
        assert len(tabs()[1]['panes']) == 2
        first.send(b'n'); pump(.1); first.send(b'\x1b'); pump(.1)
        assert len(tabs()[1]['panes']) == 2, 'cancelled close killed a pane'
        first.send(b'\x01xy')
        await_state(lambda state: len(state[1]['panes']) == 1, 'confirmed close failed')

        # Close tab with confirmation naming the affected processes.
        first.send(b'\x01c')
        wait(lambda: 'Close tab' in first.screen.text() and '1 pane' in first.screen.text(), 'close tab confirmation missing')
        first.send(b'y')
        await_state(lambda state: len(state) == 2, 'tab close failed')

        # History/copy mode with a private clipboard.
        first.send(b'\x01w')
        wait(lambda: 'Choose tab' in first.screen.text(), 'tab chooser missing (2)')
        first.send(b'k\r')
        await_state(lambda state: state[0]['focused'], 'tab one not selected')
        first.send(b'\x01[')
        wait(lambda: 'Copy ·' in first.screen.text(), 'copy hint bar missing')
        first.send(b'h' * 20 + b'k' * 5 + b' ' + b'l' * 10)
        wait(lambda: 'Copy selection' in first.screen.text(), 'selection-specific hint missing')
        first.send(b'y')
        expected = base64.b64encode(b'COPY_TARGET').decode()
        wait(lambda: expected in first.screen.clipboards, 'clipboard copy of the selected pane text missing')
        # Shift-drag selects locally and copies with y.
        # Panes start at row 1 (one-based); columns 1-11 cover COPY_TARGET.
        first.send(b'\x1b[<4;1;1M\x1b[<36;11;1M\x1b[<4;11;1m')
        wait(lambda: 'Copy selection' in first.screen.text(), 'shift-drag selection missing')
        first.send(b'y')
        wait(lambda: first.screen.clipboards.count(expected) >= 2, 'shift-drag copy failed')

        # Shared separators: one column between side-by-side panes, a row with a junction for a
        # stacked split, no frame anywhere.
        first.send(b'\x01\\')  # backslash is | without Shift
        await_state(lambda state: len(state[0]['panes']) == 2, 'side-by-side split failed')
        wait(lambda: first.screen.text().splitlines()[2].count('│') == 1, 'exactly one separator column expected')
        assert not any(ch in first.screen.text() for ch in '┌┐└┘'), 'no pane frames after a split'
        first.send(b'\x01_')  # underscore is - with Shift
        await_state(lambda state: len(state[0]['panes']) == 3, 'stacked split failed')
        wait(lambda: '├─' in first.screen.text(), 'stacked separator must join the column with ├')
        # A notice appears in the bar and disappears on its own.
        first.send(b'\x01[ ' + b'h' * 3 + b'y')
        wait(lambda: 'Copied' in first.screen.text().splitlines()[-1], 'copy notice missing from the bar')
        wait(lambda: 'Copied' not in first.screen.text().splitlines()[-1], 'copy notice did not expire', timeout=5)
        # A long tab label is truncated in the bar; with three tabs the current one keeps priority.
        subprocess.run([binary, 'default', 'tab', 'new'], env=env, capture_output=True, timeout=5, check=True)
        await_state(lambda state: len(state) == 3, 'third tab missing')
        first.send(b'\x01,\x15' + b'x' * 70 + b'\r')
        wait(lambda: '…' in first.screen.text().splitlines()[-1], 'long label was not truncated')
        bar = first.screen.text().splitlines()[-1]
        assert bar.startswith(' default │ xxxx'), f'current tab lost priority: {bar!r}'
        first.send(b'\x01,\x15main\r')
        await_state(lambda state: state[0]['name'] == 'main', 'rename back failed')
        extra = [tab['id'] for tab in tabs() if tab['name'] not in ('main', 'second')][0]
        subprocess.run([binary, 'default', 'tab', 'close', str(extra)], env=env, capture_output=True, timeout=5, check=True)
        await_state(lambda state: len(state) == 2, 'third tab close failed')
        # Control clients moved the workspace's own selection; put it back on the first tab so
        # later attachments and switches start where the scenarios expect.
        subprocess.run([binary, 'default', 'tab', 'select', '0'], env=env, capture_output=True, timeout=5, check=True)
        first.send(b'\x01xy')
        await_state(lambda state: len(state[0]['panes']) == 2, 'close after split failed')
        first.send(b'\x01Xy')  # X is x with Shift
        await_state(lambda state: len(state[0]['panes']) == 1, 'second close after split failed')

        # A second viewer keeps private menus and input while sharing the workspace.
        second = Viewer(env)
        viewers.append(second)
        wait(lambda: 'COPY_TARGET' in second.screen.text(), 'second viewer did not render')
        first.send(b'\x01')
        wait(lambda: 'split side by side' in first.screen.text(), 'first popup missing')
        hold(lambda: 'split side by side' not in second.screen.text(), 'popup crossed viewers', .3)
        second.send(b'SECOND_INPUT\r')
        wait(lambda: 'SECOND_INPUT' in second.screen.text(), 'second viewer input blocked')
        first.send(b'\x1b'); pump(.15)
        first.send(b'\x01[' + b'h' * 20 + b' ' + b'l' * 3)
        wait(lambda: 'Copy selection' in first.screen.text(), 'first copy selection missing')
        assert 'Copy' not in second.screen.text(), 'copy mode crossed viewers'
        second.send(b'\x01')
        wait(lambda: 'split side by side' in second.screen.text(), 'second popup missing')
        second.send(b'\x1b'); pump(.1)
        second.send(b'MORE_OUTPUT\r')
        wait(lambda: 'MORE_OUTPUT' in second.screen.text(), 'second input while first copies')
        first.send(b'\x1b'); pump(.1); first.send(b'\x1b'); pump(.1); first.send(b'\x1b'); pump(.1)
        assert not second.screen.clipboards, 'clipboard crossed viewers'

        # Tiny screens: paging keeps every command reachable; 1x1 is safe; context returns.
        # Four rows: three above the bar. The column shows two rows plus an indicator and scrolls
        # one row per arrow press; every heading and binding must come around.
        first.resize(4, 18)
        first.send(b'\x01')
        wait(lambda: '|  split' in first.screen.text() and 'more' in first.screen.text(), 'tiny popup missing')
        seen = set()
        for _ in range(40):
            rows = first.screen.text().splitlines()
            seen.update(row.strip() for row in rows[:3] if row.strip() and 'more' not in row)
            first.send(b'\x1b[B'); pump(.03)
        assert len(seen) >= 23, f'tiny column scrolling exposed only {len(seen)} rows: {sorted(seen)}'
        first.resize(1, 1); pump(.3)
        assert not first.exited(), 'one-cell terminal crashed the viewer'
        first.resize(24, 80)
        wait(lambda: 'split side by side' in first.screen.text(), 'resize lost command context')
        first.send(b'\x1b'); pump(.15)
        # Two rows: the bar and one pane row, nothing else.
        first.resize(2, 20); pump(.3)
        assert not first.exited(), 'two-row terminal crashed the viewer'
        wait(lambda: first.screen.text().splitlines()[-1].startswith(' default'), 'bar missing on a two-row terminal')
        rows = first.screen.text().splitlines()
        assert len(rows) == 2 and '│' not in rows[0], 'the first row belongs to the pane'
        first.resize(24, 80); pump(.3)

        # Workspace creation and switching; the suffix after the switch reaches the destination.
        first.send(b'\x01a')
        wait(lambda: 'New workspace' in first.screen.text(), 'new workspace prompt missing')
        first.send(b'other\r')
        wait(lambda: 'other' in first.screen.text().splitlines()[-1] or 'other' in first.screen.text(), 'new workspace did not attach')
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            names = json.loads(subprocess.run([binary, 'workspace', 'list'], env=env, capture_output=True, timeout=5, check=True).stdout)['names']
            if 'other' in names:
                break
            pump(.05)
        assert 'other' in names, names
        first.send(b'\x01s')
        wait(lambda: 'Choose workspace' in first.screen.text(), 'workspace chooser missing')
        first.send(b'\x1b'); pump(.1)
        wait(lambda: 'split side by side' in first.screen.text(), 'chooser cancel did not return to commands')
        first.send(b'\x1b'); pump(.1)
        first.send(b'\x01sk\r\x01,\x15switched\r')
        await_state(lambda state: state[0]['name'] == 'switched', 'workspace switch lost the queued rename')
        assert tabs('other')[0]['name'] == 'main', 'switch suffix reached the wrong workspace'

        # Detach applies preceding input and drops the suffix.
        first.send(b'\x01s')
        wait(lambda: 'Choose workspace' in first.screen.text(), 'workspace chooser missing (2)')
        first.send(b'j\r')
        wait(lambda: 'other' in first.screen.text(), 'switch back to other failed')
        first.send(b'BEFORE_DETACH\r\x01d\x01t')
        wait(first.exited, 'first viewer did not detach')
        assert first.status == 0
        assert len(tabs('other')) == 1, 'a command after detach executed'
        third = Viewer(env, ['other'])
        viewers.append(third)
        wait(lambda: 'BEFORE_DETACH' in third.screen.text(), 'detach dropped preceding pane input')
        # A lone Escape reaches the pane after the disambiguation window, even while frames flow.
        subprocess.run([binary, 'other', 'split', 'horizontal', '--', '/bin/sh', '-c', 'exec cat -v'],
                       env=env, capture_output=True, timeout=5, check=True)
        await_state(lambda state: True, 'listing')
        wait(lambda: len(tabs('other')[0]['panes']) == 2, 'cat -v pane missing')
        third.send(b'\x01l')
        pump(.2)
        third.send(b'\x1b')
        wait(lambda: '^[' in third.screen.text(), 'lone Escape never reached the pane')
        third.send(b'\x1bq')
        wait(lambda: '^[q' in third.screen.text(), 'Escape-prefixed key was not delivered as one sequence')
        third.send(b'\x01d')
        wait(third.exited, 'third viewer did not detach')
        second.send(b'\x01d')
        wait(second.exited, 'second viewer did not detach')
        assert second.status == 0
        print('PASS viewer scenarios: launch, popup, tabs, splits, resize, close, copy, viewers, tiny screens, workspaces, detach')
    finally:
        for viewer in viewers:
            viewer.close()
        for name in ['default', 'other']:
            subprocess.run([binary, 'workspace', 'kill', name], env=env, capture_output=True, timeout=5, check=False)
