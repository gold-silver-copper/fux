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
    run = Run('archive_agent', sys.argv[1])
    c = config(run.work)
    plan = code('plan', c['seed'], c['scope'])
    draft_plan = code('draft-plan', c['seed'])
    echo_result = code('echo-result', c['seed'])
    permission_token = code('permission-challenge', plan)
    permission = code('permission', plan, c['scope'])
    result = code('archive-result', plan, permission)
    request = 'plan scope=' + c['scope']
    draft = 'approve plan=' + draft_plan + ' result=' + echo_result
    approval = 'approve plan=' + plan + ' result=' + echo_result
    lock = threading.RLock()
    ready = threading.Event()
    staged = threading.Event()
    stop = threading.Event()
    editor = Editor()

    def draw():
        editor.draw('agent approve / result> ')

    def emit(message):
        with lock:
            sys.stdout.write('\r\x1b[2K' + message + '\n')
            draw()

    def worker():
        staged.wait()
        if stop.is_set(): return
        for delay, message in [(1.4, 'planner> indexing scope; draft remains unapproved'),
                               (1.8, 'planner> copied draft: ' + draft),
                               (2.2, 'planner> permission analysis finished')]:
            if stop.wait(delay): return
            emit(message)
        with lock:
            ready.set()
            emit('REVIEW READY plan=' + plan)
            emit('SUBMIT EXACTLY: ' + approval)

    with Raw() as keys:
        print('Archive agent connected. It echoes input itself; planner messages may interrupt a line.', flush=True)
        print('SUBMIT EXACTLY: ' + request, flush=True)
        line = edit_line(keys, 'agent plan> ')
        run.data['inputs'].append(line)
        if line != request: return run.fail('wrong planning request')
        print('PLANNING. Type the draft below WITHOUT Enter, then press F2 to stage it.', flush=True)
        print('DRAFT TO TYPE: ' + draft, flush=True)
        print('After F2 the planner runs in the background. Wait for REVIEW READY, then revise the line.', flush=True)
        thread = threading.Thread(target=worker, daemon=True)
        thread.start()
        draw()
        while True:
            k = keys.get()
            if k is None: continue
            with lock:
                if k in ('EOF', 'INTERRUPT'):
                    stop.set(); staged.set()
                    return run.fail('approval input closed')
                if k == 'F2':
                    if not staged.is_set():
                        if editor.value != draft:
                            stop.set(); staged.set()
                            return run.fail('F2 must stage the displayed draft')
                        run.data['staged_draft'] = editor.value
                        staged.set()
                        emit('DRAFT STAGED. Do not submit before REVIEW READY.')
                elif k == 'ENTER':
                    print(flush=True)
                    run.data['inputs'].append(editor.value)
                    if not ready.is_set():
                        stop.set(); staged.set()
                        return run.fail('approval submitted while planner was busy')
                    if editor.value != approval:
                        stop.set()
                        return run.fail('wrong revised approval')
                    break
                else:
                    editor.key(k)
                    draw()
        stop.set()
        print('Approval accepted. Resolving permission scope; input locked.', flush=True)
        if not raw_busy(keys, 2.7): return run.fail('input arrived before PERMISSION READY')
        allow = 'allow once ' + permission_token
        print('PERMISSION READY: archive write for scope=' + c['scope'], flush=True)
        print('SUBMIT EXACTLY: ' + allow, flush=True)
        line = edit_line(keys, 'agent allow once> ', 'allow session ' + draft_plan)
        run.data['inputs'].append(line)
        if line != allow: return run.fail('wrong permission grant')
        run.end(True, 'archive completed', plan=plan, permission=permission, result=result)
        print('AGENT COMPLETED plan=' + plan + ' permission=' + permission + ' result=' + result, flush=True)
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
