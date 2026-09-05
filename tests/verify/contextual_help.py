"""Real viewer prefix timing against a silent pane, with isolated runtime/session ownership."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

binary = str(Path(sys.argv[1]).resolve())
def check(mode):
    with tempfile.TemporaryDirectory(prefix='fhelp-', dir='/tmp') as directory:
        root = Path(directory)
        config = root / 'config/fux'
        config.mkdir(parents=True)
        document = 'default-command = { argv = ["/bin/sh", "-c", "printf COPY_TARGET; sleep 120"] }\n'
        document += "clipboard = 'write-only'\n"
        prefix = b'\x01'
        if mode == 'immediate':
            document += "prefix = 'C-b'\n[hints]\ndelay-ms = 0\n[bindings.m]\nbuiltin = 'zoom'\n"
            prefix = b'\x02'
        elif mode == 'hidden':
            document += '[hints]\nautomatic = false\n'
        document += '[bindings.e]\nexternal = { argv = ["/usr/bin/touch", ' + json.dumps(str(root / 'external-ran')) + '] }\n'
        (config / 'config.toml').write_text(document)
        env = os.environ.copy()
        env.update(HOME=directory, XDG_RUNTIME_DIR=directory,
                   XDG_CONFIG_HOME=str(root / 'config'), XDG_STATE_HOME=str(root / 'state'),
                   SHELL='/bin/sh', TERM='xterm-256color')
        pid, master = pty.fork()
        if pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
            os.execve(binary, [binary], env)
        exited = False
        def read_for(seconds):
            result = b''
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if select.select([master], [], [], max(0, end - time.monotonic()))[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not chunk:
                        break
                    result += chunk
                    assert len(result) <= 1024 * 1024
            return re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', result)
        try:
            startup = b''
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                startup += read_for(.05)
                if (root / 'fux/default.attach.sock').exists() and b'[main]' in startup:
                    break
            assert b'[main]' in startup, 'initial tab frame missing'
            read_for(.1)
            os.write(master, prefix)
            if mode == 'default':
                assert b'Commands' not in read_for(.08), 'hints flashed before delay'
                assert b'Commands' in read_for(.5), 'idle pane did not repaint delayed hints'
            elif mode == 'immediate':
                output = read_for(.15)
                assert b'Commands (C-b)' in output, 'zero-delay custom prefix hints missing'
                assert b'm  toggle zoom' in output, 'custom command label missing'
            else:
                assert b'Commands' not in read_for(.35), 'disabled automatic hints appeared'
                os.write(master, b'?')
                assert b'Commands' in read_for(.15), 'explicit help was disabled' 
            os.write(master, b'\x1b')
            read_for(.1)
            os.write(master, prefix + b'e')
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline and not (root / 'external-ran').exists():
                read_for(.03)
            assert (root / 'external-ran').exists(), 'explicit external binding did not execute'
            os.write(master, prefix + b'z')
            assert b'Commands' not in read_for(.3), 'fast command flashed hints'
            os.write(master, prefix + b'!')
            assert b'Commands' in read_for(.15), 'unknown command did not reveal hints immediately'
            os.write(master, b'\x1b')
            read_for(.1)
            if mode == 'default':
                def listing():
                    result = subprocess.run([binary, 'list'], env=env, capture_output=True, timeout=3, check=True)
                    return json.loads(result.stdout)['result']['value']['workspaces'][0]['tabs']
                def await_state(check, description, timeout=5):
                    output_tail = b''
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        tabs = listing()
                        if check(tabs): return tabs
                        output_tail = (output_tail + read_for(.03))[-2000:]
                    raise AssertionError(f"{description}: {tabs}; viewer: {output_tail!r}")
                os.write(master, prefix + b'r')
                assert b'No split to adjust' in read_for(.2), 'unavailable resize lacked contextual feedback'
                assert len(listing()[0]['panes']) == 1, 'unavailable resize mutated workspace'
                os.write(master, b'\x1b')
                read_for(.1)
                os.write(master, prefix + b't' + prefix + b',\x15second\r')
                await_state(lambda tabs: len(tabs) == 2 and tabs[1]['name'] == 'second',
                            'new-tab then rename in one read targeted stale state')
                assert listing()[0]['name'] == 'main', 'new-tab rename changed the old tab'
                read_for(.1)
                os.write(master, prefix + b'w')
                assert b'Choose tab' in read_for(.15), 'tab picker missing'
                os.write(master, b'k\r')
                await_state(lambda tabs: tabs[0]['focused'], 'tab selection not applied')
                os.write(master, prefix + b',\x15discarded')
                assert b'Rename tab' in read_for(.15), 'rename prompt missing'
                os.write(master, b'\x1b')
                read_for(.1)
                assert listing()[0]['name'] == 'main', 'rename cancellation changed name'
                os.write(master, b'\x1b')
                read_for(.1)
                os.write(master, prefix + b',\x15' + 'renamed界'.encode() + b'\r')
                await_state(lambda tabs: tabs[0]['name'] == 'renamed界', 'Unicode rename failed')
                os.write(master, prefix + b'|')
                tabs = await_state(lambda tabs: len(tabs[0]['panes']) == 2, 'split failed')
                before = [pane['geometry']['width'] for pane in tabs[0]['panes']]
                os.write(master, prefix + b'rjj\r')
                await_state(lambda tabs: [pane['geometry']['width'] for pane in tabs[0]['panes']] != before, 'repeat resize failed')
                # More replies than either bridge queue can hold, in one input burst.
                os.write(master, prefix + b'r' + b'jk' * 128 + b'\r' + prefix + b',\x15burst-done\r')
                await_state(lambda tabs: tabs[0]['name'] == 'burst-done', 'resize burst disconnected or stalled viewer', timeout=30)
                os.write(master, prefix + b'x')
                assert b'Close pane' in read_for(.15), 'close confirmation missing'
                assert len(listing()[0]['panes']) == 2, 'pane closed before confirmation'
                os.write(master, b'n')
                read_for(.1)
                os.write(master, b'\x1b')
                read_for(.1)
                assert len(listing()[0]['panes']) == 2, 'close cancellation killed pane'
                os.write(master, prefix + b'xy')
                await_state(lambda tabs: len(tabs[0]['panes']) == 1, 'confirmed close failed')
                os.write(master, prefix + b'[')
                assert b'Copy' in read_for(.3), 'copy hint bar missing'
                assert not listing()[0]['panes'][0]['copy']['active'], 'copy mode leaked into shared state'
                os.write(master, b'h' * 11 + b' ' + b'l' * 10)
                assert b'selection' in read_for(.15), 'selection-specific hints missing'
                os.write(master, b'y')
                assert b'\x1b]52;c;Q09QWV9UQVJHRVQ=' in read_for(.3), 'viewer-local clipboard output missing'
                assert not listing()[0]['panes'][0]['copy']['active'], 'copy completion changed shared state'
                os.write(master, b'\x1b[<4;2;2M\x1b[<36;12;2M\x1b[<4;12;2my')
                assert b'\x1b]52;c;Q09QWV9UQVJHRVQ=' in read_for(.4), 'private shift-drag copy failed'
                assert not listing()[0]['panes'][0]['copy']['active'], 'mouse selection leaked into shared state'

                literal_probe = "import os,tty; tty.setraw(0); print('LITERAL_READY',flush=True); data=b''\nwhile len(data)<2: data+=os.read(0,2-len(data))\nprint('LITERAL:'+data.hex(),flush=True); os.execl('/bin/cat','cat')"
                subprocess.run([binary, 'popup', sys.executable, '-c', literal_probe], env=env, capture_output=True, timeout=3, check=True)
                output = b''
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline and b'LITERAL_READY' not in output:
                    output += read_for(.05)
                assert b'Popup' in output, 'popup context footer missing'
                assert b'LITERAL_READY' in output, 'literal-prefix probe did not start'
                os.write(master, prefix + prefix + b'Q')
                assert b'LITERAL:' + prefix.hex().encode() + b'51' in read_for(.3), 'literal prefix was duplicated or consumed'

                os.write(master, b'popup-input\r')
                assert b'popup-input' in read_for(.3), 'popup hints intercepted application input'
                os.write(master, prefix)
                assert b'Commands' in read_for(.5), 'popup footer blocked delayed command hints'
                os.write(master, b'xy')
                read_for(.3)
                subprocess.run([binary, 'workspace', 'new', 'other'], env=env,
                               capture_output=True, timeout=5, check=True)
                os.write(master, prefix + b's')
                assert b'Choose workspace' in read_for(.3), 'workspace picker missing'
                os.write(master, b'\x1b')
                assert b'Commands' in read_for(.15), 'workspace cancel did not return to commands'
                os.write(master, b'\x1b')
                read_for(.1)
                # Select the second workspace and immediately rename there. The suffix
                # must survive switching and must never be sent to the old workspace.
                os.write(master, prefix + b'sj\r' + prefix + b',\x15switched\r')
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    result = subprocess.run([binary, 'other', 'list'], env=env,
                                            capture_output=True, timeout=3, check=True)
                    tabs = json.loads(result.stdout)['result']['value']['workspaces'][0]['tabs']
                    if tabs[0]['name'] == 'switched': break
                    read_for(.05)
                assert tabs[0]['name'] == 'switched', 'workspace switch lost queued command'
                assert listing()[0]['name'] == 'burst-done', 'switch suffix reached old workspace'
            os.write(master, prefix + b'd')
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                child, status = os.waitpid(pid, os.WNOHANG)
                if child:
                    exited = True
                    assert os.waitstatus_to_exitcode(status) == 0
                    break
                read_for(.05)
            assert exited, 'viewer failed to detach'
            if mode == 'default':
                for selection in [False, True]:
                    os.close(master)
                    pid, master = pty.fork()
                    if pid == 0:
                        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
                        os.execve(binary, [binary], env)
                    exited = False
                    output = b''
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline and b'Choose workspace' not in output:
                        output += read_for(.05)
                    assert b'Choose workspace' in output, 'initial workspace picker missing'
                    # A separate named attach needs the startup lock. It must proceed
                    # while this picker is still waiting for keyboard input.
                    probe_pid, probe_master = pty.fork()
                    if probe_pid == 0:
                        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
                        os.execve(binary, [binary, 'other'], env)
                    probe_exited = False
                    try:
                        probe_output = b''
                        deadline = time.monotonic() + 3
                        while time.monotonic() < deadline and b'switched' not in re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', probe_output):
                            if select.select([probe_master], [], [], .05)[0]:
                                probe_output += os.read(probe_master, 65536)
                        assert b'switched' in re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', probe_output), f'concurrent named attach did not render: {probe_output[-1000:]!r}'
                        os.write(probe_master, prefix + b'd')
                        deadline = time.monotonic() + 3
                        while time.monotonic() < deadline:
                            child, status = os.waitpid(probe_pid, os.WNOHANG)
                            if child:
                                probe_exited = True
                                assert os.waitstatus_to_exitcode(status) == 0
                                break
                            if select.select([probe_master], [], [], .02)[0]:
                                try:
                                    os.read(probe_master, 65536)
                                except OSError as error:
                                    if error.errno != errno.EIO: raise
                        assert probe_exited, 'concurrent viewer failed to detach'
                    finally:
                        if not probe_exited:
                            os.kill(probe_pid, signal.SIGKILL)
                            os.waitpid(probe_pid, 0)
                        os.close(probe_master)
                    os.write(master, b'j\r' + prefix + b'd' if selection else b'\x1b')
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        child, status = os.waitpid(pid, os.WNOHANG)
                        if child:
                            exited = True
                            assert os.waitstatus_to_exitcode(status) == 0
                            break
                        read_for(.05)
                    assert exited, 'initial picker did not cancel or preserve queued detach'
            print('PASS contextual help:', mode)
        finally:
            if not exited:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            os.close(master)
            for name in ['default', 'other']:
                subprocess.run([binary, 'workspace', 'kill', name], env=env,
                               capture_output=True, timeout=5, check=False)

for mode in ["default", "immediate", "hidden"]:
    check(mode)
