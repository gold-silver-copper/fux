import hashlib
import json
import os
from pathlib import Path

CFG = {'id': 'TW-022', 'title': 'Reconcile a locked migration against a live database', 'category': 'panes', 'roles': {'database': ['catalog', 'snapshot', 'describe', 'checksum', 'freeze', 'export', 'retain', 'seal'], 'migration': ['attach', 'plan', 'dry-run', 'inspect-lock', 'release-lock', 'apply', 'verify', 'seal'], 'query': ['connect', 'explain', 'count', 'join', 'constraints', 'rollback-check', 'report', 'seal']}, 'imports': {'migration:0': 'database', 'query:0': 'migration'}, 'order': ['database', 'migration', 'query'], 'size': [90, 18], 'busy': {'database:3': 7, 'migration:5': 13}, 'recovery': {'migration:3': 'The migration is blocked by an abandoned advisory lock. Inspect the owner before releasing it.'}, 'archive_role': 'database', 'archive_steps': [2], 'flood': 360, 'kinds': ['several-programs', 'long-horizon', 'recovery', 'echo-traps', 'reading', 'layout-size'], 'needs': ['start-program', 'type-text', 'wait-text', 'wait-exit', 'read-screen', 'read-scrollback', 'set-size', 'several-panes', 'side-by-side'], 'story': 'Keep database, migration, and query running concurrently in separate terminal views, with database and migration side by side. Each view needs at least 90 columns and 18 rows when commands are submitted. Finish the database snapshot dialogue first. Its describe challenge is in the audit archive before the log flood. Transfer the database RESULT receipt to migration, resolve the displayed lock incident in the displayed order, finish the migration, and transfer its RESULT receipt to query. Complete all query checks.'}

def digest(s):
    return hashlib.sha256((CFG['id']+'|'+s).encode()).hexdigest()[:12]

def code(role,step):
    return digest('challenge|'+role+'|'+str(step))

def job_status(role):
    return 1+int(digest('exit|'+role)[:4],16)%47

def token(role,step):
    source=CFG.get('imports',{}).get(role+':'+str(step))
    if source:
        return (str(job_status(source))+' ' if CFG.get('shell') and source.startswith('j') else '')+receipt(source)
    return code(role,step)

def transcript(role):
    return [verb+' '+token(role,i) for i,verb in enumerate(CFG['roles'][role])]

def receipt(role):
    return digest('receipt|'+role+'|'+'|'.join(transcript(role)))

def check(work: str) -> tuple[bool,str]:
    root=Path(work)
    try:
        records={role:json.loads((root/'.fx'/(role+'.json')).read_text()) for role in CFG['roles']}
        for role,r in records.items():
            if r.get('role')!=role or not r.get('tty') or r.get('early'):
                return False,role+': missing terminal attestation or early input'
            if not r.get('success') or r.get('phase')!='closed':
                return False,role+': did not finish and close'
            if r.get('received')!=transcript(role) or r.get('receipt')!=receipt(role):
                return False,role+': wrong interaction history or receipt'
            if not CFG.get('shell') and r.get('all_finished')!={name:v['pid'] for name,v in records.items()}:
                return False,role+': a program closed before all current roles finished'
            if (root/'.fx'/(role+'.pid')).read_text()!=str(r['pid'])+'\n':
                return False,role+': pid record mismatch'
            try:
                os.kill(r['pid'],0)
            except ProcessLookupError:
                pass
            else:
                return False,role+': process remains alive'
            if CFG.get('shell') and role.startswith('j'):
                if 'i' not in r.get('shell_flags','') or r.get('reported_exit')!=job_status(role):
                    return False,role+': wrong interactive-shell or exit attestation'
                peers=['monitor']
            else:
                peers=[] if CFG.get('shell') else [x for x in CFG['roles'] if x!=role]
            concurrent=r.get('concurrent',{})
            if set(concurrent)!=set(peers):
                return False,role+': missing simultaneous peer attestation'
            terminals=[r['terminal']]+[concurrent[x]['terminal'] for x in peers]
            if len(terminals)!=len(set(terminals)):
                return False,role+': simultaneous peers shared a terminal'
            for peer in peers:
                if concurrent[peer]['pid']!=records[peer]['pid']:
                    return False,role+': a concurrent peer was replaced'
            expected_dependencies=[CFG['imports'][role+':'+str(i)] for i in range(len(CFG['roles'][role])) if role+':'+str(i) in CFG.get('imports',{})]
            if r.get('dependencies',[])!=expected_dependencies:
                return False,role+': missing ordered dependencies'
            if role in CFG.get('tui',[]) and r.get('menu_actions')!=CFG['roles'][role]:
                return False,role+': console actions incomplete'
            incident_steps=[i for i in range(len(CFG['roles'][role])) if role+':'+str(i) in CFG.get('recovery',{})]
            if r.get('recovery_seen',[])!=incident_steps:
                return False,role+': incident was not handled'
            if 'size' in CFG:
                sizes=r.get('sizes',[])
                if len(sizes)!=len(CFG['roles'][role]) or any(w<CFG['size'][0] or h<CFG['size'][1] for w,h in sizes):
                    return False,role+': terminal size requirement not met'
        if CFG.get('resize_step') and not records['release'].get('resize_verified'):
            return False,'release was not enlarged'
        if CFG.get('shell'):
            jobs=[records['j'+str(i).zfill(2)] for i in range(1,7)]
            if len({r.get('shell_parent_pid') for r in jobs})!=1 or not jobs[0].get('shell_parent_pid'):
                return False,'jobs did not return to the same interactive shell'
            if any(a.get('job_closed_ns',0)>=b.get('job_started_ns',0) for a,b in zip(jobs,jobs[1:])):
                return False,'jobs were not run sequentially in numeric order'
        if CFG.get('quiet_step') and not records['gate'].get('quiet_verified'):
            return False,'quiet interval was not observed'
        if CFG.get('session'):
            r=records['daemon']
            if r.get('park_pid')!=r['pid'] or r.get('resume_pid')!=r['pid'] or r.get('park_progress',0)<=r.get('park_ledger_count',0):
                return False,'daemon did not survive ledger progress while parked'
        lines=[]
        for role in CFG['order']:
            if CFG.get('shell') and role.startswith('j'):
                lines.append(role+' status='+str(job_status(role))+' receipt='+receipt(role))
            else:
                lines.append(role+'='+receipt(role))
        expected='\n'.join(lines)+'\n'
        if (root/'answer.txt').read_bytes()!=expected.encode():
            return False,'answer.txt must exactly match observed receipts/statuses in the specified order'
        return True,'all terminal dialogues, dependencies, exits, and exact answer verified'
    except (OSError,ValueError,KeyError,TypeError) as exc:
        return False,'missing or invalid final evidence: '+str(exc)
