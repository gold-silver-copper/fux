"""Real CLI/TTY regression checks. Uses only disposable identities and local networking."""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

BINARY = str(Path(sys.argv[1]).resolve())
CLIENT_PASS = b"test client passphrase"
SERVER_PASS = b"different workspace passphrase"


class Terminal:
    def __init__(self, env, *args):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(BINARY, [BINARY, *args], env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        self.output = b""
        self.cursor = 0
        self.status = None

    def pump(self):
        if select.select([self.fd], [], [], 0.05)[0]:
            try:
                self.output += os.read(self.fd, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise

    def until(self, predicate, description, timeout=40):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.pump()
            if predicate():
                return
            if self.status is None:
                pid, status = os.waitpid(self.pid, os.WNOHANG)
                if pid:
                    self.status = status
            if self.status is not None:
                if predicate():
                    return
                break
        raise AssertionError(f"waiting for {description}: {self.output[:1500]!r} ... {self.output[-500:]!r}")

    def expect(self, text):
        self.until(lambda: text in self.output[self.cursor:], repr(text))
        self.cursor = self.output.index(text, self.cursor) + len(text)

    def send(self, text):
        os.write(self.fd, text)

    def password(self, password):
        # rpassword prints its prompt before disabling echo. Wait for the input mode transition.
        self.until(lambda: not termios.tcgetattr(self.fd)[3] & termios.ECHO, "password input mode")
        self.send(password + b"\n")

    def raw(self):
        self.expect(b"\x1b[?1049h")
        assert not termios.tcgetattr(self.fd)[3] & termios.ICANON
        self.expect(b"\x1b[2;")  # Wait for a pane, not just the initial empty viewport.

    def finish(self, success=True):
        def exited():
            if self.status is None:
                pid, status = os.waitpid(self.pid, os.WNOHANG)
                if pid:
                    self.status = status
            return self.status is not None
        self.until(exited, "process exit")
        assert (os.waitstatus_to_exitcode(self.status) == 0) == success, self.output
        assert termios.tcgetattr(self.fd)[3] & termios.ICANON, "terminal left raw"
        assert termios.tcgetattr(self.fd)[3] & termios.ECHO, "terminal echo not restored"

    def close(self):
        if self.status is None:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        os.close(self.fd)


def run(env, *args, success=True, extra=None):
    result = subprocess.run([BINARY, *args], env=env | (extra or {}),
                            stdin=subprocess.DEVNULL, capture_output=True, timeout=40)
    assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
    return result.stdout + result.stderr


def exercise(root):
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("KOH_", "FUX_", "XDG_"))}
    env.update(HOME=str(root), XDG_CONFIG_HOME=str(root / "config"),
               XDG_STATE_HOME=str(root / "state"), XDG_RUNTIME_DIR=str(root / "run"),
               TERM="xterm-256color", SHELL="/bin/sh")
    for directory in (root / "config/fux", root / "run"):
        directory.mkdir(parents=True, mode=0o700)
    (root / "config/fux/config.toml").write_text(
        'local-network = true\ndefault-command = { argv = ["/bin/sh"] }\n'
        'zor-path = "/nonexistent/fux-test-zor"\n')
    client = root / "config/koh/client.key"
    server = root / "state/fux/keys/default.key"
    socket = root / "run/fux/manager.sock"
    terminals = []

    def terminal(*args):
        value = Terminal(env, *args)
        terminals.append(value)
        return value

    def unlock(tty, path, password):
        tty.expect(f"Passphrase for {path}: ".encode())
        tty.password(password)

    def no_manager():
        assert not socket.exists(), "credential failure left a daemon socket"
        assert not list((root / "run/fux/workspaces").glob("*.json"))
        assert not list((root / "run/fux").glob("startup-*.sock"))

    try:
        assert run(env, "key", "path").strip() == str(client).encode()
        assert run(env, "key", "path", "--workspace", "default").strip() == str(server).encode()
        assert not client.exists() and not server.exists()
        run(env, "key", "reset", "--yes", success=False)
        run(env, "key", "reset", "--workspace", "../bad", "--yes", success=False)

        # Cancellation during both fresh key setup and existing-key loading starts no child.
        cancelled = terminal()
        cancelled.expect(b"Set a passphrase")
        cancelled.until(lambda: not termios.tcgetattr(cancelled.fd)[3] & termios.ECHO,
                        "password input mode")
        os.kill(cancelled.pid, signal.SIGINT)
        cancelled.finish(success=False)
        no_manager()
        assert not client.exists()

        fresh = terminal()
        for path, password in ((client, CLIENT_PASS), (server, SERVER_PASS)):
            fresh.expect(f"new identity key {path}: ".encode())
            fresh.password(password)
            fresh.expect(b"Confirm passphrase: ")
            fresh.password(password)
        fresh.raw()
        fresh.send(b"\x01d")
        fresh.finish()
        assert b"Passphrase for " not in fresh.output, fresh.output
        assert fresh.output.count(b"Confirm passphrase:") == 2
        assert client.read_bytes().startswith(b"koh-key-v1")
        assert server.read_bytes().startswith(b"koh-key-v1")
        assert client.stat().st_mode & 0o777 == 0o600
        assert server.stat().st_mode & 0o777 == 0o600
        for password in (CLIENT_PASS, SERVER_PASS):
            assert password not in fresh.output, "passphrase echoed"

        # Reset refuses a live manager without changing either identity.
        before = client.read_bytes(), server.read_bytes()
        run(env, "key", "reset", "--client", success=False)
        blocked = run(env, "key", "reset", "--client", "--yes", success=False)
        assert b"manager is running" in blocked
        run(env, "key", "reset", "--workspace", "default", "--yes", success=False)
        assert before == (client.read_bytes(), server.read_bytes())

        run(env, "workspace", "new", "second",
            extra={"KOH_KEY_NEW_PASSPHRASE": SERVER_PASS.decode()})
        attached = terminal("default")
        unlock(attached, client, CLIENT_PASS)
        attached.raw()
        attached.send(b"\x01s")
        attached.expect(b"select workspace")
        assert termios.tcgetattr(attached.fd)[3] & termios.ICANON
        attached.send(b"2\n")
        attached.raw()
        attached.send(b"\x01d")
        attached.finish()
        assert attached.output.count(b"Passphrase for ") == 1, attached.output
        assert CLIENT_PASS not in attached.output

        picker_cancel = terminal("default")
        unlock(picker_cancel, client, CLIENT_PASS)
        picker_cancel.raw()
        picker_cancel.send(b"\x01s")
        picker_cancel.expect(b"select workspace")
        os.kill(picker_cancel.pid, signal.SIGINT)
        picker_cancel.finish(success=False)

        run(env, "workspace", "kill", "second")
        run(env, "workspace", "kill", "default")
        deadline = time.monotonic() + 10
        while socket.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        no_manager()

        wrong = terminal()
        unlock(wrong, client, b"wrong passphrase")
        wrong.finish(success=False)
        assert b"wrong passphrase" in wrong.output and b"--key-file" not in wrong.output
        no_manager()

        for cancel in (True, False):
            failed = terminal()
            unlock(failed, client, CLIENT_PASS)
            failed.expect(f"Passphrase for {server}: ".encode())
            if cancel:
                failed.until(lambda: not termios.tcgetattr(failed.fd)[3] & termios.ECHO,
                             "password input mode")
                os.kill(failed.pid, signal.SIGINT)
            else:
                failed.password(b"wrong workspace passphrase")
            failed.finish(success=False)
            no_manager()

        cold = terminal()
        unlock(cold, client, CLIENT_PASS)
        unlock(cold, server, SERVER_PASS)
        cold.raw()
        os.kill(cold.pid, signal.SIGTERM)
        cold.finish()
        assert cold.output.count(b"Passphrase for ") == 2
        run(env, "workspace", "kill", "default")
        deadline = time.monotonic() + 10
        while socket.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        no_manager()

        # A custom remote key is loaded before starting terminal input or parsing/dialing the peer.
        remote = terminal("connect", "invalid-endpoint", "--key-file", str(client))
        unlock(remote, client, CLIENT_PASS)
        remote.finish(success=False)
        assert b"parsing server endpoint id" in remote.output
        assert remote.output.count(b"Passphrase for ") == 1

        # Changing the passphrase preserves identity; reset changes it and needs no old password.
        extra = {"KOH_KEY_PASSPHRASE": CLIENT_PASS.decode()}
        old_id = run(env, "id", extra=extra).strip()
        new_pass = "replacement test passphrase"
        run(env, "key", "passwd", "--client",
            extra=extra | {"KOH_KEY_NEW_PASSPHRASE": new_pass})
        assert run(env, "id", extra={"KOH_KEY_PASSPHRASE": new_pass}).strip() == old_id
        run(env, "key", "info", "--client", extra=extra, success=False)
        run(env, "key", "reset", "--workspace", "default", "--yes")
        assert not server.exists() and client.exists()
        run(env, "key", "reset", "--client", "--yes")
        assert not client.exists()
        new_id = run(env, "id", extra={"KOH_KEY_NEW_PASSPHRASE": new_pass}).strip()
        assert old_id != new_id

        client.parent.chmod(0o777)
        output = run(env, "default", extra=extra, success=False)
        assert b"wrong passphrase" not in output
        client.parent.chmod(0o700)

        # Corruption and unsafe paths remain distinct from authentication failures.
        client.write_text("malformed identity")
        output = run(env, "default", extra=extra, success=False)
        assert b"wrong passphrase" not in output and b"koh-key-v1" in output
        client.unlink()
        client.symlink_to(root / "outside")
        run(env, "key", "reset", "--client", "--yes", success=False)
        output = run(env, "default", extra=extra, success=False)
        assert b"symlink" in output
        no_manager()
        print("Identity CLI and PTY regressions passed")
    finally:
        for tty in terminals:
            tty.close()
        # Only this fixture's manager is addressed; never signal processes discovered outside it.
        for name in ("second", "default"):
            subprocess.run([BINARY, "workspace", "kill", name], env=env,
                           stdin=subprocess.DEVNULL, capture_output=True, timeout=20)


with tempfile.TemporaryDirectory(prefix="fux-keys-", dir="/tmp") as directory:
    exercise(Path(directory))
