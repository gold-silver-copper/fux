"""Byte-exact PTY scenario runner. It never invokes a terminal-driving tool."""
import base64
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import tempfile
import termios
import time


FIELDS = {
    'start': {'seq', 'ev', 'name', 'argv'},
    'mode': {'seq', 'ev', 'raw', 'echo', 'alt'},
    'block': {'seq', 'ev', 'on'},
    'read': {'seq', 'ev', 'b64', 'consumed'},
    'write': {'seq', 'ev', 'b64', 'role', 'answers', 'thread'},
    'bell': {'seq', 'ev', 'answers', 'thread'},
    'exit': {'seq', 'ev', 'status'},
}


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def decode(record):
    data = base64.b64decode(record['b64'], validate=True)
    require(base64.b64encode(data).decode('ascii') == record['b64'], 'noncanonical base64')
    return data


def interleaving(shown, written, echoed):
    """Match the exact shuffle of writes and independently generated kernel echo.

    At observed offset i and echo offset k the write offset is i-k. Keeping
    all possible k avoids a greedy match stealing an input-repeating reply.
    No bytes can be omitted, duplicated, stripped, or reordered in either stream.
    """
    require(len(shown) == len(written) + len(echoed),
            'byte count: shown=%d written=%d expected kernel echo=%d' %
            (len(shown), len(written), len(echoed)))
    states = {0}
    for i, byte in enumerate(shown):
        following = set()
        for k in states:
            j = i - k
            if j < len(written) and written[j] == byte:
                following.add(k)
            if k < len(echoed) and echoed[k] == byte:
                following.add(k+1)
        require(following, 'unaccounted/reordered byte at PTY offset %d: %r' % (i, shown[max(0,i-16):i+32]))
        states = following
    require(len(echoed) in states, 'incomplete write or kernel echo stream')


def input_effects(data, attrs):
    """Exact input/echo rules used by these ASCII scenarios, from live termios.

    Canonical fixtures send printable ASCII and Enter only. Other editing keys
    are exclusively noncanonical with kernel ECHO disabled. Unsupported kernel
    editing is rejected instead of being guessed.
    """
    iflag, oflag, _, lflag, _, _, _ = attrs
    raw = not bool(lflag & termios.ICANON)
    echo = bool(lflag & termios.ECHO)
    require(not echo or not raw, 'unexpected kernel echo in noncanonical mode')
    received = bytearray()
    kernel = bytearray()
    for byte in data:
        if byte == 13:
            if iflag & termios.IGNCR:
                continue
            if iflag & termios.ICRNL:
                byte = 10
        elif byte == 10 and iflag & termios.INLCR:
            byte = 13
        require(not (lflag & termios.ICANON) or byte == 10 or 32 <= byte < 127,
                'canonical scenario uses unsupported kernel editing/control byte')
        received.append(byte)
        if echo or (byte == 10 and lflag & termios.ECHONL):
            require(not raw and echo, 'unlogged echo outside canonical ECHO-on mode')
            if byte == 10 and oflag & termios.OPOST and oflag & termios.ONLCR:
                kernel.extend(b'\r\n')
            else:
                kernel.append(byte)
    return bytes(received), bytes(kernel)


