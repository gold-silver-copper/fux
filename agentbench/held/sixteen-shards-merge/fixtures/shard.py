import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *
work, text_index = sys.argv[1:]
i = int(text_index)
if not 0 <= i < 16: raise SystemExit(2)
name = f'shard-{i:02d}'
f = Fixture(work, name)
jobs = (f.work / 'jobs.txt').read_text().splitlines()
challenge = digest(name + ':ready:' + jobs[i])
result = digest(name + ':result:' + jobs[i] + ':' + challenge, 16)
f.save(index=i, started_at=time.monotonic())
print(name + ' loading shard dictionary ...', flush=True)
f.pause(.9 + (i % 5) * 1.8)
print('\x1b[32mREADY ' + name + ' challenge=' + challenge + '\x1b[0m', flush=True)
f.expect('run> ', 'run ' + challenge)
print('DONE dry-run result=' + digest('dry:' + jobs[i],16) + ' (not committed)', flush=True)
f.pause(1.2 + ((15-i) % 4) * .7)
print('\r\x1b[2KRESULT ' + name + '=' + result, flush=True)
f.expect('accept> ', 'accept ' + result)
f.done(index=i, challenge=challenge, result=result, accepted_at=time.monotonic(), exit_code=0)
print('SHARD ACCEPTED ' + name, flush=True)
