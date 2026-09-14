"""Actual dashboard -> remote exact viewer -> dashboard under a controlling PTY.
Requires explicit FUX_BIN, ZOR_BIN, KOH_BIN; every service is disposable.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import selectors
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time

arguments = argparse.ArgumentParser(description=__doc__)
arguments.add_argument('--initial-machine', choices=('first', 'local'))
arguments.add_argument('--without-local-runtime', action='store_true')
arguments.add_argument('--observed-agent', action='store_true')
arguments.add_argument('--reload-catalog', action='store_true')
arguments.add_argument('--notifications', action='store_true')
arguments.add_argument('--transport-faults', action='store_true')
arguments.add_argument('--restart-zor', action='store_true')
arguments.add_argument('--restart-controller', action='store_true')
arguments.add_argument('--restart-fux', action='store_true')
arguments.add_argument('--resume-refusal', action='store_true')
arguments.add_argument('--dashboard-resume', action='store_true')
arguments.add_argument('--columns', type=int, choices=(40,80,180), default=180)
arguments.add_argument('--rows', type=int, choices=(16,24,30), default=30)
arguments.add_argument('--artifacts-dir', type=Path, default=Path(__file__).resolve().parent)
options = arguments.parse_args()
out = options.artifacts_dir.resolve()
out.mkdir(parents=True, exist_ok=True)
bins = {key: str(Path(os.environ[key]).resolve(strict=True)) for key in ('FUX_BIN', 'ZOR_BIN', 'KOH_BIN')}
if options.transport_faults:
    bins['FAULT_BIN'] = str(Path(os.environ['KOH_FAULT_SERVER_BIN']).resolve(strict=True))
children, logs = [], []
helpers_before = set(Path('/tmp').glob('zor-gw-*'))
with tempfile.TemporaryDirectory(prefix='zmh-', dir='/tmp') as directory:
    root = Path(directory)
    def environment(path):
        return dict(PATH=os.environ.get('PATH', '/usr/bin:/bin'), HOME=str(path), XDG_RUNTIME_DIR=str(path),
                    XDG_CONFIG_HOME=str(path/'config'), XDG_STATE_HOME=str(path/'state'), SHELL='/bin/sh', TERM='xterm-256color',
                    KOH_KEY_PASSPHRASE='disposable-handoff-fixture', KOH_KEY_NEW_PASSPHRASE='disposable-handoff-fixture')
    env = environment(root)
    def run(binary, *args, target_env=env, expect_success=True):
        p = subprocess.Popen([bins[binary], *map(str, args)], cwd=root, env=target_env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        deadline = time.monotonic()+15
        while True:
            try:
                stdout, stderr = p.communicate(timeout=.05)
                break
            except subprocess.TimeoutExpired:
                if time.monotonic() >= deadline:
                    p.kill()
                    p.communicate()
                    raise
                if 'captured' in globals() and dashboard.poll() is None:
                    with selectors.DefaultSelector() as selector:
                        selector.register(master, selectors.EVENT_READ)
                        if selector.select(0): captured.extend(os.read(master,65536))
        assert (p.returncode == 0) == expect_success, (args, p.returncode, stderr)
        return stdout if expect_success else stderr
    def spawn(binary, *args, target_env=env, advertise=False):
        log = open(out/f'process-{len(logs)}.log', 'wb')
        logs.append(log)
        child = subprocess.Popen([bins[binary], *map(str,args)], cwd=root, env=target_env,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE if advertise else log,
                                 stderr=log, start_new_session=True)
        children.append(child)
        return child
    def until(probe, timeout=8):
        deadline = time.monotonic()+timeout
        while True:
            value = probe()
            if value:
                return value
            assert time.monotonic() < deadline, 'condition timeout'
            time.sleep(.02)
    def gateway(path, key, identity):
        if options.transport_faults and path == root/'first/fux/agent.attach.sock':
            fault = root/'fault'
            fault.mkdir(mode=0o700)
            (fault/'config.json').write_text(json.dumps(dict(socket=str(path),key_file=str(key),allow=identity)))
            child = spawn('FAULT_BIN', 'gateway::fault_fixture::controlled_gateway', '--exact', '--ignored', '--nocapture',
                          target_env=dict(env,KOH_FAULT_FIXTURE=str(fault)))
            until(lambda: (fault/'ready.json').exists())
            assert child.poll() is None
            return json.loads((fault/'ready.json').read_text())
        child = spawn('KOH_BIN', 'gateway', 'serve', '--socket', path, '--key-file', key,
                      '--allow', identity, '--local', advertise=True)
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            assert selector.select(10), 'advertisement timeout'
            return json.loads(child.stdout.readline())
    def control(path, request):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
            stream.settimeout(3)
            stream.connect(str(path/'fux/agent.sock'))
            stream.sendall(b'FUX\n')
            reader = stream.makefile('rb')
            assert reader.read(4) == b'FUX\n', 'control preface mismatch'
            stream.sendall(json.dumps(dict(request,id=1)).encode()+b'\n')
            reply = json.loads(reader.readline(1048576))
            assert reply['status'] == 'completed', reply
            return reply['result'].get('value')
    try:
        spawn('FUX_BIN', 'serve')
        spawn('ZOR_BIN', 'serve')
        until(lambda: (root/'zor/control.sock').exists())
        key = root/'client.key'
        identity = run('KOH_BIN', 'id', '--key-file', key).strip()
        hosts = []
        adopted = {}
        zor_servers = {}
        fux_servers = {}
        for name in ('first', 'second'):
            path = root/name
            path.mkdir(mode=0o700)
            host_env = environment(path)
            fux_servers[name] = spawn('FUX_BIN', 'serve', target_env=host_env)
            zor_servers[name] = spawn('ZOR_BIN', *(['--agent', 'codex', 'serve'] if options.observed_agent else ['serve']), target_env=host_env)
            until(lambda: (path/'zor/control.sock').exists() and (path/'fux/default.sock').exists())
            run('FUX_BIN', 'workspace', 'new', 'agent', target_env=host_env)
            until(lambda: (path/'fux/agent.sock').exists())
            listing = control(path, dict(command='list'))
            workspace = next(ws for ws in listing['workspaces'] if ws['name'] == 'agent')
            pane = workspace['tabs'][0]['panes'][0]
            run('ZOR_BIN', 'task', 'adopt', 'same', '--title', name+' task', '--instance', listing['instance'],
                '--workspace', 'agent', '--pane', pane['id'], target_env=host_env)
            adopted[name] = pane['id']
            if options.observed_agent:
                extra = control(path, dict(command='split', axis='horizontal', argv=['/bin/sh'], final_retain_ms=60000))['pane']
                listing = control(path, dict(command='list'))
                pane = next(pane for ws in listing['workspaces'] for tab in ws['tabs'] for pane in tab['panes'] if pane['id'] == extra)
            ad = gateway(path/'zor/control.sock', root/(name+'-control.key'), identity)
            run('ZOR_BIN', 'machine', 'add', name, '--endpoint', ad['endpoint_id'], '--key-file', key,
                '--direct', ad['direct_addr'])
            # Only the first machine receives an explicit attachment grant/binding.
            if name == 'first':
                attachment = gateway(path/'fux/agent.attach.sock', root/(name+'-attach.key'), identity)
                run('ZOR_BIN', 'machine', 'bind', name, '--workspace', 'agent', '--endpoint', attachment['endpoint_id'],
                    '--key-file', key, '--direct', attachment['direct_addr'])
            hosts.append((path, host_env, pane))
        aggregate = json.loads(run('ZOR_BIN', '--koh-binary', bins['KOH_BIN'], 'dashboard', '--all-machines', '--once'))
        assert all(m['fresh'] for m in aggregate['machines']), aggregate
        (out/'aggregate.json').write_text(json.dumps(aggregate,indent=2))
        task_indexes = {}
        for machine in aggregate['machines']:
            if machine['name'] == 'Local':
                continue
            pane = hosts[0 if machine['name'] == 'first' else 1][2]['id']
            task_indexes[machine['name']] = next(i for i,row in enumerate(machine['view']['rows'])
                if ((row['kind'] == 'observation' and (row.get('target') or {}).get('pane') == pane)
                    if options.observed_agent else (row.get('expected') or {}).get('task') == 'same'))
        inspection_needle = (f"first / observed agent / pane {hosts[0][2]['id']}".encode()
                             if options.observed_agent else b'first / same / attempt')
        def observed_request(handle):
            machine = next(machine for machine in aggregate['machines'] if machine['name'] == 'first')
            request = dict(v=1,id=81,op='task',service_instance=machine['view']['service_instance'],
                           task=dict(action='observed-attachment',handle=handle))
            with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
                stream.settimeout(3)
                stream.connect(str(hosts[0][0]/'zor/control.sock'))
                stream.sendall(json.dumps(request).encode()+b'\n')
                return json.loads(stream.makefile('rb').readline(524289))
        if options.observed_agent:
            first_view = next(machine['view'] for machine in aggregate['machines'] if machine['name'] == 'first')
            target = first_view['rows'][task_indexes['first']]['target']
            observed_handle = {key:target[key] for key in ('instance','workspace','stream','pane','pid')}
            assert observed_request(observed_handle)['status'] == 'completed'
            assert observed_request(dict(observed_handle,pid=observed_handle['pid']+1))['status'] == 'failed', 'stale PID accepted'
            assert observed_request(dict(observed_handle,runtime=str(root/'forbidden')))['status'] == 'failed', 'caller runtime accepted'
        master, slave = pty.openpty()
        before = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', options.rows, options.columns, 0, 0))
        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
            assert os.tcgetpgrp(0) == os.getpgrp(), 'dashboard is not the foreground terminal group'
        log = open(out/'dashboard.stderr', 'wb')
        logs.append(log)
        # Keep the controlling session leader alive through the attribute capture.
        # macOS revokes its slave terminal when that leader exits.
        launcher = '''import json, subprocess, sys, termios
result = subprocess.run(sys.argv[2:])
attributes = termios.tcgetattr(0)
attributes[3] &= ~getattr(termios, 'PENDIN', 0)
attributes[6] = [value[0] if isinstance(value, bytes) else value for value in attributes[6]]
with open(sys.argv[1], 'w') as output:
    json.dump(attributes, output)
sys.exit(result.returncode)
'''
        dashboard_args = (['--machine', options.initial_machine, 'dashboard'] if options.initial_machine
                          else ['dashboard', '--all-machines'])
        if options.notifications:
            notifier = root/'notifier.py'
            notifier.write_text('#!/usr/bin/python3\nimport json,sys\nwith open('+repr(str(root/'notices.jsonl'))+', "a") as output: output.write(json.dumps(sys.argv[1:])+"\\n")\n')
            notifier.chmod(0o700)
            dashboard_args += ['--bell', '--notify', '--notification-command', str(notifier)]
        dashboard_env = dict(env)
        if options.without_local_runtime:
            dashboard_env.pop('HOME', None)
            dashboard_env.pop('XDG_RUNTIME_DIR', None)
        dashboard = subprocess.Popen([sys.executable, '-c', launcher, str(out/'restored-termios.json'),
                                      bins['ZOR_BIN'], '--koh-binary', bins['KOH_BIN'], '--fux-binary', bins['FUX_BIN'],
                                      *dashboard_args], cwd=root, env=dashboard_env, stdin=slave, stdout=slave,
                                     stderr=log, preexec_fn=controlling_terminal)
        children.append(dashboard)
        captured = bytearray()
        cursor = 0
        def wait_output(needle, timeout=12, cell_output=False):
            deadline = time.monotonic()+timeout
            with selectors.DefaultSelector() as selector:
                selector.register(master, selectors.EVENT_READ)
                def observed():
                    data = bytes(captured[cursor:])
                    if b'\x1b' in needle:
                        return data
                    if cell_output or options.columns < 180:
                        data = re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', data)
                    if options.columns < 180:
                        data = data.replace(b'\r\n', b'')
                    return data
                while needle not in observed():
                    assert dashboard.poll() is None, ('dashboard exited', dashboard.returncode)
                    assert time.monotonic() < deadline, (needle, bytes(captured[-3000:]))
                    if selector.select(.1):
                        captured.extend(os.read(master,65536))
        def press(data):
            global cursor
            cursor = len(captured)
            os.write(master,data)
        def checkpoint(name):
            # A visible marker can arrive before the rest of fux's synchronized frame.
            # Capture only after its actual closing bytes arrive; never synthesize them.
            deadline = time.monotonic()+12
            with selectors.DefaultSelector() as selector:
                selector.register(master, selectors.EVENT_READ)
                while captured.rfind(b'\x1b[?2026h') > captured.rfind(b'\x1b[?2026l'):
                    assert time.monotonic() < deadline, 'unfinished synchronized frame'
                    assert dashboard.poll() is None, 'dashboard exited inside a frame'
                    if selector.select(.1):
                        captured.extend(os.read(master,65536))
            (out/(name+'.ansi')).write_bytes(captured)
        try:
            if options.columns < 180:
                wait_output(b'2/3 machines live' if options.without_local_runtime else b'3/3 machines live')
            else:
                wait_output(b'first:live')
                wait_output(b'second:live')
            if options.without_local_runtime:
                wait_output(b'Local:unavailable')
            initial = {'first': b'first', 'local': b'Local', None: b'All machines'}[options.initial_machine]
            wait_output(b'zor dashboard | '+initial)
            checkpoint('01-machine-scope' if options.initial_machine else '01-aggregate')
            if options.restart_controller:
                catalog_before_restart = (root/'config/zor/machines.json').read_bytes()
                inputs_before_restart = [control(path, dict(command='capture',pane=pane['id'],max_bytes=65536))['input_sequence']
                                         for path,host_env,pane in hosts]
                press(b'q')
                deadline = time.monotonic()+10
                with selectors.DefaultSelector() as selector:
                    selector.register(master, selectors.EVENT_READ)
                    while dashboard.poll() is None:
                        assert time.monotonic() < deadline, 'controller shutdown timeout'
                        if selector.select(.1): captured.extend(os.read(master,65536))
                assert dashboard.returncode == 0
                restored = json.loads((out/'restored-termios.json').read_text())
                normalized_before = list(before)
                normalized_before[3] &= ~getattr(termios,'PENDIN',0)
                normalized_before[6] = [v[0] if isinstance(v,bytes) else v for v in before[6]]
                assert restored == normalized_before, 'first controller did not restore terminal'
                assert set(Path('/tmp').glob('zor-gw-*')) == helpers_before, 'first controller leaked helper'
                children.remove(dashboard)
                old_controller_pid = dashboard.pid
                (out/'controller-before-restart.ansi').write_bytes(captured)
                os.close(master)
                os.close(slave)
                master, slave = pty.openpty()
                before = termios.tcgetattr(slave)
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', options.rows, options.columns, 0, 0))
                dashboard = subprocess.Popen([sys.executable, '-c', launcher, str(out/'restored-termios.json'),
                                              bins['ZOR_BIN'], '--koh-binary', bins['KOH_BIN'], '--fux-binary', bins['FUX_BIN'],
                                              *dashboard_args], cwd=root, env=dashboard_env, stdin=slave, stdout=slave,
                                             stderr=log, preexec_fn=controlling_terminal)
                children.append(dashboard)
                captured = bytearray()
                cursor = 0
                wait_output(b'zor dashboard | '+initial)
                wait_output(b'first:live' if options.columns == 180 else
                            (b'2/3 machines live' if options.without_local_runtime else b'3/3 machines live'))
                if options.columns == 180: wait_output(b'second:live')
                assert dashboard.pid != old_controller_pid
                assert (root/'config/zor/machines.json').read_bytes() == catalog_before_restart
                refreshed = json.loads(run('ZOR_BIN', '--koh-binary', bins['KOH_BIN'], 'dashboard', '--all-machines', '--once'))
                for name in ('first','second'):
                    old_machine = next(m for m in aggregate['machines'] if m['name'] == name)
                    new_machine = next(m for m in refreshed['machines'] if m['name'] == name)
                    assert new_machine['id'] == old_machine['id'], 'saved machine identity changed'
                    assert new_machine['view']['service_instance'] == old_machine['view']['service_instance'], 'controller restarted remote service'
                inputs_after_restart = [control(path, dict(command='capture',pane=pane['id'],max_bytes=65536))['input_sequence']
                                        for path,host_env,pane in hosts]
                assert inputs_after_restart == inputs_before_restart, 'controller restart replayed input'
                assert all(child.poll() is None for child in children), 'controller restart killed remote owner'
                (out/'controller-restart.json').write_text(json.dumps(dict(old_pid=old_controller_pid,new_pid=dashboard.pid,
                    input_before=inputs_before_restart,input_after=inputs_after_restart,machines=refreshed)))
                checkpoint('19-controller-restarted')
            if options.initial_machine is None:
                press(b'\t')
                wait_output(b'zor dashboard | Local')
            if options.initial_machine != 'first':
                press(b'\t')
            wait_output(b'zor dashboard | first')
            if task_indexes['first']:
                press(b'j'*task_indexes['first'])
                wait_output(b'> first')
            if options.dashboard_resume:
                assert not options.observed_agent, 'dashboard resume fixture uses an adopted task'
                resume_journal = hosts[0][0]/'state/zor/journal.json'
                resume_retained = resume_journal.read_bytes()
                press(b'u')
                wait_output(b'Resume selected task')
                checkpoint('20-resume-form')
                press(b'\x1b')
                wait_output(b'zor dashboard | first')
                assert resume_journal.read_bytes() == resume_retained, 'cancelled resume mutated journal'
                assert json.loads(run('ZOR_BIN','machine','resume-intents')) == [], 'cancelled form recorded a dispatch intent'
                press(b'u')
                wait_output(b'Resume selected task')
                resume_view = next(m['view'] for m in aggregate['machines'] if m['name'] == 'first')
                resume_instance = resume_view['rows'][task_indexes['first']]['expected']['target']['instance']
                press(('ui-resume-refusal '+resume_instance).encode())
                wait_output(resume_instance.encode(), cell_output=True)
                checkpoint('22-resume-input')
                press(b'\r')
                if options.columns < 100:
                    wait_output(b'outcome unconfirmed', cell_output=True)
                    press(b'e')
                    wait_output(b'j/k scroll | Esc back')
                wait_output(b'resume requires a managed task', cell_output=True)
                assert resume_journal.read_bytes() == resume_retained, 'refused dashboard resume mutated journal'
                resume_intents = json.loads(run('ZOR_BIN','machine','resume-intents'))
                assert len(resume_intents) == 1, resume_intents
                assert resume_intents[0]['operation'] == 'ui-resume-refusal'
                assert resume_intents[0]['expected']['task'] == 'same'
                assert resume_intents[0]['fux_instance'] == resume_instance
                assert resume_intents[0]['machine'] == next(m['id'] for m in aggregate['machines'] if m['name'] == 'first')
                (out/'dashboard-resume-intents.json').write_text(json.dumps(resume_intents))
                checkpoint('21-resume-refused')
                if options.columns < 100:
                    press(b'\x1b')
                    wait_output(b'zor dashboard | first')
                press(b'U')
                wait_output(b'Inspect resume operation')
                press(b'ui-resume-refusal\r')
                wait_output(b'Read only; an absent record', cell_output=True)
                wait_output(b'"record": null', cell_output=True)
                assert resume_journal.read_bytes() == resume_retained, 'status UI mutated task journal'
                assert json.loads(run('ZOR_BIN','machine','resume-intents')) == resume_intents, 'status UI changed saved intent'
                checkpoint('23-resume-status')
                press(b'\x1b')
                wait_output(b'zor dashboard | first')
            if options.observed_agent:
                press(b'c')
                wait_output(b'task-only action unavailable for an observed agent')
            press(b'\r')
            wait_output(b'preparing action...')
            assert len(set(Path('/tmp').glob('zor-gw-*')) - helpers_before) == 2, 'inspection spawned a redundant control gateway'
            wait_output(b'Inspection/result' if options.columns >= 80 else b'j/k scroll | Esc back')
            wait_output(inspection_needle)
            checkpoint('02-inspection')
            press(b'\x1b')
            wait_output(b'zor dashboard | first')
            press(b'a')
            # Fux's pane output appears after its own handshake; terminal must stay responsive.
            wait_output(b'\x1b[?2026l')
            press(b'printf "HANDOFF_%s\\n" OK\r')
            wait_output(b'HANDOFF_OK', cell_output=True)
            checkpoint('03-viewer')
            target_capture = control(hosts[0][0], dict(command='capture', pane=hosts[0][2]['id'], max_bytes=65536))
            assert 'HANDOFF_OK' in target_capture['text'], target_capture
            untouched = control(hosts[1][0], dict(command='capture', pane=hosts[1][2]['id'], max_bytes=65536))
            assert untouched['input_sequence'] == 0, untouched
            if options.transport_faults:
                fault = root/'fault'
                def fault_command(action):
                    global fault_sequence
                    fault_sequence = globals().get('fault_sequence', 0)+1
                    (fault/'command.tmp').write_text(json.dumps(dict(id=fault_sequence,action=action)))
                    (fault/'command.tmp').replace(fault/'command.json')
                    until(lambda: (fault/'ack.json').exists() and json.loads((fault/'ack.json').read_text())['id'] == fault_sequence)
                    return json.loads((fault/'ack.json').read_text())
                fault_acks = []
                effects = root/'transport-effects'
                for index in range(3):
                    ack = fault_command('drop')
                    assert ack['accepted'] >= index+1, ack
                    fault_acks.append(ack)
                    press(('printf x >> '+str(effects)+'; printf "RESUMED_'+str(index)+'\\n"\n').encode())
                    wait_output(('RESUMED_'+str(index)).encode(), cell_output=True)
                    until(lambda: effects.exists() and effects.read_bytes() == b'x'*(index+1))
                before_expiry = control(hosts[0][0],dict(command='capture',pane=hosts[0][2]['id'],max_bytes=65536))
                fault_command('expire')
                wait_output(b'attachment session expired', timeout=45)
                wait_output(b'zor dashboard | first')
                checkpoint('14-expired')
                assert effects.read_bytes() == b'xxx', 'expiry replayed input'
                after_expiry = control(hosts[0][0],dict(command='capture',pane=hosts[0][2]['id'],max_bytes=65536))
                assert after_expiry['input_sequence'] == before_expiry['input_sequence']
                fault_command('resume')
                press(b'a')
                wait_output(b'\x1b[?2026l')
                press(b'printf "FRESH_ATTACHMENT_OK\\n"\n')
                wait_output(b'FRESH_ATTACHMENT_OK', cell_output=True)
                target_capture = control(hosts[0][0],dict(command='capture',pane=hosts[0][2]['id'],max_bytes=65536))
                assert effects.read_bytes() == b'xxx', 'fresh attach replayed old input'
                (out/'transport-evidence.json').write_text(json.dumps(dict(acks=fault_acks, effects=effects.read_text(), before_expiry=before_expiry['input_sequence'], after_expiry=after_expiry['input_sequence'], fresh_attachment=target_capture['input_sequence'], pane=hosts[0][2]), indent=2))
                checkpoint('15-fresh-attachment')
            press(b'\x01dSUFFIX_MUST_NOT_REACH_PANE')
            wait_output(b'Detached; refreshing the selection')
            wait_output(b'zor dashboard | first')
            checkpoint('04-return')
            after_detach = control(hosts[0][0], dict(command='capture', pane=hosts[0][2]['id'], max_bytes=65536))
            assert after_detach['input_sequence'] == target_capture['input_sequence'], (target_capture,after_detach)
            press(b'\r')
            wait_output(inspection_needle)
            press(b'\x1b')
            wait_output(b'zor dashboard | first')
            press(b'\t')
            wait_output(b'zor dashboard | second')
            if task_indexes['second']:
                press(b'j'*task_indexes['second'])
                wait_output(b'> second')
            press(b'a')
            wait_output(b'no attachment binding for workspace agent')
            checkpoint('05-missing-binding')
            for scope in (b'All machines', b'Local', b'first'):
                press(b'\t')
                wait_output(b'zor dashboard | '+scope)
            # The first machine's original task selection survives scope changes.
            press(b'a')
            wait_output(b'\x1b[?2026l')
            process_rows = [line.split(None, 3) for line in subprocess.check_output(
                ['ps','-axo','pid,ppid,pgid,command'], text=True).splitlines()[1:]]
            controllers = [int(row[0]) for row in process_rows if int(row[1]) == dashboard.pid and row[3].startswith(bins['ZOR_BIN'])]
            assert len(controllers) == 1, controllers
            viewers = [row for row in process_rows if int(row[1]) == controllers[0] and row[3].startswith(bins['FUX_BIN']+' attach')]
            assert len(viewers) == 1 and int(viewers[0][2]) == dashboard.pid, viewers
            cursor = len(captured)
            os.kill(int(viewers[0][0]),signal.SIGKILL)
            wait_output(b'Attachment ended: viewer ended')
            wait_output(b'zor dashboard | first')
            assert b'\x1b[?1003l' in captured[cursor:] and b'\x1b[?2004l' in captured[cursor:], 'viewer reporting modes not reset'
            checkpoint('06-viewer-killed')
            press(b'\r')
            wait_output(inspection_needle)
            press(b'\x1b')
            wait_output(b'zor dashboard | first')
            press(b'a')
            wait_output(b'preparing action...')
            press(b'\x1b')
            wait_output(b'Cancellation requested')
            wait_output(b'preparation cancelled')
            assert b'\x1b[?2026h' not in captured[cursor:], 'cancelled preparation opened a viewer'
            checkpoint('07-preparation-cancelled')
            unchanged = control(hosts[0][0], dict(command='capture', pane=hosts[0][2]['id'], max_bytes=65536))
            assert unchanged['input_sequence'] == target_capture['input_sequence'], 'recovery or cancellation replayed input'
            if options.resume_refusal:
                resume_before = json.loads(run('ZOR_BIN','task','inspect','same',target_env=hosts[0][1]))
                instance = resume_before['session']['target']['instance']
                refusal = run('ZOR_BIN','--machine','first','--koh-binary',bins['KOH_BIN'],
                    'task','resume','same','--operation','refused-resume','--instance',instance,expect_success=False)
                assert 'resume requires a managed task' in refusal, refusal
                resume_after = json.loads(run('ZOR_BIN','task','inspect','same',target_env=hosts[0][1]))
                assert resume_after == resume_before, 'refused remote resume changed retained task evidence'
                (out/'resume-refusal.txt').write_text(refusal)
            if options.restart_fux:
                first_fux = fux_servers['first']
                old_instance = next((row.get('expected') or {})['target']['instance']
                    for machine in aggregate['machines'] if machine['name'] == 'first'
                    for row in machine['view']['rows'] if (row.get('expected') or {}).get('task') == 'same')
                first_fux.terminate()
                assert first_fux.wait(timeout=5) == 0
                children.remove(first_fux)
                fux_servers['first'] = spawn('FUX_BIN', 'serve', target_env=hosts[0][1])
                until(lambda: (hosts[0][0]/'fux/default.sock').exists())
                replacement = json.loads(run('FUX_BIN', 'list', target_env=hosts[0][1]))['result']['value']
                if not any(workspace['name'] == 'agent' for workspace in replacement['workspaces']):
                    run('FUX_BIN', 'workspace', 'new', 'agent', target_env=hosts[0][1])
                replacement = control(hosts[0][0],dict(command='list'))
                assert replacement['instance'] != old_instance, 'fux restarted with old incarnation'
                replacement_pane = next(pane for workspace in replacement['workspaces'] if workspace['name'] == 'agent'
                    for tab in workspace['tabs'] for pane in tab['panes'] if pane['id'] == hosts[0][2]['id'])
                assert replacement_pane['pid'] != hosts[0][2]['pid'], 'process was not replaced'
                # The historical task may still be inspectable, but its exact process cannot attach.
                press(b'a')
                wait_output(b'action failed')
                assert b'\x1b[?2026h' not in captured[cursor:], 'old task opened a replacement viewer'
                checkpoint('18-fux-replaced')
                untouched = control(hosts[0][0],dict(command='capture',pane=replacement_pane['id'],max_bytes=65536))
                assert untouched['input_sequence'] == 0, 'old selection sent input to replacement'
                (out/'fux-replacement.json').write_text(json.dumps(dict(old_instance=old_instance,new_instance=replacement['instance'],old_pane=hosts[0][2],new_pane=replacement_pane,capture=untouched),indent=2))
            if options.restart_zor:
                first_server = zor_servers['first']
                original = next(machine['view']['service_instance'] for machine in aggregate['machines'] if machine['name'] == 'first')
                cursor = len(captured)
                first_server.terminate()
                assert first_server.wait(timeout=5) == 0
                children.remove(first_server)
                wait_output(b'first:stale')
                checkpoint('16-zor-offline')
                zor_servers['first'] = spawn('ZOR_BIN', *(['--agent', 'codex', 'serve'] if options.observed_agent else ['serve']), target_env=hosts[0][1])
                cursor = len(captured)
                wait_output(b'first:live')
                press(b'\r')
                wait_output(b'selection no longer exists')
                checkpoint('17-zor-restarted')
                current = json.loads(run('ZOR_BIN', '--machine', 'first', '--koh-binary', bins['KOH_BIN'], 'dashboard', '--once'))
                (out/'restarted-zor.json').write_text(json.dumps(current,indent=2))
                assert current['view']['service_instance'] != original, 'zor restart retained service incarnation'
                current_rows = current['view']['rows']
                index = next(i for i,row in enumerate(current_rows) if (row.get('expected') or {}).get('task') == 'same')
                assert index > 0, 'fixture needs an explicit navigation step from unavailable selection'
                press(b'j'*index)
                press(b'\r')
                wait_output(b'first / same / attempt')
                press(b'\x1b')
                wait_output(b'zor dashboard | first')
                current_pane = control(hosts[0][0],dict(command='capture',pane=hosts[0][2]['id'],max_bytes=65536))
                assert current_pane['input_sequence'] == target_capture['input_sequence'], 'zor restart replayed input'
            if options.columns < 180:
                press(b'?')
                wait_output(b'Dashboard controls')
                checkpoint('12-help')
                if options.rows <= 16:
                    # Help grows with controls; verify its tail through actual scrolling.
                    press(b'j'*32)
                    wait_output(b'Task-only actions refuse observed agents.')
                press(b'\x1b')
                wait_output(b'zor dashboard | first')
            if options.notifications:
                for path, host_env, pane in hosts:
                    listing = control(path, dict(command='list'))
                    run('ZOR_BIN', 'task', 'start', 'alert', '--title', 'PRIVATE_NOTIFICATION_TITLE',
                        '--instance', listing['instance'], '--workspace', 'agent', '--cwd', path, '--', '/bin/cat', target_env=host_env)
                    run('ZOR_BIN', 'task', 'require-check', 'alert', 'required', '--', '/usr/bin/false', target_env=host_env)
                    run('ZOR_BIN', 'task', 'check', 'alert', 'failed', '--requirement', 'required', '--', '/usr/bin/false', target_env=host_env)
                def notices():
                    path = root/'notices.jsonl'
                    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
                expected_view = json.loads(run('ZOR_BIN', '--koh-binary', bins['KOH_BIN'], 'dashboard', '--all-machines', '--once'))
                assert all(item['fresh'] for item in expected_view['machines']), expected_view
                expected_count = sum(row['attention'] for item in expected_view['machines'] for row in item['view']['rows'])
                assert expected_count >= 2
                (out/'notification-view.json').write_text(json.dumps(expected_view, indent=2))
                deadline = time.monotonic()+12
                with selectors.DefaultSelector() as selector:
                    selector.register(master, selectors.EVENT_READ)
                    while sum(int(args[1].split()[0]) for args in notices()) != expected_count:
                        assert time.monotonic() < deadline, ('notification timeout', notices(), bytes(captured[-3000:]))
                        if selector.select(.1): captured.extend(os.read(master,65536))
                first_notices = notices()
                assert all(args[0] == 'Zor needs attention' and 'PRIVATE' not in str(args) and 'first' not in str(args) and 'second' not in str(args) for args in first_notices), first_notices
                wait_output(b'checks-failed')
                checkpoint('11-notifications')
                # Keep draining the real PTY while unchanged fresh polls cross the cooldown.
                end = time.monotonic()+6
                with selectors.DefaultSelector() as selector:
                    selector.register(master, selectors.EVENT_READ)
                    while time.monotonic() < end:
                        if selector.select(.1): captured.extend(os.read(master,65536))
                assert notices() == first_notices, 'unchanged attention repeated notification'
                audible = re.sub(rb'\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)', b'', captured)
                assert audible.count(b'\x07') == len(first_notices), 'bell and desktop coalescing diverged'
                (out/'notifications.json').write_text(json.dumps(first_notices))
            if options.reload_catalog:
                existing_helpers = set(Path('/tmp').glob('zor-gw-*'))
                run('ZOR_BIN', 'machine', 'rename', 'first', 'renamed')
                press(b'R')
                wait_output(b'Machine catalog reloaded')
                wait_output(b'zor dashboard | renamed')
                assert set(Path('/tmp').glob('zor-gw-*')) == existing_helpers, 'rename restarted a control connection'
                press(b'\r')
                wait_output(inspection_needle.replace(b'first', b'renamed', 1))
                press(b'\x1b')
                wait_output(b'zor dashboard | renamed')
                checkpoint('08-renamed')
                catalog_path = root/'config/zor/machines.json'
                valid_catalog = catalog_path.read_bytes()
                catalog_path.write_bytes(b'{invalid')
                press(b'R')
                wait_output(b'Reload failed; existing profiles retained')
                assert set(Path('/tmp').glob('zor-gw-*')) == existing_helpers
                checkpoint('09-invalid-catalog')
                catalog_path.write_bytes(valid_catalog)
                run('ZOR_BIN', 'machine', 'control', 'second', '--clear')
                press(b'R')
                wait_output(b'Machine catalog reloaded')
                wait_output(b'second:unavailable')
                until(lambda: len(set(Path('/tmp').glob('zor-gw-*')) - helpers_before) == 1)
                press(b'\r')
                wait_output(inspection_needle.replace(b'first', b'renamed', 1))
                press(b'\x1b')
                wait_output(b'zor dashboard | renamed')
                run('ZOR_BIN', 'machine', 'remove', 'renamed')
                press(b'R')
                wait_output(b'Machine catalog reloaded')
                wait_output(b'zor dashboard | All machines')
                press(b'\r')
                wait_output(b'selection no longer exists')
                checkpoint('10-removed-selection')
                until(lambda: set(Path('/tmp').glob('zor-gw-*')) == helpers_before)
            press(b'q')
            deadline = time.monotonic()+10
            with selectors.DefaultSelector() as selector:
                selector.register(master, selectors.EVENT_READ)
                while dashboard.poll() is None:
                    if time.monotonic() >= deadline:
                        if Path('/usr/bin/sample').exists():
                            subprocess.run(['/usr/bin/sample', str(dashboard.pid), '1', '1', '-file', str(out/'shutdown-timeout.sample')],
                                           capture_output=True, timeout=5)
                        raise AssertionError('dashboard shutdown exceeded ten seconds')
                    if selector.select(.1):
                        captured.extend(os.read(master,65536))
            assert dashboard.returncode == 0
            after = json.loads((out/'restored-termios.json').read_text())
            before[3] &= ~getattr(termios,'PENDIN',0)
            before[6] = [value[0] if isinstance(value,bytes) else value for value in before[6]]
            assert before == after, ('terminal not restored',before,after)
            assert all(child.poll() is None for child in children if child != dashboard), 'remote owner exited'
            assert set(Path('/tmp').glob('zor-gw-*')) == helpers_before, 'owned helper leaked'
            if options.dashboard_resume:
                assert json.loads(run('ZOR_BIN','machine','resume-intents')) == resume_intents, 'dashboard exit lost original resume intent'
            if options.observed_agent:
                for index,name in enumerate(('first','second')):
                    untouched_task_pane = control(hosts[index][0], dict(command='capture',pane=adopted[name],max_bytes=65536))
                    assert untouched_task_pane['input_sequence'] == 0, 'observed attachment targeted an adopted task instead'
                    tasks = json.loads(run('ZOR_BIN','task','list',target_env=hosts[index][1]))
                    assert len(tasks['tasks']) == 1 and tasks['tasks'][0]['id'] == 'same', 'observed attachment created or removed a task'
                control(hosts[0][0],dict(command='kill',pane=observed_handle['pane']))
                replacement = control(hosts[0][0],dict(command='split',axis='horizontal',argv=['/bin/sh'],final_retain_ms=60000))['pane']
                assert replacement != observed_handle['pane']
                assert observed_request(observed_handle)['status'] == 'failed', 'exited observed handle accepted a replacement'
                replacement_capture = control(hosts[0][0],dict(command='capture',pane=replacement,max_bytes=65536))
                assert replacement_capture['input_sequence'] == 0, 'old observed handle delivered input to replacement'
            print(json.dumps(dict(result='passed', dashboard_resume=options.dashboard_resume, restart_controller=options.restart_controller, resume_refusal=options.resume_refusal, restart_fux=options.restart_fux, restart_zor=options.restart_zor, transport_faults=options.transport_faults, columns=options.columns, rows=options.rows, notifications=options.notifications, reload_catalog=options.reload_catalog, observed_agent=options.observed_agent, initial_machine=options.initial_machine, without_local_runtime=options.without_local_runtime, binaries={key:dict(path=value,sha256=hashlib.sha256(Path(value).read_bytes()).hexdigest()) for key,value in bins.items()},
                 checks=['Local + two remotes','same-name task identities','controlling PTY foreground group','task inspection','non-default exact viewer',
                         'target input only','detach suffix discarded','same selection return','missing binding refused','killed viewer returns without losing task',
                         'viewer reporting modes reset','cancelled preparation never attaches','terminal restored','helpers cleaned','remote owners preserved'])))
        finally:
            if (root/'notices.jsonl').exists():
                (out/'notifications-raw.jsonl').write_bytes((root/'notices.jsonl').read_bytes())
            (out/'dashboard-all.ansi').write_bytes(captured)
            os.close(master)
            os.close(slave)
    finally:
        for child in reversed(children):
            if child.poll() is None:
                try:
                    os.killpg(child.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                except PermissionError:
                    # Closing a controlling PTY can exit the session leader between
                    # poll and killpg; the owned Child remains safe to wait/terminate.
                    child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=5)
        for log in logs:
            log.close()
