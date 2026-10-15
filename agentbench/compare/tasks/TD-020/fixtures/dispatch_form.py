import os
import sys
import json
import time
import hashlib
import threading
import queue
import select
import termios
import tty
from pathlib import Path


def code(label, *parts):
    return hashlib.sha256('|'.join([label] + [str(p) for p in parts]).encode()).hexdigest()[:10].upper()


def atomic(path, data):
    tmp = path.with_name(path.name + '.tmp-' + str(os.getpid()))
    tmp.write_text(data)
    os.replace(tmp, path)


class Run:
    def __init__(self, name, work):
        self.work = Path(work)
        self.fx = self.work / '.fx'
        self.fx.mkdir(exist_ok=True)
        self.name = name
        self.path = self.fx / (name + '.run-' + str(os.getpid()) + '.json')
        self.data = {'name': name, 'tty': os.isatty(0) and os.isatty(1),
                     'success': False, 'state': 'running', 'inputs': []}
        atomic(self.fx / (name + '.pid'), str(os.getpid()) + '\n')
        self.save()
        if not self.data['tty']:
            self.end(False, 'stdin and stdout must both be terminals')
            raise SystemExit(2)

    def save(self):
        atomic(self.path, json.dumps(self.data, sort_keys=True) + '\n')

    def end(self, success, reason, **fields):
        self.data.update(fields)
        self.data.update(success=success, state='finished', reason=reason)
        self.save()

    def fail(self, reason):
        self.end(False, reason)
        print('\r\nPROGRAM ERROR: ' + reason, flush=True)
        return 3


class Keys:
    MAP = {
        '\x1b[A': 'UP', '\x1bOA': 'UP', '\x1b[B': 'DOWN', '\x1bOB': 'DOWN',
        '\x1b[C': 'RIGHT', '\x1bOC': 'RIGHT', '\x1b[D': 'LEFT', '\x1bOD': 'LEFT',
        '\x1b[H': 'HOME', '\x1bOH': 'HOME', '\x1b[1~': 'HOME', '\x1b[7~': 'HOME',
        '\x1b[F': 'END', '\x1bOF': 'END', '\x1b[4~': 'END', '\x1b[8~': 'END',
        '\x1b[3~': 'DELETE', '\x1bO3~': 'DELETE', '\x1b[Z': 'BACKTAB',
        '\x1bOP': 'F1', '\x1bOQ': 'F2', '\x1bOR': 'F3', '\x1bOS': 'F4',
        '\x1b[11~': 'F1', '\x1b[12~': 'F2', '\x1b[13~': 'F3', '\x1b[14~': 'F4',
        '\x1b[[A': 'F1', '\x1b[[B': 'F2', '\x1b[[C': 'F3', '\x1b[[D': 'F4',
        '\x1b[15~': 'F5', '\x1b[[E': 'F5', '\x1b[17~': 'F6',
        '\x1b[18~': 'F7', '\x1b[19~': 'F8', '\x1b[20~': 'F9', '\x1b[21~': 'F10',
        '\x1b[200~': 'PASTE_START', '\x1b[201~': 'PASTE_END',
    }
    def __init__(self):
        self.buf = ''
        self.after_cr = False

    def get(self, timeout=0.1):
        if not self.buf:
            if not select.select([0], [], [], timeout)[0]:
                return None
            chunk = os.read(0, 4096)
            if not chunk:
                return 'EOF'
            self.buf += chunk.decode('utf-8', 'replace')
        if self.buf.startswith('\x1b'):
            while self.buf not in self.MAP and any(s.startswith(self.buf) for s in self.MAP):
                if not select.select([0], [], [], 0.1)[0]:
                    break
                self.buf += os.read(0, 4096).decode('utf-8', 'replace')
            for seq in sorted(self.MAP, key=len, reverse=True):
                if self.buf.startswith(seq):
                    self.buf = self.buf[len(seq):]
                    result = self.MAP[seq]
                    return None if result.startswith('PASTE_') else result
            self.buf = self.buf[1:]
            return 'ESC'
        ch, self.buf = self.buf[0], self.buf[1:]
        if ch == '\n' and self.after_cr:
            self.after_cr = False
            return None
        self.after_cr = ch == '\r'
        return {'\r': 'ENTER', '\n': 'ENTER', '\x7f': 'BACKSPACE', '\x08': 'BACKSPACE',
                '\x15': 'KILL', '\x17': 'WORD', '\x01': 'HOME', '\x05': 'END',
                '\x04': 'DELETE', '\t': 'TAB', '\x03': 'INTERRUPT'}.get(ch, ch)


