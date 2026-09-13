#!/usr/bin/env python3
"""One-off orchestration of existing xtask workloads; raw evidence, no pass thresholds."""
import datetime, hashlib, json, os, pathlib, re, subprocess, sys, time
work = pathlib.Path('/tmp/fux-codebase-work')
repo = pathlib.Path('/Users/kisaczka/Desktop/code/fux')
harness = repo/'target/rust-harness/debug/fux-xtask'
products = {name: work/('release-'+name)/'release' for name in ('baseline','candidate')}
output = pathlib.Path(sys.argv[1])
output.mkdir(parents=True, exist_ok=False)
def digest(path):
    with open(path,'rb') as file:
        return hashlib.file_digest(file,'sha256').hexdigest()
def now(): return datetime.datetime.now(datetime.timezone.utc).isoformat()
env = os.environ.copy()
for key in tuple(env):
    if key.startswith(('FUX_', 'ZOR_')): del env[key]
manifest = {'started':now(),'complete':False,'runs':[],'orchestrator_sha256':digest(pathlib.Path(__file__)), 'harness':{'path':str(harness),'sha256':digest(harness)},
    'products':{name:{binary:{'path':str(root/binary),'sha256':digest(root/binary)} for binary in ('fux','zor')} for name,root in products.items()},
    'baseline_qualification':json.loads((work/'performance-baseline-source/BASELINE-IDENTITY.json').read_text()),
    'order':'three blocks, baseline/candidate then candidate/baseline then baseline/candidate per workload',
    'limits':'Focused follow-up of recovery retry timing and pressure CPU differences. Local synthetic workloads; discrete resource observations are not kernel peak or physical disk writes. No release build or Betamax capture overlaps measurement.'}
def save():
    (output/'manifest.tmp').write_text(json.dumps(manifest,indent=2)+'\n')
    (output/'manifest.tmp').replace(output/'manifest.json')
def idle(path):
    # Retain actual host observations; do not turn contention into a product regression.
    observations=[]
    for attempt in range(6):
        result=subprocess.run(['/usr/bin/top','-l','2','-s','1','-n','0'],capture_output=True,text=True,timeout=10)
        observations.append({'at':now(),'exit':result.returncode,'stdout':result.stdout,'stderr':result.stderr})
        path.write_text(json.dumps(observations,indent=2)+'\n')
        values=re.findall(r'([\d.]+)% idle',result.stdout)
        if result.returncode==0 and values and float(values[-1])>=85: return float(values[-1])
        time.sleep(5)
    raise RuntimeError('Host idle precondition not reached; retained observations at '+str(path))
def command(name, root, evidence):
    fux,zor=str(root/'fux'),str(root/'zor')
    return {
        'idle-burst':['measure',fux,'--samples','100'],
        'many-viewers':['measure-frames',fux,'--keystrokes','100','--config','24x80+24x80+24x80+24x80'],
        'resize':['measure-layout',fux],
        'history-memory':['measure-memory',fux,'--scrollback','10000','--rows','24','--columns','80'],
        'scroll-manager':['measure-interactions',fux],
        'recovery':['measure-recovery',fux,zor],
        'journal':['headless-journal','--fux',fux,'--zor',zor,'--output',str(evidence)],
        'pressure':['headless-performance','--fux',fux,'--zor',zor,'--output',str(evidence),'--repetitions','1'],
    }[name]
save()
for block in range(3):
    for workload in ['recovery','pressure']:
        for product in (['baseline','candidate'] if block%2==0 else ['candidate','baseline']):
            label=f'{block+1}-{workload}-{product}'
            folder=output/label;folder.mkdir()
            quiet=idle(folder/'host-before.json')
            argv=[str(harness),*command(workload,products[product],folder/'evidence.json')]
            record={'label':label,'block':block+1,'workload':workload,'product':product,'command':argv,'started':now(),'idle_percent_before':quiet,'complete':False}
            manifest['runs'].append(record);save()
            print('RUN '+label,flush=True)
            begin=time.monotonic()
            with (folder/'stdout.log').open('wb') as stdout, (folder/'stderr.log').open('wb') as stderr:
                result=subprocess.run(argv,cwd=repo,env=env,stdout=stdout,stderr=stderr,timeout=300)
            record.update(exit_code=result.returncode,elapsed_s=time.monotonic()-begin,finished=now())
            save()
            if result.returncode: raise RuntimeError('Workload failed: '+label)
            if workload not in ('journal','pressure'):
                data=json.loads((folder/'stdout.log').read_text())
                (folder/'evidence.json').write_text(json.dumps(data,indent=2)+'\n')
            record['evidence_sha256']=digest(folder/'evidence.json');record['complete']=True;save()
manifest.update(complete=True,finished=now());save()
print('COMPLETE '+str(output),flush=True)
