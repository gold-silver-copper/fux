import hashlib,json,os,pathlib,subprocess,time
repo=pathlib.Path('/Users/kisaczka/Desktop/code/fux')
work=pathlib.Path('/tmp/fux-codebase-work')
root=work/'pressure-reproduction-20260913'
root.mkdir(exist_ok=False)
harness=repo/'target/rust-harness/debug/fux-xtask'
fux=work/'release-candidate/release/fux';zor=work/'release-candidate/release/zor'
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
env=os.environ.copy()
for key in tuple(env):
 if key.startswith(('FUX_','ZOR_')):del env[key]
m={'complete':False,'purpose':'Fixed 200-case correctness reproduction; stop at first failure, no performance conclusions','planned_batches':10,'repetitions_per_batch':5,'cases_per_repetition':4,'harness_sha256':digest(harness),'fux_sha256':digest(fux),'zor_sha256':digest(zor),'runs':[]}
def save():(root/'manifest.json').write_text(json.dumps(m,indent=2)+'\n')
save()
for index in range(10):
 folder=root/f'batch-{index+1}';folder.mkdir()
 argv=[str(harness),'headless-performance','--fux',str(fux),'--zor',str(zor),'--output',str(folder/'evidence.json'),'--repetitions','5']
 record={'index':index+1,'argv':argv,'complete':False};m['runs'].append(record);save()
 print('START',index+1,flush=True);start=time.monotonic()
 with (folder/'stdout.log').open('wb') as out,(folder/'stderr.log').open('wb') as err:
  result=subprocess.run(argv,cwd=repo,env=env,stdout=out,stderr=err,timeout=300)
 record.update(exit_code=result.returncode,elapsed_s=time.monotonic()-start);save()
 if result.returncode:raise SystemExit('Failure retained in '+str(folder))
 record['complete']=True;record['evidence_sha256']=digest(folder/'evidence.json');save();print('PASS',index+1,flush=True)
m['complete']=True;save();print('COMPLETE',flush=True)
