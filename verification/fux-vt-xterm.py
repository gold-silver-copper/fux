#!/usr/bin/env python3
"""Independent macOS XTerm checks; owns every child and display it creates.
Requires Homebrew xterm and xorg-server. No interactive shell is driven.
Run: python3 verification/fux-vt-xterm.py /tmp/fux-vt-evidence/xterm
"""
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import time


def child():
    import fcntl
    import struct
    import termios
    import tty
    tty.setraw(0)
    root = Path(sys.argv[2])
    sequence = bytes.fromhex(sys.argv[3])
    dimensions = struct.unpack("HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, b"\0" * 8))[:2]
    os.write(1, b"\x1b[2J\x1b[H" + sequence + b"\x1b[6n")
    reply = bytearray()
    deadline = time.monotonic() + 5
    with selectors.DefaultSelector() as events:
        events.register(0, selectors.EVENT_READ)
        while not reply.endswith(b"R"):
            if not events.select(max(0, deadline - time.monotonic())):
                raise TimeoutError("xterm cursor report")
            reply.extend(os.read(0, 128))
    # Media Copy captures the independent emulator's complete screen. A
    # subsequent reply establishes that xterm consumed the print command.
    # Printing can be restricted to the scrolling region by xterm's resources;
    # restore full margins only after the cursor report, without changing cells.
    os.write(1, b"\x1b[r\x1b[0i\x1b[5n")
    ack = bytearray()
    with selectors.DefaultSelector() as events:
        events.register(0, selectors.EVENT_READ)
        while not ack.endswith(b"n"):
            if not events.select(max(0, deadline - time.monotonic())):
                raise TimeoutError("xterm print acknowledgement")
            ack.extend(os.read(0, 128))
    (root / "reply.json").write_text(json.dumps({"size": dimensions, "cursor": reply.hex(), "ack": ack.hex()}))


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def main():
    root = Path(sys.argv[1]).resolve()
    root.mkdir(parents=True, exist_ok=True)
    read_fd, write_fd = os.pipe()
    with (root / "xvfb.log").open("wb") as log:
        display = subprocess.Popen(["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "800x600x24", "-nolisten", "tcp"],
                                   pass_fds=(write_fd,), stdout=log, stderr=log)
        os.close(write_fd)
        try:
            with selectors.DefaultSelector() as events:
                events.register(read_fd, selectors.EVENT_READ)
                if not events.select(10):
                    raise TimeoutError("Xvfb display readiness")
            display_number = os.read(read_fd, 128).strip().decode()
            env = dict(os.environ, DISPLAY=":" + display_number)
            records = []
            for name, geometry, sequence, expected in [
                ("decawm-off", "3x2", b"\x1b[?7labcdef", b"abf"),
                ("decawm-on", "3x2", b"abcdef", b"abc\ndef"),
                ("below-region-control", "4x4", b"\x1b[2;3r\x1b[4;4HZ", b"\n\n\n   Z"),
                ("insert-below-region", "4x4", b"\x1b[2;3r\x1b[4;4HZ\x1b[L", b"\n\n\n   Z"),
                ("delete-below-region", "4x4", b"\x1b[2;3r\x1b[4;4HZ\x1b[M", b"\n\n\n   Z"),
                ("insert-above-region", "4x4", b"\x1b[2;3r\x1b[1;1HA\x1b[L", b"A"),
                ("delete-above-region", "4x4", b"\x1b[2;3r\x1b[1;1HA\x1b[M", b"A"),
            ]:
                case = root / name
                case.mkdir(exist_ok=True)
                with (case / "xterm.log").open("wb") as log:
                    # The printer is a harness-written command in a controlled
                    # path, not an interactive shell or another user's terminal.
                    capture = case / 'screen.txt'
                    capture.unlink(missing_ok=True)
                    printer = f"/bin/cat > {str(case / 'screen.part')!r}; /bin/mv {str(case / 'screen.part')!r} {str(capture)!r}"
                    terminal = subprocess.Popen(["xterm", "-fa", "monospace", "-fs", "10", "-geometry", geometry,
                        "-xrm", "*printerCommand: " + printer, "-xrm", "*printAttributes: 0",
                        "-e", sys.executable, str(Path(__file__).resolve()), "--child", str(case), sequence.hex()],
                        env=env, stdout=log, stderr=log)
                    try:
                        status = terminal.wait(timeout=15)
                    finally:
                        stop(terminal)
                    if status:
                        raise RuntimeError(f"xterm {name}: exit {status}; see {case}")
                report = json.loads((case / "reply.json").read_text())
                deadline = time.monotonic() + 5
                while not capture.exists():
                    if time.monotonic() >= deadline:
                        raise TimeoutError("printer completion")
                    time.sleep(0.01)  # Bounded observation of the printer's atomic rename.
                screen = capture.read_bytes().rstrip(b"\n\r\x0c ")
                if screen != expected:
                    raise AssertionError((name, screen, expected, report))
                records.append({"case": name, "input_hex": sequence.hex(), "screen_hex": screen.hex(), **report})
            (root / "results.json").write_text(json.dumps(records, indent=2) + "\n")
            print(json.dumps(records, indent=2))
        finally:
            os.close(read_fd)
            stop(display)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--child":
        child()
    else:
        main()
