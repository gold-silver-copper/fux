#!/usr/bin/env python3
"""Self-contained terminal provenance fixture. See README.md."""
import argparse
import base64
import codecs
import fcntl
import json
import os
import random
import select
import signal
import struct
import sys
import termios
import threading
import time
import tty
import unicodedata

PROFILE = 5
NAME = "FG-%02d" % PROFILE


class Terminal:
    def __init__(self, args):
        threading.current_thread().name = 'main'
        self.args = args
        self.rng = random.Random(args.seed)
        self.delay = (150 + self.rng.randrange(60)) / 1000
        self.lock = threading.RLock()
        self.seq = self.consumed = 0
        self.saved = termios.tcgetattr(0)
        self.alt = False
        self.resized = False
        self.stop = threading.Event()
        self.worker = None
        self.log = open(os.environ['FUX_ORACLE_LOG'], 'a', encoding='utf-8') if os.environ.get('FUX_ORACLE_LOG') else None
        self.event('start', name=NAME, argv=sys.argv[1:])
        self.mode_record()
        signal.signal(signal.SIGWINCH, lambda *_: setattr(self, 'resized', True))

    def event(self, ev, **fields):
        with self.lock:
            self.seq += 1
            if self.log:
                self.log.write(json.dumps(dict(seq=self.seq, ev=ev, **fields), ensure_ascii=True) + '\n')
                self.log.flush()

    def mode_record(self):
        a = termios.tcgetattr(0)
        self.event('mode', raw=not bool(a[3] & termios.ICANON), echo=bool(a[3] & termios.ECHO), alt=self.alt)

    def mode(self, raw=False, echo=False, timed=False):
        with self.lock:
            if raw:
                tty.setraw(0, termios.TCSANOW)
            a = termios.tcgetattr(0)
            a[1] = 0  # No ONLCR: side-log bytes equal bytes leaving the slave.
            if not raw:
                a[0] = termios.ICRNL
                a[3] = termios.ICANON | termios.ISIG | termios.IEXTEN | termios.ECHOE | termios.ECHOK
            a[3] = (a[3] | termios.ECHO) if echo else (a[3] & ~(termios.ECHO | termios.ECHONL))
            if timed:
                a[6][termios.VMIN] = 0
                a[6][termios.VTIME] = 1 + self.args.seed % 3
            termios.tcsetattr(0, termios.TCSANOW, a)
            self.mode_record()

    def write(self, value, role='output', answers=None):
        data = value.encode('utf-8') if isinstance(value, str) else value
        with self.lock:
            while data:
                n = os.write(1, data[:16384])
                self.event('write', b64=base64.b64encode(data[:n]).decode(), role=role,
                           answers=answers, thread=threading.current_thread().name)
                data = data[n:]

    def bell(self, text='', answers=None):
        with self.lock:
            self.event('bell', answers=answers, thread=threading.current_thread().name)
            self.write('\a' + text, answers=answers)

    def read(self, n=1):
        self.event('block', on='read')
        data = os.read(0, n)
        with self.lock:
            self.consumed += len(data)
            self.event('read', b64=base64.b64encode(data).decode(), consumed=self.consumed)
        return data

    def sleep(self, factor=1):
        self.event('block', on='sleep')
        time.sleep(self.delay * factor)

    def busy(self):
        if PROFILE in (10, 12):
            self.event('block', on='work')
            end = time.monotonic() + self.delay
            value = self.args.seed
            while time.monotonic() < end:
                value = (value * 1664525 + 1013904223) & 0xffffffff
        else:
            self.sleep()

    def background(self):
        def run():
            for tick in range(1, 5):
                self.event('block', on='sleep')
                if self.stop.wait(self.delay * 1.25):
                    return
                message = '\x1b[36m[worker %d] 缓存 e\u0301 🙂\x1b[0m' % tick
                if PROFILE in (3, 4, 5, 10, 12):
                    # Save/restore the editor cursor; update a row above the input.
                    self.write('\x1b7\r\x1b[1A\x1b[2K' + message + '\x1b8')
                else:
                    self.write('\r\x1b[2K' + message + '\r\n')
        self.worker = threading.Thread(target=run, name='worker')
        self.worker.start()

    def finish(self, status):
        self.stop.set()
        if self.worker:
            self.worker.join()
        with self.lock:
            if self.alt:
                self.write('\x1b[?25h\x1b[?1049l')
                self.alt = False
                self.mode_record()
            termios.tcsetattr(0, termios.TCSANOW, self.saved)
            self.mode_record()
            self.event('exit', status=status)
            if self.log:
                self.log.close()


def reply(t, line):
    n = t.consumed
    t.bell(answers=n)
    t.write('\rchecking 0%\rchecking 99%\x1b[2K\r', answers=n)
    # Three particularly ambiguous outputs: prefix, suffix, and exact equality.
    t.write(line + ' : accepted\r\n', answers=n)
    t.write('saved value: ' + line + '\r\n', answers=n)
    t.write(line + '\r\n', answers=n)
    t.bell('done ✓\r\n', answers=n)