def validate(folder, scenario, records, observed_at, typed_count, received, kernel, shown, status):
    require(records and records[0]['ev'] == 'start', 'log must begin with start')
    require(len(records) >= 2 and records[1]['ev'] == 'mode', 'initial mode must follow start')
    require(records[0]['name'] == folder.name, 'wrong fixture name')
    require(records[0]['argv'] == scenario['args'], 'start argv differs from scenario')
    require(records[-1]['ev'] == 'exit', 'scenario did not exit; no idle scenario is declared')
    require(status is not None and os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, 'child exit failed')
    require(records[-1]['status'] == 0, 'logged exit failed')
    consumed, written, reads = 0, bytearray(), bytearray()
    pending_read = False
    mode = None
    bell_writes, bell_events = [], []
    for index, record in enumerate(records):
        kind = record.get('ev')
        require(kind in FIELDS, 'unknown event')
        require(set(record) == FIELDS[kind], 'fields differ from frozen schema for ' + str(kind))
        require(type(record['seq']) is int and record['seq'] == index+1, 'noncontiguous shared seq')
        if kind == 'start':
            require(index == 0, 'repeated start')
            require(isinstance(record['name'], str) and isinstance(record['argv'], list), 'invalid start')
        elif kind == 'mode':
            require(all(type(record[k]) is bool for k in ('raw', 'echo', 'alt')), 'mode fields not booleans')
            mode = record
        elif kind == 'block':
            require(record['on'] in ('read', 'sleep', 'poll', 'work'), 'invalid block reason')
            if record['on'] == 'read':
                require(not pending_read, 'read block without intervening read')
                pending_read = True
        elif kind == 'read':
            require(pending_read, 'read without preceding read block')
            pending_read = False
            data = decode(record)
            consumed += len(data)
            reads.extend(data)
            require(type(record['consumed']) is int and record['consumed'] == consumed, 'read total incorrect')
            require(consumed <= observed_at[index], 'read exceeds bytes typed so far')
        elif kind in ('write', 'bell'):
            answer = record['answers']
            require(answer is None or (type(answer) is int and 0 <= answer <= consumed), 'answers exceeds consumed input')
            require(answer is None or answer <= observed_at[index], 'answers exceeds bytes typed so far')
            require(isinstance(record['thread'], str) and bool(record['thread']), 'missing writing thread')
            if kind == 'write':
                require(mode is not None, 'write before initial mode')
                require(record['role'] in ('echo', 'output'), 'invalid write role')
                data = decode(record)
                require(bool(data), 'empty write record')
                written.extend(data)
                if b'\a' in data:
                    require(record['role'] == 'output', 'BEL write must be output')
                    bell_writes.extend([(answer, record['thread'])] * data.count(b'\a'))
            else:
                bell_events.append((answer, record['thread']))
        elif kind == 'exit':
            require(index == len(records)-1 and type(record['status']) is int, 'exit is not final')
    require(not pending_read, 'unfulfilled read at exit')
    require(consumed <= typed_count, 'read total exceeds all typed bytes')
    require(bytes(reads) == received[:len(reads)], 'read stream differs from typed bytes after termios translation')
    require(bell_events == bell_writes, 'bell events differ from BEL writes')
    require(mode is not None and not mode['alt'], 'alternate screen left active')
    require(all(mode[k] == records[1][k] for k in ('raw', 'echo')), 'initial terminal mode not restored')
    interleaving(shown, bytes(written), kernel)
    semantic_checks(folder.name, scenario['name'], records, kernel)
    return len(shown), len(records)


def semantic_checks(ident, name, records, kernel):
    """Independent assertions about selected ground-truth behaviors."""
    writes = [r for r in records if r['ev'] == 'write']
    output = b''.join(decode(r) for r in writes if r['role'] == 'output')
    echo = [r for r in writes if r['role'] == 'echo']
    if ident == 'FX-01':
        require(kernel and not echo, 'canonical kernel echo was not exercised')
        if name == 'exact-replies-and-empty':
            require(any(decode(r) == b'same text\r\n' and r['role'] == 'output' for r in writes), 'exact repeating response not output')
    if ident == 'FX-02':
        require(any(r['ev'] == 'mode' and not r['raw'] and not r['echo'] for r in records), 'password mode absent')
        require(b'swordfish' not in kernel and b'wrong' not in kernel, 'password unexpectedly echoed')
        require(any(decode(r).startswith((b'Unlocked', b'\aAccess denied')) for r in writes), 'password response did not begin on prompt row')
    expectations = {
        ('FX-03', 'all-editing-keys'): b'Alpha gamZa! -> queued\r\n',
        ('FX-03', 'ctrl-u-and-word-erase'): b'queued: fresh line\r\n',
        ('FX-03', 'home-end-middle-insertion'): b'^acd! -> queued\r\n',
        ('FX-04', 'tab-completion'): b'Executing: deploy\r\n',
        ('FX-04', 'whole-area-editing'): b'Executing: deplo\r\n',
        ('FX-04', 'completion-miss-and-clear'): b'Executing: describe\r\n',
        ('FX-08', 'thread-and-editing'): b'Search matches for rigTt\r\n',
    }
    if (ident, name) in expectations:
        require(expectations[ident, name] in output, 'editor produced wrong final text')
    if ident in ('FX-03', 'FX-04', 'FX-08'):
        require(any(decode(r) == b'\r\n' and r['answers'] is not None for r in echo), 'submission newline is not echo')
    if ident == 'FX-05':
        require(any(r['ev'] == 'mode' and r['alt'] for r in records), 'alternate screen never entered')
        require(b'\x1b[?1049h' in output and b'\x1b[?1049l' in output, 'alternate screen transition bytes absent')
    if ident == 'FX-06':
        reads = [r for r in records if r['ev'] == 'read']
        if name == 'timeouts-without-input':
            require(len(reads) == 3 and all(decode(r) == b'' for r in reads), 'empty VTIME reads absent')
        if name == 'interbyte-timeout':
            require([decode(r) for r in reads] == [b'ab', b'cde'], 'interbyte read grouping incorrect')
    if ident == 'FX-07':
        require(any(r['ev'] == 'block' and r['on'] == 'poll' for r in records), 'poll waits absent')
        require(any(r['ev'] == 'bell' for r in records), 'bell coverage absent')
    if ident == 'FX-08' and name != 'early-submit-stops-worker':
        require(any(r['thread'] == 'indexer' and r['answers'] is None for r in writes), 'worker writes absent')
        require(any(r['thread'] == 'indexer' for r in echo), 'worker did not restore input')
    if ident == 'FX-09':
        require(any(r['ev'] == 'block' and r['on'] == ('work' if 'computing' in name else 'sleep') for r in records), 'busy interval absent')
        require(any(b'\r\x1b[2K\x1b[5Cqueued:' in decode(r) for r in writes), 'single-write overwrite burst absent')
    if ident == 'FX-10':
        require(len(output) > 6000, 'insufficient scrolling output')
        require(any(b'phase one...\rphase two...\x1b[1G\x1b[2K' in decode(r) for r in writes), 'transient overwrite burst absent')


