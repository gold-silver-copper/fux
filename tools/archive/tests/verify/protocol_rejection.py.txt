"""A rejected local handshake (a server that answers the hello with something other than a hello)
must leave terminal settings and screen mode untouched."""
import errno
import json
import os
from pathlib import Path
import pty
import socket
import struct
import subprocess
import sys
import tempfile
import termios

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="fpr-", dir="/tmp") as directory:
    root = Path(directory)
    path = root / "service.sock"
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(path))
    path.chmod(0o600)
    listener.listen(1)
    listener.settimeout(5)
    env = os.environ.copy()
    env.update(HOME=directory, XDG_CONFIG_HOME=directory + "/config",
               XDG_RUNTIME_DIR=directory, XDG_STATE_HOME=directory + "/state",
               TERM="xterm-256color", SHELL="/bin/sh")
    for response in ({"hello": {"version": 1}}, {"exited": {"code": None}},
                     {"error": {"message": "the first attachment frame must be a hello"}}):
        master, slave = pty.openpty()
        before = termios.tcgetattr(slave)
        child = subprocess.Popen([binary, "attach", "--socket", str(path)], env=env,
                                 stdin=slave, stdout=slave, stderr=slave)
        peer = None
        try:
            peer, _ = listener.accept()
            peer.settimeout(5)
            def exact(size):
                result = b""
                while len(result) < size:
                    part = peer.recv(size - len(result))
                    assert part, "client closed before handshake"
                    result += part
                return result
            size = struct.unpack(">I", exact(4))[0]
            assert size <= 65536
            hello = json.loads(exact(size))
            assert hello["type"] == "hello"
            encoded = json.dumps(response).encode()
            peer.sendall(struct.pack(">I", len(encoded)) + encoded)
            assert child.wait(timeout=5) != 0
            assert termios.tcgetattr(slave) == before, "terminal attributes changed on rejection"
            os.set_blocking(master, False)
            output = b""
            while True:
                try:
                    part = os.read(master, 16384)
                    if not part:
                        break
                    output += part
                    assert len(output) <= 65536, "unbounded rejection output"
                except OSError as error:
                    if error.errno in (errno.EIO, errno.EAGAIN):
                        break
                    raise
            assert b"session server" in output, output
            assert b"\x1b[?1049h" not in output, "alternate screen entered before negotiation"
            assert b"Passphrase" not in output, output
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            if peer is not None:
                peer.close()
            os.close(master)
            os.close(slave)
    listener.close()
    assert not list(root.rglob("*.key"))
print("PASS: handshake rejection preserves terminal state and creates no keys")
