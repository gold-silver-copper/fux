#!/usr/bin/env python3
"""Run scenarios with stdlib PTYs and check exact byte provenance."""
import base64
import copy
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

ROOT = Path(__file__).resolve().parent


def un64(record):
    return base64.b64decode(record['b64'], validate=True)


def shuffled(actual, written, echo):
    """Exact two-stream shuffle, retaining ambiguities instead of greedy guessing.

    At actual offset i, e echo bytes implies i-e fixture bytes. Thus each
    candidate needs just one integer, even for the megabyte fixture.
    """
    assert len(actual) == len(written) + len(echo), 'unaccounted or missing terminal bytes'
    if not echo:
        assert actual == written, 'terminal differs from logged writes'
        return
    states = {0}
    for i, value in enumerate(actual):
        after = set()
        for e in states:
            w = i - e
            if w < len(written) and written[w] == value:
                after.add(e)
            if e < len(echo) and echo[e] == value:
                after.add(e + 1)
        assert after, 'terminal provenance mismatch at byte %d' % i
        states = after
    assert len(echo) in states, 'echo stream incomplete'


def check(records, actual, echo, typed, received):
    assert len(records) >= 3
    assert records[0]['ev'] == 'start' and records[1]['ev'] == 'mode', 'missing initial start/mode'
    assert records[-1]['ev'] == 'exit', 'scenario did not exit'
    assert records[-1]['status'] == 0, 'fixture error exit'
    consumed = 0
    mode = None
    writes = []
    reads = []
    bells = 0
    fields = {
        'start': {'name', 'argv'}, 'mode': {'raw', 'echo', 'alt'},
        'block': {'on'}, 'read': {'b64', 'consumed'},
        'write': {'b64', 'role', 'answers', 'thread'},
        'bell': {'answers', 'thread'}, 'exit': {'status'},
    }
    for seq, record in enumerate(records, 1):
        assert record['seq'] == seq, 'sequence gap'
        kind = record['ev']
        assert kind in ('start','mode','block','read','write','bell','exit')
        assert set(record) == {'seq', 'ev'} | fields[kind], 'unexpected log fields'
        if kind == 'mode':
            assert all(type(record[k]) is bool for k in ('raw','echo','alt'))
            mode = record
        elif kind == 'block':
            assert record['on'] in ('read','sleep','poll','work')
        elif kind == 'read':
            payload = un64(record)
            reads.append(payload)
            consumed += len(payload)
            assert consumed == record['consumed'] and consumed <= typed
        elif kind in ('write','bell'):
            answer = record['answers']
            assert answer is None or (type(answer) is int and 0 <= answer <= consumed <= typed), 'invalid answers'
            assert isinstance(record['thread'], str) and record['thread']
            if kind == 'write':
                assert mode is not None
                assert record['role'] in ('echo','output')
                if record['role'] == 'echo':
                    assert mode['raw'] and not mode['echo'], 'program echo in wrong mode'
                data = un64(record)
                writes.append(data)
                if bells:
                    assert record['role'] == 'output' and b'\x07' in data, 'bell lacks BEL output'
                    bells -= 1
            else:
                bells += 1
        if seq != len(records):
            assert kind != 'exit', 'record after exit'
        if seq > 1:
            assert kind != 'start', 'duplicate start'
    assert bells == 0
    assert b''.join(reads) == received, 'read log differs from bytes delivered by the input mode'
    shuffled(actual, b''.join(writes), echo)


