import json
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    WORK = w.resolve()
    def seed_token(label, prefix='', length=4):
        # The resolved work path is a per-validation seed shared by related programs.
        import hashlib
        return prefix + hashlib.sha256((str(WORK) + ':' + label).encode()).hexdigest()[:length]

    opening = seed_token('opening', 'opening-', 4)
    settled = seed_token('settled', 'settled-', 4)
    try:
        for path in (w / '.fx').glob('*.json'):
            record = json.loads(path.read_text())
            assert record.get('runs') == 1, "a fixture was restarted"
            assert record.get('terminal') == dict(stdin=True, stdout=True), "fixture did not run in a terminal"
        import os
        s = json.loads((w / '.fx/ledger-watch.json').read_text())
        assert s['verified'] and s['tick'] == 8 and s['history'] == [('mark ' + opening), ('mark ' + settled)], 'same process did not retain both marks'
        assert s['settled_at'] - s['opening_at'] >= 8, 'ledger did not advance unattended'
        assert not s['complete'] and s['exit'] is None, 'ledger was stopped'
        assert int((w / '.fx/ledger-watch.pid').read_text()) == s['pid'], 'process identity changed'
        os.kill(s['pid'], 0)
        assert s['heartbeat'] >= s['opening_at'], 'ledger never advanced its heartbeat'
        stat = Path('/proc/%d/stat' % s['pid']).read_text().rsplit(')', 1)[1].split()
        assert stat[0] not in ('Z', 'T', 't'), 'ledger is no longer running'
        assert Path('/proc/%d/fd/0' % s['pid']).resolve() == Path('/proc/%d/fd/1' % s['pid']).resolve(), 'ledger terminal changed'
        assert (w / 'answer.txt').read_text() == ('opening=' + opening + '\nsettled=' + settled + '\n'), 'wrong checkpoint report'
        return True, "all required terminal interactions and final values verified"
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, str(exc) or "missing or incorrect final state"