def canonical(t):
    t.mode(echo=True)
    t.write('\x1b[34mArchive console\x1b[0m — 输入 / e\u0301 / 🙂\r\n')
    if PROFILE in (1, 8):
        t.background()
    if PROFILE == 11:
        t.write('Preparing a large index; commands may be typed ahead.\r\n')
        t.busy()
        # > 1 MiB of real terminal output, before any value is read.
        row = ('index: ' + '0123456789abcdef' * 15 + ' 界 e\u0301 🙂\r\n').encode()
        for batch in range(270):
            t.write(row * 16)
        t.write('\rindex 100%\r\x1b[2Kindex ready\r\n')
    while True:
        t.write('build queue> ' if PROFILE == 8 else 'archive> ', answers=t.consumed or None)
        data = t.read(65536)
        if not data:
            return
        line = data.decode('utf-8', 'replace').rstrip('\n')
        if line == ':quit':
            t.write('closed\r\n', answers=t.consumed)
            return
        t.write('Queued ' + line + '\r\n', answers=t.consumed)
        if PROFILE == 8:
            for phase in ('resolving', 'compiling', 'linking'):
                t.write('\r' + phase + ' ' + line + '\x1b[K', answers=t.consumed)
                t.sleep(.5)
            t.write('\r\x1b[2Kbuild complete\r\n', answers=t.consumed)
        else:
            t.busy()
        reply(t, line)


def secret(t):
    t.mode(echo=False)
    t.write('\x1b[35mVault unlock\x1b[0m\r\nPassword: ')
    t.background()
    data = t.read(65536)
    # No newline: the answer occupies the password prompt's row.
    t.write('unlocked [' + data.decode('utf-8', 'replace').rstrip('\n') + ']\r\n', answers=t.consumed)
    t.mode(echo=True)
    while True:
        t.write('vault> ', answers=t.consumed)
        data = t.read(65536)
        line = data.decode('utf-8', 'replace').rstrip('\n')
        if not data or line == ':quit':
            return
        t.busy()
        reply(t, line)


def width(text):
    return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in text)


def layout(text, columns):
    """Explicit wraps cancel autowrap ambiguity, including a wide glyph at an edge."""
    rendered = []
    row = col = 0
    for char in text:
        cells = width(char)
        if cells and col + cells > columns:
            rendered.append('\r\n')
            row += 1
            col = 0
        rendered.append(char)
        col += cells
    # Normalize a cursor resting at the right margin to the following row.
    if col == columns:
        rendered.append('\r\n')
        row += 1
        col = 0
    return ''.join(rendered), row, col


