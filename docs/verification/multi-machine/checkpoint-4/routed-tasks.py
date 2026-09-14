"""Real saved-machine routing smoke test; requires explicit FUX_BIN, ZOR_BIN, KOH_BIN."""
import json
import socket
import os
from pathlib import Path
import selectors
import pty
import termios
import fcntl
import struct
import signal
import subprocess
import tempfile
import time

binaries = {name: str(Path(os.environ[name]).resolve(strict=True)) for name in ('FUX_BIN', 'ZOR_BIN', 'KOH_BIN')}
children = []
logs = []
with tempfile.TemporaryDirectory(prefix='zmr-', dir='/tmp') as directory:
    root = Path(directory)
    env = dict(PATH=os.environ.get("PATH", "/usr/bin:/bin"), HOME=str(root), XDG_RUNTIME_DIR=str(root), XDG_STATE_HOME=str(root/'state'),
               XDG_CONFIG_HOME=str(root/'config'), SHELL='/bin/sh', TERM='xterm-256color',
               KOH_KEY_PASSPHRASE='fixture-only-multi-machine-credentials',
               KOH_KEY_NEW_PASSPHRASE='fixture-only-multi-machine-credentials')
    def run(binary, *args, expected=0, service_env=None):
        result = subprocess.run([binaries[binary], *map(str,args)], env=service_env or env, cwd=root,
                                stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15)
        assert result.returncode in ((0, 1) if expected is None else (expected,)), (args, result.returncode, result.stderr)
        return result
    def spawn(binary, *args, output=False, service_env=None):
        log = tempfile.TemporaryFile()
        logs.append(log)
        child = subprocess.Popen([binaries[binary], *map(str,args)], env=service_env or env, cwd=root,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE if output else log,
                                 stderr=log, start_new_session=True)
        children.append(child)
        return child
    try:
        fux = spawn('FUX_BIN', 'serve')
        zor = spawn('ZOR_BIN', 'serve')
        deadline = time.monotonic() + 8
        while not (root/'zor/control.sock').exists():
            assert fux.poll() is None and zor.poll() is None
            assert time.monotonic() < deadline, 'service startup timed out'
            time.sleep(.02)
        remote_roots = []
        for name in ('first', 'second'):
            remote_root = root/name
            remote_root.mkdir(mode=0o700)
            remote_env = dict(env, HOME=str(remote_root), XDG_RUNTIME_DIR=str(remote_root),
                              XDG_CONFIG_HOME=str(remote_root/'config'), XDG_STATE_HOME=str(remote_root/'state'))
            spawn('FUX_BIN', 'serve', service_env=remote_env)
            spawn('ZOR_BIN', 'serve', service_env=remote_env)
            deadline = time.monotonic() + 8
            while not (remote_root/'zor/control.sock').exists():
                assert all(child.poll() is None for child in children)
                assert time.monotonic() < deadline
                time.sleep(.02)
            listing = json.loads(run('FUX_BIN', 'list', service_env=remote_env).stdout)['result']['value']
            pane = listing['workspaces'][0]['tabs'][0]['panes'][0]
            run('ZOR_BIN', 'task', 'adopt', 'same', '--title', name+' task', '--instance', listing['instance'],
                '--workspace', 'default', '--pane', pane['id'], service_env=remote_env)
            run('ZOR_BIN', 'task', 'start', 'managed', '--title', name+' managed', '--instance', listing['instance'],
                '--workspace', 'default', '--cwd', remote_root, '--', '/bin/sh', '-c',
                'while :; do sleep 1; done', service_env=remote_env)
            remote_roots.append(remote_root)
        client_key = root/'client.key'
        identity = run('KOH_BIN', 'id', '--key-file', client_key).stdout.strip()
        server = spawn('KOH_BIN', 'gateway', 'serve', '--socket', remote_roots[0]/'zor/control.sock',
                       '--key-file', root/'server.key', '--allow', identity, '--local', output=True)
        with selectors.DefaultSelector() as selector:
            selector.register(server.stdout, selectors.EVENT_READ)
            assert selector.select(10), 'gateway advertisement timeout'
            advertisement = json.loads(server.stdout.readline())
        profile = json.loads(run('ZOR_BIN', 'machine', 'add', 'remote', '--endpoint', advertisement['endpoint_id'],
                                 '--key-file', client_key, '--direct', advertisement['direct_addr']).stdout)
        second = spawn('KOH_BIN', 'gateway', 'serve', '--socket', remote_roots[1]/'zor/control.sock',
                       '--key-file', root/'second-server.key', '--allow', identity, '--local', output=True)
        with selectors.DefaultSelector() as selector:
            selector.register(second.stdout, selectors.EVENT_READ)
            assert selector.select(10)
            second_advertisement = json.loads(second.stdout.readline())
        run('ZOR_BIN', 'machine', 'add', 'second', '--endpoint', second_advertisement['endpoint_id'],
            '--key-file', client_key, '--direct', second_advertisement['direct_addr'])
        retained = []
        inspections = []
        for machine, title in [('remote','first task'),('second','second task')]:
            routed = ('--machine',machine,'--koh-binary',binaries['KOH_BIN'])
            listed = json.loads(run('ZOR_BIN', *routed, 'task', 'list').stdout)
            inspected = json.loads(run('ZOR_BIN', *routed, 'task', 'inspect', 'same').stdout)
            result = json.loads(run('ZOR_BIN', *routed, 'task', 'result', 'same').stdout)
            assert next(task for task in listed['value']['tasks'] if task['id'] == 'same')['title'] == title
            assert inspected['value']['task']['title'] == title
            assert inspected['service_instance'] == result['service_instance']
            assert result['value']['task_id'] == 'same'
            assert result['value']['attempt']['id'] == inspected['value']['attempt']['id']
            retained.append(inspected['value']['session']['target']['instance'])
            inspections.append(inspected)
        assert len(set(retained)) == 2, 'same-named tasks crossed machines'
        wrong = inspections[0]['value']
        expected = dict(task='same',attempt=wrong['task']['attempt'],session=wrong['attempt']['session'],target=wrong['session']['target'])
        request = dict(v=1,id=71,op='task',service_instance=inspections[1]['service_instance'],
                       task=dict(action='supervise',expected=expected,action_kind='cancel'))
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
            stream.settimeout(3)
            stream.connect(str(remote_roots[1]/'zor/control.sock'))
            stream.sendall(json.dumps(request).encode()+b'\n')
            reply = json.loads(stream.makefile('rb').readline(524289))
            assert reply['status'] == 'failed', 'cross-machine attempt authorized cancellation'
        routed_first = ('--machine','remote','--koh-binary',binaries['KOH_BIN'])
        routed_second = ('--machine','second','--koh-binary',binaries['KOH_BIN'])
        cancelled = json.loads(run('ZOR_BIN', *routed_first, 'task', 'cancel', 'same').stdout)
        untouched = json.loads(run('ZOR_BIN', *routed_second, 'task', 'inspect', 'same').stdout)
        assert cancelled['value']['task']['outcome'] == 'cancelled'
        assert untouched['value']['task']['outcome'] == 'open'
        assert cancelled['value']['session']['target'] == inspections[0]['value']['session']['target']
        run('ZOR_BIN', *routed_second, 'task', 'stop', 'same', expected=1)
        stop_reply = run('ZOR_BIN', *routed_first, 'task', 'stop', 'managed', expected=None)
        if stop_reply.returncode:
            assert 'result unconfirmed' in stop_reply.stderr, stop_reply.stderr
        # Reconcile retained final evidence without sending another kill or restarting a pane.
        stopped = json.loads(run('ZOR_BIN', *routed_first, 'task', 'launch-reconcile', 'managed').stdout)
        other_managed = json.loads(run('ZOR_BIN', *routed_second, 'task', 'inspect', 'managed').stdout)
        assert stopped['value']['launch']['phase'] == 'closed'
        assert other_managed['value']['launch']['phase'] == 'attached'

        before = set(Path('/tmp').glob('zor-gw-*'))
        args = ('--machine', profile['id'], '--koh-binary', binaries['KOH_BIN'])
        status = json.loads(run('ZOR_BIN', *args, 'status').stdout)
        view = json.loads(run('ZOR_BIN', *args, 'dashboard', '--once').stdout)
        assert status['machine']['id'] == profile['id'] == view['machine']['id']
        assert status['snapshot']['service_instance'] == view['view']['service_instance']
        assert set(Path('/tmp').glob('zor-gw-*')) == before, 'owned helper directory leaked'
        assert all(child.poll() is None for child in children), 'remote service exited'
        run('ZOR_BIN', 'machine', 'rename', 'remote', 'renamed')
        assert json.loads(run('ZOR_BIN', *args, 'status').stdout)['machine']['name'] == 'renamed'
        run('ZOR_BIN', 'machine', 'add', 'unconfigured')
        aggregate = json.loads(run('ZOR_BIN', '--koh-binary', binaries['KOH_BIN'],
                                  'dashboard', '--all-machines', '--once').stdout)
        machines = {item['name']: item for item in aggregate['machines']}
        assert machines['renamed']['view']['service_instance'] == view['view']['service_instance']
        instances = {machines[name]['view']['service_instance'] for name in ('Local','renamed','second')}
        assert len(instances) == 3, 'machine routing aliased isolated services'
        assert all(machines[name]['problem'] is None for name in ('Local','renamed','second'))
        assert machines['unconfigured']['view'] is None and not machines['unconfigured']['fresh']
        assert 'No control binding' in machines['unconfigured']['problem']
        assert set(Path('/tmp').glob('zor-gw-*')) == before
        master, slave = pty.openpty()
        terminal_before = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 160, 0, 0))
        dashboard_log = tempfile.TemporaryFile()
        logs.append(dashboard_log)
        dashboard = subprocess.Popen([binaries['ZOR_BIN'], '--koh-binary', binaries['KOH_BIN'],
                                      'dashboard', '--all-machines'], env=env, cwd=root,
                                     stdin=slave, stdout=slave, stderr=dashboard_log, start_new_session=True)
        children.append(dashboard)
        captured = bytearray()
        def wait_output(needle):
            deadline = time.monotonic()+10
            with selectors.DefaultSelector() as selector:
                selector.register(master, selectors.EVENT_READ)
                while needle not in captured:
                    assert dashboard.poll() is None, 'dashboard exited before expected frame'
                    assert time.monotonic() < deadline, ('dashboard frame timeout', needle, bytes(captured[-2000:]))
                    if selector.select(.1):
                        captured.extend(os.read(master, 65536))
        try:
            wait_output(b'zor dashboard | All machines')
            wait_output(b'renamed:live')
            wait_output(b'second:live')
            os.write(master, b'\t')
            wait_output(b'zor dashboard | Local')
            os.write(master, b'\t')
            wait_output(b'zor dashboard | renamed')
            os.write(master, b'q')
            assert dashboard.wait(timeout=10) == 0
            terminal_after = termios.tcgetattr(slave)
            # macOS sets driver-managed PENDIN on a raw -> canonical transition, even
            # when tcsetattr restores the exact saved struct. Compare every other bit.
            terminal_after[3] &= ~getattr(termios, 'PENDIN', 0)
            terminal_before[3] &= ~getattr(termios, 'PENDIN', 0)
            assert terminal_after == terminal_before, ('dashboard failed terminal restoration', terminal_before, terminal_after)
            Path(__file__).with_name('dashboard-navigation.ansi').write_bytes(captured)
        finally:
            os.close(master)
            os.close(slave)
        children.remove(dashboard)
        assert set(Path('/tmp').glob('zor-gw-*')) == before, 'dashboard helper leaked'
        outsider = root/'outsider.key'
        run('KOH_BIN', 'id', '--key-file', outsider)
        run('ZOR_BIN', 'machine', 'control', 'renamed', '--endpoint', advertisement['endpoint_id'],
            '--key-file', outsider, '--direct', advertisement['direct_addr'])
        denied = run('ZOR_BIN', *args, 'status', expected=1)
        assert 'Unauthorized' in denied.stderr, denied.stderr
        assert set(Path('/tmp').glob('zor-gw-*')) == before, 'failed helper leaked'
        assert all(child.poll() is None for child in children)
        print(json.dumps({'result':'passed', 'checks':['same-named task list/inspect/results','guarded cancel/managed stop isolation, explicit reconciliation and adopted stop refusal','saved ID routing','snapshot/view incarnation',
              'rename identity','aggregate Local/two isolated remotes/unconfigured isolation','interactive machine navigation and terminal restoration','unauthorized status','helper cleanup','remote owners preserved'], 'binaries':binaries}))
    finally:
        for child in reversed(children):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=5)
        for log in logs:
            log.close()
