import json
from pathlib import Path
import sys
import hashlib

def digest(text, n=12):
    return hashlib.sha256(text.encode()).hexdigest()[:n]

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    def record(name):
        value = json.loads((w / '.fx' / (name + '.json')).read_text())
        require(value['launch_number'] == 1 and len(list((w / '.fx').glob(name + '.launch-*.pid'))) == 1, 'program launched more than once: ' + name)
        require((w / '.fx' / (name + '.pid')).read_text() == str(value['pid']), 'pid record differs: ' + name)
        require(value['stdin_tty'] is True and value['stdout_tty'] is True, 'program did not run with terminal stdin and stdout: ' + name)
        return value
    def require(condition, reason):
        if not condition:
            raise ValueError(reason)
    def answer(expected):
        require((w / 'answer.txt').read_text() == expected, 'answer.txt differs from the exact expected text')
    try:
        for name, content in {'tickets.json': '[\n  {\n    "id": "Q-42",\n    "severity": "low",\n    "owner": "noah",\n    "revision": 1,\n    "description": "Mirror of Q-24; keep this ticket unchanged."\n  },\n  {\n    "id": "Q-24",\n    "severity": "low",\n    "owner": "mina",\n    "revision": 1,\n    "description": "Inbound receipt mismatch; preserve this description."\n  },\n  {\n    "id": "R-00",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 0."\n  },\n  {\n    "id": "R-01",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 1."\n  },\n  {\n    "id": "R-02",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 2."\n  },\n  {\n    "id": "R-03",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 3."\n  },\n  {\n    "id": "R-04",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 4."\n  },\n  {\n    "id": "R-05",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 5."\n  },\n  {\n    "id": "R-06",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 6."\n  },\n  {\n    "id": "R-07",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 7."\n  },\n  {\n    "id": "R-08",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 8."\n  },\n  {\n    "id": "R-09",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 9."\n  },\n  {\n    "id": "R-10",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 10."\n  },\n  {\n    "id": "R-11",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 11."\n  },\n  {\n    "id": "R-12",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 12."\n  },\n  {\n    "id": "R-13",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 13."\n  },\n  {\n    "id": "R-14",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 14."\n  },\n  {\n    "id": "R-15",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 15."\n  },\n  {\n    "id": "R-16",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 16."\n  },\n  {\n    "id": "R-17",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 17."\n  },\n  {\n    "id": "R-18",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 18."\n  },\n  {\n    "id": "R-19",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 19."\n  },\n  {\n    "id": "R-20",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 20."\n  },\n  {\n    "id": "R-21",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 21."\n  },\n  {\n    "id": "R-22",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 22."\n  },\n  {\n    "id": "R-23",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 23."\n  },\n  {\n    "id": "R-24",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 24."\n  },\n  {\n    "id": "R-25",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 25."\n  },\n  {\n    "id": "R-26",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 26."\n  },\n  {\n    "id": "R-27",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 27."\n  },\n  {\n    "id": "R-28",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 28."\n  },\n  {\n    "id": "R-29",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 29."\n  },\n  {\n    "id": "R-30",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 30."\n  },\n  {\n    "id": "R-31",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 31."\n  },\n  {\n    "id": "R-32",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 32."\n  },\n  {\n    "id": "R-33",\n    "severity": "medium",\n    "owner": "team",\n    "revision": 1,\n    "description": "Routine inspection 33."\n  }\n]\n', 'notebook-seed.txt': 'incident-copper-82\n'}.items():
            require((w / name).read_text() == content, "setup file changed: " + name)
        r = record('notebook')
        expected = json.loads((w / 'tickets.json').read_text())
        initial_revision = next(x['revision'] for x in expected if x['id'] == 'Q-24')
        revision = initial_revision + 1
        for x in expected:
            if x['id'] == 'Q-24': x.update(severity='high', owner='ivy', revision=revision)
        seed = (w / 'notebook-seed.txt').read_text().strip()
        approval = digest('D-07:' + seed)
        receipt = digest(seed + ':Q-24:high:ivy:' + str(revision) + ':' + approval)
        require(r['phase'] == 'complete' and r['tickets'] == expected and r['revision'] == revision, 'saved incidents differ')
        require(r['help_seen'] and r['reloaded'] and r['modal_approved'] and r['receipt'] == receipt, 'pager/reload/confirmation missing')
        require(r['attempts'] == [{'id':'Q-24', 'severity':'high', 'owner':'ivy', 'code':approval, 'revision':v} for v in (initial_revision, revision)], 'save attempt sequence differs')
        answer(f'ticket=Q-24\nseverity=high\nowner=ivy\nrevision={revision}\nreceipt={receipt}\n')
        return True, 'all required outcomes verified'
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return False, str(exc)

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    raise SystemExit(0 if ok else 1)
