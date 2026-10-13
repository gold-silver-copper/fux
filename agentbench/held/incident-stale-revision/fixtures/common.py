import atexit
import hashlib
import json
import os
from pathlib import Path
import re
import select
import signal
import sys
import termios
import time
import tty


def digest(text, n=12):
    return hashlib.sha256(text.encode()).hexdigest()[:n]


class TerminalInput:
    """Decode terminal events, keeping partial sequences across reads."""
    def __init__(self):
        self.buffer = bytearray()
        self.after_cr = False

    def read(self):
        data = os.read(0, 4096)
        if not data:
            raise EOFError('terminal closed')
        self.buffer.extend(data)

    def parse(self):
        while self.buffer:
            if self.after_cr:
                self.after_cr = False
                if self.buffer[0] == 10:
                    del self.buffer[0]
                    continue
            first = self.buffer[0]
            if first == 27:
                if len(self.buffer) < 2:
                    return None
                if self.buffer[1] in (ord('['), ord('O')):
                    if self.buffer[:3] == b'\x1b[[':
                        if len(self.buffer) < 4:
                            return None
                        final = chr(self.buffer[3])
                        del self.buffer[:4]
                        return {'A': 'F1', 'B': 'F2', 'C': 'F3', 'D': 'F4', 'E': 'F5'}.get(final, 'UNKNOWN')
                    if self.buffer[:3] == b'\x1b[M':
                        if len(self.buffer) < 6:
                            return None
                        button, x, y = (v - 32 for v in self.buffer[3:6])
                        del self.buffer[:6]
                        return ('MOUSE', button, x, y, (button & 3) != 3)
                    end = next((i for i in range(2, len(self.buffer))
                                if 0x40 <= self.buffer[i] <= 0x7e), None)
                    if end is None:
                        return None
                    seq = bytes(self.buffer[:end + 1])
                    del self.buffer[:end + 1]
                    if seq in (b'\x1b[200~', b'\x1b[201~'):
                        continue
                    mouse = re.fullmatch(rb'\x1b\[<(\d+);(\d+);(\d+)([Mm])', seq)
                    if mouse:
                        button, x, y = (int(mouse[i]) for i in (1, 2, 3))
                        return ('MOUSE', button, x, y, mouse[4] == b'M')
                    final = chr(seq[-1])
                    keys = {'A': 'UP', 'B': 'DOWN', 'C': 'RIGHT', 'D': 'LEFT',
                            'H': 'HOME', 'F': 'END', 'Z': 'BACKTAB',
                            'P': 'F1', 'Q': 'F2', 'R': 'F3', 'S': 'F4'}
                    if final in keys:
                        return keys[final]
                    if final == '~':
                        number = seq[2:-1].split(b';')[0]
                        return {b'1': 'HOME', b'4': 'END', b'5': 'PAGEUP',
                                b'6': 'PAGEDOWN', b'7': 'HOME', b'8': 'END',
                                b'11': 'F1', b'12': 'F2', b'13': 'F3',
                                b'14': 'F4', b'15': 'F5', b'17': 'F6',
                                b'18': 'F7', b'19': 'F8', b'20': 'F9',
                                b'21': 'F10', b'23': 'F11', b'24': 'F12'}.get(number, 'UNKNOWN')
                    return 'UNKNOWN'
                del self.buffer[0]
                return 'ESC'
            if first in (10, 13):
                del self.buffer[0]
                self.after_cr = first == 13
                return '\n'
            length = 1 if first < 128 else (2 if first < 224 else (3 if first < 240 else 4))
            if len(self.buffer) < length:
                return None
            data = bytes(self.buffer[:length])
            del self.buffer[:length]
            return data.decode('utf-8', errors='replace')
        return None

    def event(self):
        while True:
            event = self.parse()
            if event is not None:
                return event
            # Only a lone Escape needs disambiguation. Once a sequence prefix
            # arrives, wait for its remainder regardless of scheduler delays.
            if self.buffer == b'\x1b' and not select.select([0], [], [], .1)[0]:
                self.buffer.clear()
                return 'ESC'
            self.read()

    def pending(self):
        while select.select([0], [], [], 0)[0]:
            self.read()
        return self.parse()


_input = TerminalInput()


class Fixture:
    def __init__(self, work, name, interactive=True):
        self.work = Path(work).resolve()
        self.fx = self.work / '.fx'
        self.fx.mkdir(exist_ok=True)
        self.name = name
        self.state = {'name': name, 'pid': os.getpid(), 'phase': 'started',
                      'stdin_tty': sys.stdin.isatty(), 'stdout_tty': sys.stdout.isatty()}
        launch_number = 1
        while True:
            marker = self.fx / (name + f'.launch-{launch_number:04d}.pid')
            try:
                with marker.open('x') as stream:
                    stream.write(str(os.getpid()))
                break
            except FileExistsError:
                launch_number += 1
        self.state['launch_number'] = launch_number
        (self.fx / (name + '.pid')).write_text(str(os.getpid()))
        self.save()
        self.old = None
        if not self.state['stdin_tty'] or not self.state['stdout_tty']:
            self.fail('stdin and stdout must both be terminals')
        if interactive:
            self.old = termios.tcgetattr(0)
            tty.setcbreak(0)
            current = termios.tcgetattr(0)
            current[0] &= ~(termios.IXON | termios.IXOFF | termios.ICRNL | termios.INLCR | termios.IGNCR)
            termios.tcsetattr(0, termios.TCSANOW, current)
            atexit.register(self.restore)

    def restore(self):
        if self.old is not None:
            termios.tcsetattr(0, termios.TCSANOW, self.old)
            self.old = None

    def save(self, **values):
        self.state.update(values)
        p = self.fx / (self.name + '.json')
        t = p.with_suffix('.tmp')
        t.write_text(json.dumps(self.state, sort_keys=True))
        t.replace(p)

    def fail(self, reason, code=90):
        self.save(phase='failed', error=reason)
        print('\n\x1b[31mREJECTED: ' + reason + '\x1b[0m', flush=True)
        raise SystemExit(code)

    def pause(self, seconds, guard=True):
        until = time.monotonic() + seconds
        while True:
            left = until - time.monotonic()
            if left <= 0:
                return
            if guard:
                try:
                    if _input.pending() is not None:
                        self.fail('input before the next prompt')
                except EOFError:
                    self.fail('terminal closed')
            time.sleep(min(left, .1))

    def line(self, prompt):
        print(prompt, end='', flush=True)
        data = ''
        while True:
            try:
                event = key()
            except EOFError:
                self.fail('terminal closed')
            if event == '\n':
                print('', flush=True)
                return data
            if event in ('\x7f', '\x08'):
                if data:
                    data = data[:-1]
                    print('\b \b', end='', flush=True)
            elif event == '\x15':
                data = ''
                print('^U', end='', flush=True)
            elif isinstance(event, str) and len(event) == 1 and event.isprintable():
                data += event
                print(event, end='', flush=True)

    def expect(self, prompt, wanted):
        got = self.line(prompt)
        self.save(received=self.state.get('received', []) + [got])
        if got != wanted:
            self.fail('unexpected input: ' + repr(got))
        return got

    def done(self, **values):
        self.save(phase='complete', **values)


def key():
    return _input.event()


def screen(lines):
    print('\x1b[2J\x1b[H' + '\r\n'.join(lines), end='', flush=True)