def run_scenario(folder, scenario):
    cols, rows = scenario['size']
    require(type(cols) is int and type(rows) is int and cols > 0 and rows > 0, 'invalid terminal size')
    with tempfile.TemporaryDirectory(prefix='.selftest-', dir=folder) as temporary:
        log_path = Path(temporary) / 'oracle.jsonl'
        pid, master = pty.fork()
        if pid == 0:
            try:
                fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
                os.environ['FUX_ORACLE_LOG'] = str(log_path)
                os.chdir(folder)
                os.execv(sys.executable, [sys.executable, str(folder / 'fixture.py')] + scenario['args'])
            except BaseException:
                os._exit(127)
        os.set_blocking(master, False)
        records, observed_at = [], []
        shown, typed, received, kernel = bytearray(), 0, bytearray(), bytearray()
        offset, pending, status, eof = 0, b'', None, False
        deadline = time.monotonic() + 12

        def logs():
            nonlocal offset, pending
            if not log_path.exists():
                return
            with log_path.open('rb') as handle:
                handle.seek(offset)
                chunk = handle.read()
            offset += len(chunk)
            lines = (pending + chunk).split(b'\n')
            pending = lines.pop()
            for line in lines:
                records.append(json.loads(line))
                observed_at.append(typed)

        def pump(duration):
            nonlocal status, eof
            end = time.monotonic() + duration
            while True:
                logs()
                if status is None:
                    waited, current = os.waitpid(pid, os.WNOHANG)
                    if waited:
                        status = current
                if not eof:
                    while True:
                        try:
                            chunk = os.read(master, 65536)
                        except BlockingIOError:
                            break
                        except OSError as exc:
                            if exc.errno != errno.EIO:
                                raise
                            eof = True
                            break
                        if not chunk:
                            eof = True
                            break
                        shown.extend(chunk)
                remaining = end - time.monotonic()
                if remaining <= 0:
                    logs()
                    return
                require(time.monotonic() < deadline, 'scenario timeout')
                if eof:
                    # All declared remaining pauses can be skipped after exit.
                    logs()
                    return
                select.select([master], [], [], min(remaining, 0.01))

        try:
            # Do not inject before mode setup and the first prompt/block point.
            while not any(r['ev'] == 'block' for r in records):
                pump(0.005)
                require(status is None, 'fixture exited before startup block')
            for step in scenario['steps']:
                require(type(step['pause_ms']) is int and step['pause_ms'] >= 0, 'invalid pause')
                data = step['keys'].encode('utf-8')
                if data:
                    require(status is None and not eof, 'scenario typed after exit')
                    attrs = termios.tcgetattr(master)
                    cooked, echoes = input_effects(data, attrs)
                    # Snapshot pre-injection records before advancing the input counter.
                    logs()
                    latest_mode = next(r for r in reversed(records) if r['ev'] == 'mode')
                    require(latest_mode['raw'] == (not bool(attrs[3] & termios.ICANON))
                            and latest_mode['echo'] == bool(attrs[3] & termios.ECHO),
                            'logged input mode differs from live terminal mode')
                    sent = 0
                    while sent < len(data):
                        try:
                            n = os.write(master, data[sent:])
                        except BlockingIOError:
                            pump(0.005)
                            continue
                        sent += n
                        typed += n
                    received.extend(cooked)
                    kernel.extend(echoes)
                pump(step['pause_ms'] / 1000)
            while status is None or not eof:
                pump(0.01)
                require(time.monotonic() < deadline, 'scenario failed to exit')
            logs()
            require(not pending, 'partial final JSONL record')
            return validate(folder, scenario, records, observed_at, typed, bytes(received), bytes(kernel), bytes(shown), status)
        finally:
            if status is None:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                os.waitpid(pid, 0)
            os.close(master)


def main(folder):
    folder = folder.resolve()
    scenarios = json.loads((folder / 'scenarios.json').read_text())
    require(3 <= len(scenarios) <= 6, 'fixture must have 3 to 6 scenarios')
    failures = 0
    for scenario in scenarios:
        try:
            size, events = run_scenario(folder, scenario)
            print('PASS %s/%s (%d terminal bytes, %d records)' % (folder.name, scenario['name'], size, events), flush=True)
        except Exception as exc:
            failures += 1
            print('FAIL %s/%s: %s: %s' % (folder.name, scenario['name'], type(exc).__name__, exc), flush=True)
    return int(bool(failures))