def run(scenario):
    with tempfile.TemporaryDirectory(prefix='.selftest-', dir=ROOT) as temp:
        log = Path(temp) / 'side.jsonl'
        pid, master = pty.fork()
        if pid == 0:
            os.environ['FUX_ORACLE_LOG'] = str(log)
            os.execv(sys.executable, [sys.executable, str(ROOT / 'fixture.py')] + scenario['args'])
        cols, rows = scenario['size']
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
        os.set_blocking(master, False)
        actual = bytearray()
        echo = bytearray()
        received = bytearray()
        typed = 0
        ended = False
        seen = 0
        status = None
        records = []
        log_pending = b''
        log_offset = 0

        def pump(duration):
            nonlocal ended, seen, records, log_pending, log_offset
            until = time.monotonic() + duration
            while True:
                remaining = max(0, until - time.monotonic())
                ready, _, _ = select.select([master] if not ended else [], [], [], min(.01, remaining))
                if ready:
                    try:
                        data = os.read(master, 65536)
                        actual.extend(data)
                        if not data: ended = True
                    except OSError as exc:
                        if exc.errno == errno.EIO: ended = True
                        elif exc.errno != errno.EAGAIN: raise
                if log.exists():
                    with log.open('rb') as stream:
                        stream.seek(log_offset)
                        chunk = stream.read()
                    log_offset += len(chunk)
                    lines = (log_pending + chunk).split(b'\n')
                    log_pending = lines.pop()
                    for line in lines:
                        record = json.loads(line)
                        # Online validation: future input cannot justify an earlier answer.
                        if record['ev'] in ('write','bell'):
                            assert record['answers'] is None or record['answers'] <= typed, 'answers exceeds bytes typed so far'
                        if record['ev'] == 'read':
                            assert record['consumed'] <= typed, 'read exceeds bytes typed so far'
                        records.append(record)
                if time.monotonic() >= until: break

        try:
            # Synchronize on the initial blocking state, not interpreter startup speed.
            deadline = time.monotonic() + 5
            while not any(r['ev'] == 'block' for r in records):
                pump(.01)
                assert not ended and time.monotonic() < deadline, 'startup failed'
            # A short settling interval also makes the secret prompt's mode observable.
            pump(.02)
            for step in scenario['steps']:
                data = step['keys'].encode('utf-8')
                assert not ended, 'exited before remaining scenario steps'
                attrs = termios.tcgetattr(master)
                expected = data.replace(b'\r', b'\n') if attrs[0] & termios.ICRNL else data
                if attrs[3] & termios.ECHO:
                    assert attrs[3] & termios.ICANON, 'unexpected raw kernel echo'
                    modes = [r for r in records if r['ev'] == 'mode']
                    assert modes and modes[-1]['echo'] and not modes[-1]['raw'], 'kernel echo without logged mode'
                    # Scenarios use printable UTF-8 and Enter in canonical mode.
                    # ICRNL is on, OPOST is off: kernel echo of Enter is LF.
                    assert all(b >= 32 or b in (10,13) for b in data), 'unsupported canonical test key'
                    echo.extend(data.replace(b'\r', b'\n'))
                offset = 0
                while offset < len(data):
                    try:
                        n = os.write(master, data[offset:])
                        offset += n
                        typed += n
                    except BlockingIOError:
                        pump(.01)
                received.extend(expected)
                pump(step['pause_ms'] / 1000)
            deadline = time.monotonic() + 8
            while not ended and time.monotonic() < deadline:
                pump(.02)
            assert ended, 'scenario hung instead of exiting'
            pump(.02)
            _, status = os.waitpid(pid, 0)
            assert os.waitstatus_to_exitcode(status) == 0, 'child failed'
            assert not log_pending, 'partial log record'
            check(records, bytes(actual), bytes(echo), typed, bytes(received))
            # Negative controls prove that the checker rejects corrupted provenance.
            bad = copy.deepcopy(records)
            next(r for r in bad if r['ev'] == 'write')['b64'] = base64.b64encode(b'corruption').decode()
            try:
                check(bad, bytes(actual), bytes(echo), typed, bytes(received))
            except AssertionError:
                pass
            else:
                raise AssertionError('checker accepted a corrupted write')
            bad = copy.deepcopy(records)
            next(r for r in bad if r['ev'] == 'write')['answers'] = typed + 1
            try:
                check(bad, bytes(actual), bytes(echo), typed, bytes(received))
            except AssertionError:
                pass
            else:
                raise AssertionError('checker accepted future answers')
            return '%d terminal bytes; %d records; %d kernel echo bytes' % (len(actual), len(records), len(echo))
        finally:
            if status is None:
                try: os.kill(pid, signal.SIGKILL)
                except ProcessLookupError: pass
                os.waitpid(pid, 0)
            os.close(master)


def main():
    failures = 0
    for scenario in json.loads((ROOT / 'scenarios.json').read_text()):
        try:
            detail = run(scenario)
            print('PASS %s: %s' % (scenario['name'], detail), flush=True)
        except Exception as exc:
            failures += 1
            print('FAIL %s: %s: %s' % (scenario['name'], type(exc).__name__, exc), flush=True)
    return bool(failures)


if __name__ == '__main__':
    sys.exit(main())
