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
        for name, content in {'jobs.txt': 'partition-00|rows=97|salt=cedar-31\npartition-01|rows=110|salt=cedar-32\npartition-02|rows=123|salt=cedar-35\npartition-03|rows=136|salt=cedar-40\npartition-04|rows=149|salt=cedar-47\npartition-05|rows=162|salt=cedar-56\npartition-06|rows=175|salt=cedar-67\npartition-07|rows=188|salt=cedar-80\npartition-08|rows=201|salt=cedar-95\npartition-09|rows=214|salt=cedar-112\npartition-10|rows=227|salt=cedar-131\npartition-11|rows=240|salt=cedar-152\npartition-12|rows=253|salt=cedar-175\npartition-13|rows=266|salt=cedar-200\npartition-14|rows=279|salt=cedar-227\npartition-15|rows=292|salt=cedar-256\n'}.items():
            require((w / name).read_text() == content, "setup file changed: " + name)
        jobs = (w / 'jobs.txt').read_text().splitlines()
        shards = [record(f'shard-{i:02d}') for i in range(16)]
        m = record('merge')
        results = []
        for i, r in enumerate(shards):
            name = f'shard-{i:02d}'
            challenge = digest(name + ':ready:' + jobs[i])
            result = digest(name + ':result:' + jobs[i] + ':' + challenge,16)
            require(r['phase'] == 'complete' and r['index'] == i and r['exit_code'] == 0 and r['result'] == result and r['challenge'] == challenge, f'{name} not completed correctly')
            require(r['received'] == ['run ' + challenge, 'accept ' + result], f'{name} input differs')
            results.append(result)
        require(max([r['started_at'] for r in shards] + [m['started_at']]) < min(r['accepted_at'] for r in shards), 'not all seventeen programs overlapped')
        verify = digest('verify:' + '|'.join(results))
        receipt = digest('merge:' + '|'.join(results),24)
        require(m['phase'] == 'complete' and m['results'] == results and m['exit_code'] == 0 and m['receipt'] == receipt and m['count'] == len(results), 'merge incomplete')
        require(m['received'] == [f'shard-{i:02d} ' + results[i] for i in range(16)] + ['confirm ' + verify], 'collector order/confirmation differs')
        answer(f'count={m["count"]}\nmerge={receipt}\n')
        return True, 'all required outcomes verified'
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return False, str(exc)

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    raise SystemExit(0 if ok else 1)
