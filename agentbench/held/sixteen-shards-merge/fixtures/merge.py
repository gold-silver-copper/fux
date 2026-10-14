import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *
f = Fixture(sys.argv[1], 'merge')
f.save(started_at=time.monotonic())
jobs = (f.work / 'jobs.txt').read_text().splitlines()
results = []
for i in range(16):
    name = f'shard-{i:02d}'
    challenge = digest(name + ':ready:' + jobs[i])
    result = digest(name + ':result:' + jobs[i] + ':' + challenge,16)
    f.expect('collect ' + name + '> ', name + ' ' + result)
    r = json.loads((f.fx / (name + '.json')).read_text())
    if r['phase'] != 'complete' or r['result'] != result:
        f.fail('collect only accepted shard results')
    results.append(result)
    print('COLLECTED ' + name, flush=True)
print('Verifying ordered merge ...', flush=True)
f.pause(2.7)
verify = digest('verify:' + '|'.join(results))
print('VERIFY key=' + verify, flush=True)
f.expect('confirm> ', 'confirm ' + verify)
receipt = digest('merge:' + '|'.join(results),24)
f.done(results=results, count=len(results), receipt=receipt, exit_code=0)
print(f'MERGE COMPLETE count={len(results)} receipt={receipt}', flush=True)
