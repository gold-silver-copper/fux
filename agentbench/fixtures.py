#!/usr/bin/env python3
"""Small deterministic terminal programs the benchmark's tasks drive.

Each is a subcommand: `fixtures.py NAME [ARGS...]`. Most write what happened to
a file in the current directory (the task's work directory), which the task's
checker reads; none uses the network. They draw with plain ANSI, as a TUI does:
the alternate screen, reverse video for a selection, colours that mean
something, the bell, mouse reports, bracketed paste.
"""
import os
import select
import signal
import sys
import termios
import time
import tty

ESC = "\x1b"
OUT = sys.stdout


def w(text):
    OUT.write(text)
    OUT.flush()


def record(name, text):
    with open(name, "a", encoding="utf-8") as f:
        f.write(text)


def raw():
    """Raw mode, put back at exit."""
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    tty.setraw(fd)
    return lambda: termios.tcsetattr(fd, termios.TCSADRAIN, old)


def read(timeout=None):
    """Input, or b"" when none came in time. When the terminal is gone (end of
    input, or an error reading it) the program ends, rather than spin on a
    descriptor that is always ready."""
    ready, _, _ = select.select([0], [], [], timeout)
    if not ready:
        return b""
    try:
        data = os.read(0, 4096)
    except OSError:
        data = b""
    if not data:
        os._exit(0)
    return data


def hold(seconds=3600.0):
    """Stays for SECONDS, ending with the terminal: a multiplexer that
    closes a pane without a hangup leaves no program behind."""
    end = time.time() + float(seconds)
    while (left := end - time.time()) > 0:
        read(min(left, 1.0))


def size():
    s = os.get_terminal_size()
    return s.lines, s.columns


# ---------------------------------------------------------------- programs


def menu(items="alpha,beta,gamma,delta,epsilon,zeta", start="0"):
    """A list on the alternate screen; the selection in reverse video. j/k or
    arrows move, Enter writes the chosen item to chosen.txt and exits."""
    items = items.split(",")
    sel = int(start)
    put = raw()
    w(f"{ESC}[?1049h{ESC}[?25l")
    try:
        while True:
            w(f"{ESC}[H{ESC}[2JChoose one (j/k, Enter):\r\n\r\n")
            for i, item in enumerate(items):
                line = f"  {item:<20}"
                w((f"{ESC}[7m{line}{ESC}[0m" if i == sel else line) + "\r\n")
            data = read()
            for key in data.replace(b"\x1b[B", b"j").replace(b"\x1b[A", b"k"):
                if key == ord("j"):
                    sel = min(sel + 1, len(items) - 1)
                elif key == ord("k"):
                    sel = max(sel - 1, 0)
                elif key in (13, 10):
                    record("chosen.txt", items[sel])
                    return 0
                elif key == ord("q"):
                    return 1
    finally:
        w(f"{ESC}[?25h{ESC}[?1049l")
        put()


def loader(seconds="2", then="READY", crash="", redraw=""):
    """A spinner for SECONDS, then THEN and a prompt that echoes lines to
    lines.txt until `quit`. With crash=N it exits N instead of being ready;
    with redraw, it draws the same screen again every 100 ms while it waits."""
    record("pids.txt", f"{os.getpid()}\n")
    put = raw()
    try:
        end = time.time() + float(seconds)
        frames = "|/-\\"
        i = 0
        while time.time() < end:
            w(f"\r{frames[i % 4]} loading...")
            i += 1
            time.sleep(0.05)
        if crash:
            w("\r\nfatal: could not load\r\n")
            return int(crash)
        w(f"\r{then}          \r\n> ")
        line = b""
        while True:
            data = read(0.1 if redraw else None)
            if not data:
                if redraw:
                    w(f"\r> {line.decode(errors='replace')}")
                continue
            for b in data:
                if b in (13, 10):
                    text = line.decode(errors="replace")
                    record("lines.txt", text + "\n")
                    if text == "quit":
                        return 0
                    w(f"\r\nok: {text}\r\n> ")
                    line = b""
                elif b == 127:
                    line = line[:-1]
                    w("\b \b")
                else:
                    line += bytes([b])
                    w(chr(b))
    finally:
        put()


def scroll(lines="300", marker="MARKER-42", at="150", hours="1"):
    """Prints LINES numbered lines at once, MARKER on line AT, then holds."""
    out = []
    for n in range(1, int(lines) + 1):
        out.append(marker if n == int(at) else f"line {n}")
    w("\n".join(out) + "\n")
    hold(float(hours) * 3600)


