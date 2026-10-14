import json
import os
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    root = Path(work)
    try:
        def rec(name):
            assert not (root / '.fx' / (name + '.duplicate.json')).exists(), name + ': worker ID restarted'
            row = json.loads((root / '.fx' / (name + '.json')).read_text())
            assert row.get('error') is None, name + ': fixture rejected input'
            assert row.get('stdin_tty') is True and row.get('stdout_tty') is True, name + ': terminal execution missing'
            assert (root / '.fx' / (name + '.pid')).read_text() == str(row['pid']), name + ': PID record is missing or inconsistent'
            if row.get('state') in ('done', 'crashed'):
                assert row.get('exited') is True, name + ': program has not completed teardown'
                try:
                    os.kill(row['pid'], 0)
                except ProcessLookupError:
                    pass
                else:
                    stat = Path(f"/proc/{row['pid']}/stat").read_text()
                    assert stat.rsplit(')', 1)[1].split()[0] == 'Z', name + ': program is still running'
            return row
        def answer(expected):
            assert (root / 'answer.txt').read_bytes() == expected.encode(), 'answer.txt differs from the exact expected answer'
        expected = ''
        pids = set()
        for wid in range(1, 9):
            r = rec(f'worker_{wid}')
            pids.add(r['pid'])
            attempts = 2 if wid % 3 == 2 else 1
            value = wid * wid * 37 + 113
            assert r['state'] == 'done' and r['value'] == value and r['attempts'] == attempts and r['all_started'] is True
            assert r['id'] == wid and r['exit_code'] == 0
            assert r['begin'] == f'W{wid}-{(wid * 173 + 61) % 1000:03d}'
            if attempts == 2:
                assert r['retry'] == f'R{wid}-{(wid * 97 + 37) % 997:03d}'
            else:
                assert 'retry' not in r
            expected += f'{wid}\t{value}\t{attempts}\n'
        assert len(pids) == 8, 'workers must be eight distinct processes'
        answer(expected)
        return True, 'all required fixture states and exact results verified'
    except (OSError, ValueError, KeyError, AssertionError, TypeError, AttributeError, IndexError, OverflowError) as exc:
        return False, str(exc) or 'missing or incorrect final state'

if __name__ == '__main__':
    import sys
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    raise SystemExit(0 if ok else 1)