def editor(t):
    t.mode(raw=True)
    fullscreen = PROFILE in (5, 12)
    toolkit = PROFILE in (4, 12)
    if fullscreen:
        t.write('\x1b[?1049h\x1b[?25l')
        t.alt = True
        t.mode_record()
        t.write('\x1b[2J\x1b[H')
    t.write('\x1b[32m' + {3:'Line shell',4:'Query composer',5:'Job monitor',7:'Polling console',9:'Mirror REPL',10:'Build runner',12:'Unicode canvas'}.get(PROFILE, 'Editor') + '\x1b[0m  (^D exits, ^] resizes)\r\n')
    if PROFILE in (3, 4, 5, 10, 12):
        t.background()
    chars, pos, menu = [], 0, False
    decoder = codecs.getincrementaldecoder('utf-8')('replace')
    escape = bytearray()
    prompt = 'query ❯ ' if toolkit else 'run> '

    def draw(role='echo', answers=None):
        size = os.get_terminal_size(0)
        text = ''.join(chars)
        columns = max(1, size.columns)
        if fullscreen:
            prefix = '\x1b[2;1H\x1b[J'
        else:
            # Track the occupied rows so long wrapped edits really redraw in place.
            prefix = '\r' + ('\x1b[%dA' % draw.row if draw.row else '') + '\x1b[J'
        body, endrow, _ = layout(prompt + text, columns)
        _, targetrow, targetcol = layout(prompt + ''.join(chars[:pos]), columns)
        if toolkit:
            hint, hintrow, _ = layout('  alpha   alpine   界面' if menu else '  Enter: execute | Tab: complete', columns)
            body += '\r\n\x1b[90m' + hint + '\x1b[0m'
            endrow += 1 + hintrow
        body += '\r'
        if fullscreen:
            body += '\x1b[%d;%dH' % (2 + targetrow, 1 + targetcol)
        else:
            if endrow > targetrow:
                body += '\x1b[%dA' % (endrow - targetrow)
            if targetcol:
                body += '\x1b[%dC' % targetcol
            draw.row = targetrow
        t.write(prefix + body, role, answers)
    draw.row = 0
    draw('output')
    while True:
        if t.resized:
            t.resized = False
            t.write('\r\x1b[2K[resize %dx%d]\r\n' % tuple(os.get_terminal_size(0)))
            draw(answers=t.consumed if t.consumed else None)
        if PROFILE == 7:
            t.event('block', on='poll')
            ready, _, _ = select.select([0], [], [], t.delay)
            if not ready:
                t.write('\rwaiting .\rwaiting ..\x1b[2K\r')
                draw(answers=t.consumed if t.consumed else None)
                continue
        data = t.read()
        if not data or data == b'\x04':
            t.write('\r\nbye\r\n', answers=t.consumed)
            return
        b = data[0]
        if escape or b == 27:
            escape.extend(data)
            seq = bytes(escape)
            sequences = {b'\x1b[D':'left',b'\x1b[C':'right',b'\x1b[H':'home',b'\x1b[F':'end',b'\x1b[3~':'delete',b'\x1b[1~':'home',b'\x1b[4~':'end'}
            if seq in sequences:
                action = sequences[seq]
                if action == 'left': pos = max(0, pos - 1)
                if action == 'right': pos = min(len(chars), pos + 1)
                if action == 'home': pos = 0
                if action == 'end': pos = len(chars)
                if action == 'delete' and pos < len(chars): del chars[pos]
                escape.clear()
            elif any(s.startswith(seq) for s in sequences):
                continue
            else:
                escape.clear()
                t.bell(answers=t.consumed)
        elif b in (10, 13):
            line = ''.join(chars)
            t.write('\r\n', 'echo', t.consumed)
            if line == ':quit':
                t.write('closed\r\n', answers=t.consumed)
                return
            if PROFILE in (3, 5, 9, 10, 12):
                t.busy()
            if PROFILE == 10:
                t.write('\rcompile 1%\rcompile 72%\x1b[1G\x1b[2Klink 99%\r\x1b[2K', answers=t.consumed)
            reply(t, line)
            chars, pos, menu = [], 0, False
            draw.row = 0
            draw('output', t.consumed)
            continue
        elif b in (8, 127):
            if pos:
                del chars[pos-1]
                pos -= 1
            else:
                t.bell(answers=t.consumed)
        elif b == 21:
            del chars[:pos]
            pos = 0
        elif b == 23:
            start = pos
            while pos and chars[pos-1].isspace(): pos -= 1
            while pos and not chars[pos-1].isspace(): pos -= 1
            del chars[pos:start]
        elif b == 9:
            menu = not menu
            if not toolkit:
                t.bell(' [no completions]', answers=t.consumed)
        elif b == 29:
            cols, rows = (int(x) for x in t.args.resize.split('x'))
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
            # A PTY not owned as controlling terminal may not receive the ioctl signal.
            os.kill(os.getpid(), signal.SIGWINCH)
        elif b < 32:
            t.bell(answers=t.consumed)
        else:
            text = decoder.decode(data)
            chars[pos:pos] = list(text)
            pos += len(text)
        draw(answers=t.consumed)


def timed(t):
    t.mode(raw=True, timed=True)
    t.write('Serial monitor (VMIN=0, VTIME=%d); ^D exits\r\nserial> ' % (1 + t.args.seed % 3))
    pending = bytearray()
    ticks = 0
    while True:
        data = t.read(4096)
        if not data:
            ticks += 1
            t.write('\rtimeout %d\r\x1b[2Kserial> ' % ticks)
            if pending:
                t.write(bytes(pending), 'echo', t.consumed)
            continue
        # Each chunk is echoed exactly; submission processing may answer a whole chunk.
        parts = data.split(b'\x04', 1)
        printable = parts[0]
        if printable:
            t.write(printable.replace(b'\r', b'\r\n'), 'echo', t.consumed)
        pending.extend(printable)
        while b'\r' in pending:
            line, _, rest = pending.partition(b'\r')
            pending = bytearray(rest)
            t.busy()
            reply(t, line.decode('utf-8', 'replace'))
            t.write('serial> ', answers=t.consumed)
        if len(parts) == 2:
            t.bell('serial closed\r\n', answers=t.consumed)
            return


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--seed', type=int, default=0)
    parser.add_argument('--resize', default='37x12', help='size applied by raw editor ^]')
    args = parser.parse_args()
    if not all(os.isatty(fd) for fd in (0, 1, 2)):
        parser.error('a terminal is required on stdin, stdout and stderr')
    t = Terminal(args)
    status = 0
    try:
        if PROFILE in (1, 8, 11): canonical(t)
        elif PROFILE == 2: secret(t)
        elif PROFILE == 6: timed(t)
        else: editor(t)
    except Exception as exc:
        status = 1
        t.write('\r\nfixture error: %s: %s\r\n' % (type(exc).__name__, exc), answers=t.consumed)
    finally:
        t.finish(status)
    return status


if __name__ == '__main__':
    sys.exit(main())