def form():
    """Two fields, name and age: Tab moves, Enter submits to form.txt."""
    put = raw()
    fields = ["", ""]
    cur = 0
    w(f"{ESC}[?1049h")
    try:
        while True:
            w(f"{ESC}[H{ESC}[2JSign up\r\n\r\n")
            for i, label in enumerate(["Name", "Age"]):
                mark = ">" if i == cur else " "
                w(f"{mark} {label}: [{fields[i]:<20}]\r\n")
            w(f"\r\n  [ Submit: Enter ]")
            data = read()
            for b in data:
                if b == 9:
                    cur = 1 - cur
                elif b in (13, 10):
                    record("form.txt", f"{fields[0]},{fields[1]}")
                    return 0
                elif b == 127:
                    fields[cur] = fields[cur][:-1]
                elif 32 <= b < 127:
                    fields[cur] += chr(b)
    finally:
        w(f"{ESC}[?1049l")
        put()


def modal():
    """A dialog that swallows keys until Escape; then a prompt whose line
    goes to modal.txt."""
    put = raw()
    w(f"{ESC}[?1049h{ESC}[H{ESC}[2J+--------------------------+\r\n| Update available! (Esc)  |\r\n+--------------------------+\r\n")
    try:
        while True:
            data = read()
            if data == b"\x1b":
                break
        w(f"{ESC}[H{ESC}[2Jcommand: ")
        line = b""
        while True:
            for b in read():
                if b in (13, 10):
                    record("modal.txt", line.decode(errors="replace"))
                    return 0
                line += bytes([b])
                w(chr(b))
    finally:
        w(f"{ESC}[?1049l")
        put()


def keys(seconds="30"):
    """Writes each read it gets, in hex, a line each, to keys.txt; with
    bracketed paste on. A lone q quits."""
    put = raw()
    w(f"{ESC}[?2004hready (q quits)\r\n")
    try:
        end = time.time() + float(seconds)
        while time.time() < end:
            data = read(0.2)
            if data == b"q":
                return 0  # quits, and is no key the task asked for
            if data:
                record("keys.txt", data.hex() + "\n")
                w(f"got {data.hex()}\r\n")
    finally:
        put()


def chat():
    """An input box as agent CLIs have: a read of more than 16 bytes is a
    paste, its Enter a newline; Enter read alone submits the message to
    sent.txt. Bracketed pastes are pastes."""
    put = raw()
    w(f"{ESC}[?2004h> ")
    msg = ""
    try:
        while True:
            data = read()
            if data.startswith(b"\x1b[200~"):
                msg += data[6:].replace(b"\x1b[201~", b"").decode(errors="replace")
                w("[pasted]")
                continue
            paste = len(data) > 16
            for b in data:
                if b in (13, 10):
                    if paste:
                        msg += "\n"
                        continue
                    record("sent.txt", msg + "\n")
                    w(f"\r\nsent: {msg!r}\r\n> ")
                    if msg == "bye":
                        return 0
                    msg = ""
                elif b == 127:
                    msg = msg[:-1]
                else:
                    msg += chr(b)
                    w(chr(b))
    finally:
        put()


def table():
    """A status table: one row's status is red (FAIL), the rest green."""
    rows = [("api", "OK"), ("db", "OK"), ("cache", "FAIL"), ("queue", "OK"), ("auth", "OK")]
    w(f"{ESC}[1mSERVICE   STATUS{ESC}[0m\n")
    for name, status in rows:
        colour = 31 if status == "FAIL" else 32
        w(f"{name:<10}{ESC}[{colour}m{'down' if status == 'FAIL' else 'up'}{ESC}[0m\n")
    hold()


def agent(seconds="2", ask=""):
    """A stand-in agent CLI: a spinner and `esc to interrupt` while it
    works, then its answer, a bell and an OSC 9 notification; with ask, a
    permission question first (y answers it), with a bell. Prompts go to
    prompts.txt."""
    put = raw()
    frames = "⠋⠙⠹⠸⠼⠴⠦⠧"
    try:
        w("agent ready\r\n> ")
        msg = ""
        while True:
            data = read()
            for b in data:
                if b == 3:
                    w("\r\n^C interrupted\r\n> ")
                    msg = ""
                elif b in (13, 10):
                    record("prompts.txt", msg + "\n")
                    if ask:
                        w(f"\r\nAllow tool use? [y/n]\a{ESC}]9;agent needs permission\a")
                        while read() not in (b"y", b"n"):
                            pass
                    end = time.time() + float(seconds)
                    i = 0
                    interrupted = False
                    while time.time() < end:
                        w(f"\r{frames[i % 8]} working (esc to interrupt)")
                        i += 1
                        if read(0.08) in (b"\x1b", b"\x03"):
                            interrupted = True
                            break
                    if interrupted:
                        w("\r\ninterrupted\r\n> ")
                    else:
                        w(f"\r\nanswer: {msg.upper()}\r\n\a{ESC}]9;agent done\a> ")
                    msg = ""
                elif b == 127:
                    msg = msg[:-1]
                elif b >= 32:
                    msg += chr(b)
                    w(chr(b))
    finally:
        put()


