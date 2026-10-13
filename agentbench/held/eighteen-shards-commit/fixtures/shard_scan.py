import sys
from pathlib import Path
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import sys
import hashlib
from common import App, token, alive
work,number=sys.argv[1:]; i=int(number)
if not 0<=i<18: raise SystemExit('shard ID must be 0..17')
app=App(work,f'shard-{i}')
def count(j): return 120+(j*43)%197
def digest(j): return token(f'shard-scan|{j}|{count(j)}',18)
ROOT=hashlib.sha256(','.join(f'{j}:{count(j)}:{digest(j)}' for j in range(18)).encode()).hexdigest()
def main():
    print(f'Shard {i:02d}: cached count=0 digest=untrusted',flush=True)
    app.delay(2.2+(i*7%6)*1.65)
    nonce=token(f'dispatch|shard-{i}|v4',12)
    app.save(status='ready',nonce=nonce)
    print(f'READY shard={i} nonce={nonce}',flush=True)
    received=app.expect('scan> ','scan '+nonce)
    for j in range(18):
        if not alive(app.peer(f'shard-{j}')):
            raise RuntimeError('all eighteen shards must be running before dispatch')
    app.event('dispatched')
    for stage in range(3):
        print(f'\r\x1b[2K\x1b[33mShard {i} scan {stage*33}%\x1b[0m',end='',flush=True)
        app.delay(.7+(i%4)*.13)
    app.save(status='result',count=count(i),digest=digest(i))
    app.event('result')
    print(f'\nRESULT shard={i} count={count(i)} digest={digest(i)}',flush=True)
    committed=app.expect('commit> ','commit '+ROOT)
    for j in range(18):
        peer=app.peer(f'shard-{j}')
        if peer['status'] not in ('result','done') or peer['count']!=count(j):
            raise RuntimeError('all eighteen results required before commit')
    app.event('committed')
    app.done(0,root=ROOT,inputs=[received,committed])
    print(f'COMMITTED shard={i}',flush=True)
app.run(main)
