from pathlib import Path
import sys
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import sys
import time
import util as u
work, text = sys.argv[1:]
index = int(text)
if not 1 <= index <= 8:
    raise SystemExit('index must be 1..8')
name = f'reducer-{index}'
u.init(work, name)
if len(list(u.root.glob(name + '.json'))) != 1:
    u.fail('Reducer registration failed')
u.event('index', index=index)
u.barrier([f'reducer-{i}' for i in range(1, 9)])
u.out(f'worker {index}: COMMIT_READY? no; reduction is in progress')
time.sleep(1 + (index * 3) % 7)
if index % 2:
    token = f'R{index}-{index*31+7}'
    u.ask('RETRY_REQUIRED ' + token, 'retry ' + token)
    u.event('retried', retried=True)
    u.out('Retry accepted; rebuilding reduction state...')
    time.sleep(2.5)
token = f'C{index}-{index*43+11}'
u.ask('COMMIT_READY ' + token, 'collect ' + token)
result = index ** 3 + 17
u.out(f'RESULT {index}:{result}')
u.finish(0, result=result, collected=True)