def confirm():
    """Asks to continue; y writes yes to confirm.txt, anything else no."""
    put = raw()
    try:
        w("Delete 3 files? [y/N] ")
        data = read()
        record("confirm.txt", "yes" if data.lower() == b"y" else "no")
        w("\r\ndone\r\n")
    finally:
        put()
    hold()


def stubborn():
    """Ignores SIGTERM and SIGHUP; writes its pid to pid.txt."""
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    record("pid.txt", str(os.getpid()))
    w("stubborn and running\n")
    while True:
        time.sleep(1)


def sizer():
    """Writes its size at start and at each SIGWINCH to sizes.txt, and
    draws it."""
    def note(*_):
        rows, cols = size()
        record("sizes.txt", f"{cols}x{rows}\n")
        w(f"{ESC}[H{ESC}[2Jsize {cols}x{rows}\n")
    signal.signal(signal.SIGWINCH, note)
    note()
    hold()


def mainalt():
    """Prints a log on the main screen, with an error line, then takes the
    alternate screen."""
    for n in range(1, 21):
        w(("error: disk full at step 13" if n == 13 else f"step {n} ok") + "\n")
    w(f"{ESC}[?1049h{ESC}[H{ESC}[2JFULL SCREEN APP (q quits)")
    put = raw()
    try:
        while read() != b"q":
            pass
    finally:
        w(f"{ESC}[?1049l")
        put()


def wide():
    """Wide characters and emoji, then echo of what is typed to wide.txt."""
    put = raw()
    try:
        w("漢字テスト 🙂 ok\r\n> ")
        line = b""
        while True:
            for b in read():
                if b in (13, 10):
                    record("wide.txt", line.decode(errors="replace"))
                    w("\r\nsaved\r\n")
                    return 0
                line += bytes([b])
    finally:
        put()


def mouse():
    """Buttons drawn at known cells; a left click on one writes its name to
    clicked.txt; the wheel moves a counter, written to wheel.txt."""
    put = raw()
    w(f"{ESC}[?1049h{ESC}[?1000h{ESC}[?1006h{ESC}[H{ESC}[2J")
    w(f"{ESC}[3;5H[ OK ]{ESC}[3;20H[ Cancel ]{ESC}[6;1Hwheel: 0")
    count = 0
    try:
        while True:
            data = read().decode(errors="replace")
            for event in data.split(f"{ESC}[<")[1:]:
                body, final = event[:-1], event[-1:]
                button, col, row = (int(x) for x in body.split(";"))
                if button == 0 and final == "M" and row == 3:
                    name = "OK" if 5 <= col <= 10 else "Cancel" if 20 <= col <= 29 else ""
                    if name:
                        record("clicked.txt", name)
                elif button in (64, 65):
                    count += 1 if button == 65 else -1
                    record("wheel.txt", f"{count}\n")
                    w(f"{ESC}[6;1Hwheel: {count}  ")
    finally:
        w(f"{ESC}[?1000l{ESC}[?1006l{ESC}[?1049l")
        put()


def quiet(seconds="3", status="0"):
    """Writes nothing for SECONDS, then `finished` and exits STATUS; each run
    is a line in runs.txt."""
    record("runs.txt", "ran\n")
    time.sleep(float(seconds))
    w("finished\n")
    return int(status)


def bell(seconds="1"):
    """Works for SECONDS without a visible change at its end but the bell."""
    w("background job running\n")
    time.sleep(float(seconds))
    w("\a")
    hold()


def main():
    name, args = sys.argv[1], sys.argv[2:]
    kwargs = dict(a.split("=", 1) for a in args if "=" in a)
    positional = [a for a in args if "=" not in a]
    status = globals()[name](*positional, **kwargs)
    sys.exit(status or 0)


if __name__ == "__main__":
    main()
