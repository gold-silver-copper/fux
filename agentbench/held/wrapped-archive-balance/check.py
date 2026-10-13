import json
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    WORK = w.resolve()
    def seed_token(label, prefix='', length=4):
        # The resolved work path is a per-validation seed shared by related programs.
        import hashlib
        return prefix + hashlib.sha256((str(WORK) + ':' + label).encode()).hexdigest()[:length]

    archive = seed_token('archive', 'ARCH-', 4)
    checksum = seed_token('checksum', '', 6)
    row_count = 1200 + int(seed_token('row-count', '', 4), 16) % 101
    balance_final = 400 + int(seed_token('balance_final', '', 4), 16) % 500
    try:
        for path in (w / '.fx').glob('*.json'):
            record = json.loads(path.read_text())
            assert record.get('runs') == 1, "a fixture was restarted"
            assert record.get('terminal') == dict(stdin=True, stdout=True), "fixture did not run in a terminal"
        s = json.loads((w / '.fx/archive-reader.json').read_text())
        assert s['complete'] and s['exit'] == 0 and s['exported'], 'export was not accepted'
        assert (s['columns'], s['rows']) == (64, 18), 'wrapping was not exercised'
        assert s['history'] == [('export ' + archive + ' ' + checksum + ' ' + str(balance_final))], 'incorrect archive extraction'
        expected = ('archive=' + archive + '\nchecksum=' + checksum + '\nbalance=' + str(balance_final) + '\nfooter source=cedar\nfooter rows=' + str(row_count) + '\nfooter status=sealed\n')
        assert (w / 'answer.txt').read_text() == expected, 'incorrect current value or footer'
        return True, "all required terminal interactions and final values verified"
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, str(exc) or "missing or incorrect final state"