class Raw:
    def __enter__(self):
        self.old = termios.tcgetattr(0)
        tty.setraw(0)
        # Keep ordinary output newlines useful in captures, even though input is raw.
        attrs = termios.tcgetattr(0)
        attrs[1] |= termios.OPOST | termios.ONLCR
        termios.tcsetattr(0, termios.TCSANOW, attrs)
        self.keys = Keys()
        return self.keys

    def __exit__(self, *args):
        termios.tcsetattr(0, termios.TCSANOW, self.old)


class Editor:
    def __init__(self, value=''):
        self.value = value
        self.cursor = len(value)

    def key(self, k):
        if k == 'LEFT': self.cursor = max(0, self.cursor - 1)
        elif k == 'RIGHT': self.cursor = min(len(self.value), self.cursor + 1)
        elif k == 'HOME': self.cursor = 0
        elif k == 'END': self.cursor = len(self.value)
        elif k == 'BACKSPACE' and self.cursor:
            self.value = self.value[:self.cursor-1] + self.value[self.cursor:]
            self.cursor -= 1
        elif k == 'DELETE': self.value = self.value[:self.cursor] + self.value[self.cursor+1:]
        elif k == 'KILL':
            self.value = self.value[self.cursor:]
            self.cursor = 0
        elif k == 'WORD':
            pos = self.cursor
            while pos and self.value[pos-1].isspace(): pos -= 1
            while pos and not self.value[pos-1].isspace(): pos -= 1
            self.value = self.value[:pos] + self.value[self.cursor:]
            self.cursor = pos
        elif len(k) == 1 and k.isprintable():
            self.value = self.value[:self.cursor] + k + self.value[self.cursor:]
            self.cursor += 1

    def draw(self, prompt):
        sys.stdout.write('\r\x1b[2K\x1b[36m' + prompt + '\x1b[0m' + self.value)
        back = len(self.value) - self.cursor
        if back: sys.stdout.write('\x1b[' + str(back) + 'D')
        sys.stdout.flush()


def edit_line(keys, prompt, initial=''):
    editor = Editor(initial)
    editor.draw(prompt)
    while True:
        k = keys.get()
        if k is None: continue
        if k in ('EOF', 'INTERRUPT'): return None
        if k == 'ENTER':
            print(flush=True)
            return editor.value
        editor.key(k)
        editor.draw(prompt)


def raw_busy(keys, seconds):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        k = keys.get(min(0.1, max(0, end-time.monotonic())))
        if k is not None:
            return False
    return True


class Lines:
    def __init__(self):
        # The kernel owns echo and line editing here.
        attrs = termios.tcgetattr(0)
        attrs[0] |= termios.ICRNL
        attrs[3] |= termios.ICANON | termios.ECHO | termios.ECHOE | termios.ECHOK
        attrs[6][termios.VERASE] = b'\x7f'
        attrs[6][termios.VKILL] = b'\x15'
        termios.tcsetattr(0, termios.TCSANOW, attrs)
        self.q = queue.Queue()
        threading.Thread(target=self.reader, daemon=True).start()

    def reader(self):
        pending = ''
        while True:
            data = os.read(0, 4096)
            if not data:
                self.q.put(None)
                return
            pending += data.decode('utf-8', 'replace').replace('\r\n', '\n').replace('\r', '\n')
            while '\n' in pending:
                line, pending = pending.split('\n', 1)
                line = line.replace('\x1b[200~', '').replace('\x1b[201~', '')
                normalized = Editor()
                for char in line:
                    normalized.key({'\x08': 'BACKSPACE', '\x7f': 'BACKSPACE', '\x15': 'KILL', '\x17': 'WORD'}.get(char, char))
                if normalized.value: self.q.put((time.monotonic(), normalized.value))

    def quiet(self, seconds):
        try:
            item = self.q.get(timeout=seconds)
            return False, item
        except queue.Empty:
            return True, None

    def get(self):
        return self.q.get()


def config(work):
    return json.loads((Path(work) / 'job.json').read_text())

