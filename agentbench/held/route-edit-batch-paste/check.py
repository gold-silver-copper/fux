import json
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    WORK = w.resolve()
    def seed_token(label, prefix='', length=4):
        # The resolved work path is a per-validation seed shared by related programs.
        import hashlib
        return prefix + hashlib.sha256((str(WORK) + ':' + label).encode()).hexdigest()[:length]

    current_password = seed_token('current_password', 'plum-', 4)
    try:
        for path in (w / '.fx').glob('*.json'):
            record = json.loads(path.read_text())
            assert record.get('runs') == 1, "a fixture was restarted"
            assert record.get('terminal') == dict(stdin=True, stdout=True), "fixture did not run in a terminal"
        s = json.loads((w / '.fx/batch-console.json').read_text())
        assert s['complete'] and s['exit'] == 0 and s['saved'], 'console was not saved'
        assert s['password'] == (current_password) and s['route'] == 'east-27', 'wrong credential or route'
        assert s['payload'] == 'run = "$PATH"\n\nliteral = `date`\nfinish = true\n', 'paste changed'
        assert s['paste_lines'] == len((w / 'batch.txt').read_text().splitlines()), 'wrong paste count'
        assert (w / 'answer.txt').read_text() == 'route=' + s['route'] + '\npaste-lines=' + str(s['paste_lines']) + '\n', 'wrong answer'
        return True, "all required terminal interactions and final values verified"
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, str(exc) or "missing or incorrect final state"
