import sys, os, json, time, hashlib, select, termios, re
from pathlib import Path

def code(label, seed, size=10):
    return hashlib.sha256((label + ':' + str(seed)).encode()).hexdigest()[:size].upper()

def atomic(path, data):
    tmp = path.with_name(path.name + '.tmp.' + str(os.getpid()))
    tmp.write_text(json.dumps(data, sort_keys=True) + '\n')
    os.replace(tmp, path)

def record(work, name):
    try:
        return json.loads((Path(work) / '.fx' / (name + '.json')).read_text())
    except (FileNotFoundError, ValueError):
        return {}

class Terminal:
    def __init__(self, work, name):
        self.work, self.name = Path(work), name
        self.fx = self.work / '.fx'
        self.fx.mkdir(parents=True, exist_ok=True)
        self.path = self.fx / (name + '.json')
        prior = record(work, name)
        self.data = {'runs': prior.get('runs', 0) + 1, 'tty': os.isatty(0) and os.isatty(1),
                     'pid': os.getpid(), 'success': False, 'premature': [], 'inputs': []}
        if self.data['tty']:
            self.data['terminal'] = os.ttyname(0)
            z = os.get_terminal_size(1)
            self.data['initial_size'] = [z.columns, z.lines]
        self.save()
        pidpath = self.fx / (Path(__file__).stem + '.pid')
        pidtmp = pidpath.with_name(pidpath.name + '.tmp.' + str(os.getpid()))
        pidtmp.write_text(str(os.getpid()) + '\n')
        os.replace(pidtmp, pidpath)
        if self.data['runs'] != 1 or not self.data['tty']:
            raise SystemExit('This fixture needs a terminal and exactly one launch.')
        self.saved = termios.tcgetattr(0)
        cfg = termios.tcgetattr(0)
        cfg[0] |= termios.ICRNL
        cfg[0] &= ~(termios.IGNCR | termios.INLCR)
        cfg[3] |= termios.ICANON | termios.ECHO
        termios.tcsetattr(0, termios.TCSANOW, cfg)
        self.pending = b''
        self.queue = []
    def save(self):
        atomic(self.path, self.data)
    def out(self, text='', end='\n'):
        print(text, end=end, flush=True)
    def clean(self, b):
        s = b.decode('utf-8', 'replace').replace('\x1b[200~', '').replace('\x1b[201~', '')
        result = []
        for ch in s:
            if ch in '\b\x7f':
                if result: result.pop()
            elif ch == '\x15': result.clear()
            else: result.append(ch)
        return ''.join(result)
    def receive(self):
        chunk = os.read(0, 65536)
        if not chunk: raise RuntimeError('terminal input closed')
        self.pending += chunk.replace(b'\r', b'\n')
        while b'\n' in self.pending:
            raw, self.pending = self.pending.split(b'\n', 1)
            line = self.clean(raw)
            if line: self.queue.append(line)
    def line(self, prompt):
        # Reject completed typeahead before exposing a newly enabled prompt.
        # Blank CR/LF companions and paste delimiters do not count as submissions.
        while select.select([0], [], [], 0)[0]:
            self.receive()
        if self.queue:
            self.data['premature'].extend(self.queue)
            self.queue.clear()
            self.fail('Input arrived before the real prompt was ready.')
        self.out(prompt, end='')
        while not self.queue:
            self.receive()
        result = self.queue.pop(0)
        self.data['inputs'].append(result)
        self.save()
        return result
    def delay(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if self.queue:
                self.data['premature'].extend(self.queue)
                self.queue.clear()
                self.fail('Input arrived before the real prompt was ready.')
            if select.select([0], [], [], max(0, end - time.monotonic()))[0]:
                self.receive()
        if self.queue:
            self.data['premature'].extend(self.queue)
            self.queue.clear()
            self.fail('Input arrived before the real prompt was ready.')
    def expect(self, prompt, wanted):
        got = self.line(prompt)
        if got != wanted:
            self.fail('Submitted value did not match the current request.')
        return got
    def fail(self, message):
        self.data['error'] = message
        self.save()
        self.out('\nERROR: ' + message)
        raise SystemExit(2)
    def done(self, **fields):
        self.data.update(fields)
        self.data['success'] = True
        self.save()
    def __enter__(self): return self
    def __exit__(self, *args):
        termios.tcsetattr(0, termios.TCSANOW, self.saved)

work = sys.argv[1]
seed = 452
archive = code('archive', seed)
request = code('stream', archive)
resume = code('continue', archive)
tail = code('tail', archive, 16)
receipt = code('sealed', tail)
with Terminal(work, 'archive') as t:
    if t.data['initial_size'] != [72,18]: t.fail('Initial terminal must be 72 columns by 18 rows.')
    t.out('SAMPLE ARCHIVE ' + code('sample', seed) + ' tail=' + code('sample-tail', seed, 16))
    t.out('\x1b[36mARCHIVE ' + archive + '\x1b[0m')
    t.out('Enter exactly: stream ' + request)
    t.expect('Stream> ', 'stream ' + request)
    t.out('SCAN BEGIN archive=' + archive)
    t.delay(2.4)
    total = 0
    for batch in range(60):
        rows = []
        for j in range(250):
            n = batch*250+j
            rows.append('scan %05d '%n + ('block-%04d '%(n%997))*10 + ' verified\n')
        chunk=''.join(rows)
        total += len(chunk.encode())
        sys.stdout.write(chunk); sys.stdout.flush()
        t.delay(0.015)
    t.data['scan_bytes'] = total
    t.save()
    t.out('RESIZE REQUIRED: 113 columns by 31 rows in this same terminal.')
    t.out('After resizing enter exactly: continue ' + resume)
    t.expect('Resize> ', 'continue ' + resume)
    size=os.get_terminal_size(1)
    t.data['resumed_size']=[size.columns,size.lines]; t.save()
    if t.data['resumed_size'] != [113,31]: t.fail('The running terminal was not resized as requested.')
    t.out('PAYLOAD BEGIN — one logical line follows; only its LAST tail field counts.')
    payload='PAYLOAD archive='+archive+' tail='+code('false-prefix', seed,16)+' '
    payload += ''.join('segment%04d:%s|'%(i,code('segment',i,48)) for i in range(820))
    payload += ' tail='+code('false-middle',seed,16)+' '+('Z'*8192)+' tail='+tail
    t.out('\x1b[33m'+payload+'\x1b[0m')
    t.out('PAYLOAD END')
    for batch in range(26):
        t.out(''.join('footer %04d archive index retained; no payload token here\n'%(batch*100+j) for j in range(100)),end='')
        t.delay(0.02)
    t.out('FOOTERS COMPLETE')
    t.out('Submit: seal <current-archive-tag> <final-tail-token>')
    t.expect('Seal> ', 'seal '+archive+' '+tail)
    t.out('SEALED receipt='+receipt)
    t.done(archive=archive, tail=tail, receipt=receipt, normal_exit=True)