def main():
    run = Run('dispatch_form', sys.argv[1])
    c = config(run.work)
    batch = code('batch', c['seed'], c['destination'], c['note'], c['mode'])
    preview = code('preview', c['seed'])
    challenge = code('form-challenge', batch)
    receipt = code('form-receipt', batch, challenge)
    ack = 'commit ' + challenge + ' receipt=' + preview
    fields = [Editor('retired'), Editor('receipt=' + preview)]
    modes = ['staged', 'verified', 'live']
    mode = 0
    focus = 0
    state = 'form'
    modal = Editor()
    message = 'Edit both fields, choose the requested mode, then F4 to review.'
    busy_start = None
    commit_ready = False
    committed = False

    def draw():
        sys.stdout.write('\x1b[H\x1b[2J\x1b[1;35mDISPATCH FORM\x1b[0m\n')
        print('Tab / Shift-Tab: field   Left / Right: mode   F4: review   F10: quit')
        print('Required destination: ' + c['destination'])
        print('Required note: ' + c['note'])
        print('Required mode: ' + c['mode'])
        print(('> ' if focus == 0 else '  ') + 'Destination: ' + fields[0].value)
        print(('> ' if focus == 1 else '  ') + 'Note: ' + fields[1].value)
        print(('> ' if focus == 2 else '  ') + 'Mode: ' + modes[mode])
        print('Receipt preview: ' + preview)
        print(message)
        if state == 'modal':
            print('\nREVIEW MODAL: type the acknowledgement, then Enter.')
            print('SUBMIT EXACTLY: ' + ack)
            print('commit receipt> ' + modal.value)
        elif state == 'busy':
            print('\nSubmitted acknowledgement: ' + modal.value)
            print('COMMIT READY - press F6' if commit_ready else 'COMMIT BUSY - input is locked')
        elif state == 'done':
            print('\nCOMMITTED batch=' + batch + ' receipt=' + receipt)
            print('F10 exits and restores the ordinary terminal screen.')
        sys.stdout.flush()

    with Raw() as keys:
        sys.stdout.write('\x1b[?1049h\x1b[?25l')
        try:
            draw()
            while True:
                if state == 'busy' and not commit_ready and time.monotonic() - busy_start >= 5.3:
                    commit_ready = True
                    message = 'Journal is durable. The final commit still requires F6.'
                    draw()
                k = keys.get()
                if k is None: continue
                if k in ('EOF', 'INTERRUPT'):
                    return run.fail('form interrupted')
                if k == 'F10':
                    if not committed: return run.fail('quit before commit')
                    run.end(True, 'form committed and closed', batch=batch, receipt=receipt, closed=True)
                    return 0
                if state == 'done':
                    continue
                if state == 'busy':
                    if k != 'F6' or not commit_ready:
                        return run.fail('input arrived before COMMIT READY or was not F6')
                    committed = True
                    state = 'done'
                    message = 'Commit complete.'
                    draw()
                    continue
                if state == 'modal':
                    if k == 'ENTER':
                        run.data['inputs'].append(modal.value)
                        if modal.value != ack: return run.fail('wrong modal acknowledgement')
                        state = 'busy'
                        busy_start = time.monotonic()
                        message = 'Writing journal; the acknowledgement receipt is a preview.'
                    else: modal.key(k)
                    draw()
                    continue
                if k == 'TAB': focus = (focus + 1) % 3
                elif k == 'BACKTAB': focus = (focus - 1) % 3
                elif focus == 2 and k in ('LEFT', 'RIGHT', 'UP', 'DOWN'):
                    mode = (mode + (1 if k in ('RIGHT', 'DOWN') else -1)) % len(modes)
                elif k == 'F4':
                    actual = [fields[0].value, fields[1].value, modes[mode]]
                    run.data['form'] = actual
                    if actual != [c['destination'], c['note'], c['mode']]:
                        message = 'Validation failed: match all three required values.'
                    else:
                        state = 'modal'
                        modal = Editor('commit ' + code('old-form', c['seed']) + ' receipt=' + preview)
                        message = 'Review the current batch.'
                elif focus < 2:
                    fields[focus].key(k)
                draw()
        finally:
            sys.stdout.write('\x1b[?25h\x1b[?1049l')
            sys.stdout.flush()
            if run.data['success']:
                print('FORM CLOSED', flush=True)

if __name__ == '__main__':
    raise SystemExit(main())
