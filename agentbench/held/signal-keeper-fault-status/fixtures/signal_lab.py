
import os, sys, json, time, select, signal, hashlib, termios
from pathlib import Path

work = Path(sys.argv[1]).resolve()
fx = work / ".fx"
fx.mkdir(parents=True, exist_ok=True)
NAME = "signal-" + sys.argv[2]
if len(sys.argv) > 3 and sys.argv[2] == "report":
    NAME += "-" + sys.argv[3]
if not (sys.stdin.isatty() and sys.stdout.isatty()):
    (fx / (NAME + ".failure.json")).write_text(json.dumps({"reason": "stdin and stdout must be terminals"}) + "\n")
    raise SystemExit("stdin and stdout must be terminals")
launch_file = fx / (NAME + ".launches.json")
import fcntl
with (fx / (NAME + ".launch.lock")).open("w") as launch_lock:
    fcntl.flock(launch_lock, fcntl.LOCK_EX)
    launches = json.loads(launch_file.read_text()) + 1 if launch_file.exists() else 1
    launch_file.write_text(json.dumps(launches) + "\n")
if launches != 1:
    (fx / (NAME + ".failure.json")).write_text(json.dumps({"reason": "run each program exactly once"}) + "\n")
    raise SystemExit("run each program exactly once")
(fx / (NAME + ".pid")).write_text(str(os.getpid()) + "\n")
start_ticks = int(Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19])
(fx / (NAME + ".process.json")).write_text(json.dumps({"pid": os.getpid(), "start_ticks": start_ticks, "stdin_tty": sys.stdin.isatty(), "stdout_tty": sys.stdout.isatty()}) + "\n")

def save(name, value):
    target = fx / (name + ".json")
    tmp = fx / (name + ".tmp")
    tmp.write_text(json.dumps(value, sort_keys=True) + "\n")
    tmp.replace(target)

def load(name):
    try:
        return json.loads((fx / (name + ".json")).read_text())
    except (OSError, ValueError):
        fail("required fixture record is missing or invalid: " + name)

def running(name):
    identity = load(name + ".process")
    try:
        fields = Path(f"/proc/{identity['pid']}/stat").read_text().rsplit(")", 1)[1].split()
    except FileNotFoundError:
        return False
    return int(fields[19]) == identity["start_ticks"] and fields[0] not in ("Z", "X")

def out(value="", end="\n"):
    print(value, end=end, flush=True)

def fail(reason):
    save(NAME + ".failure", {"reason": reason})
    out("\033[31mPROTOCOL FAILED: " + reason + "\033[0m")
    raise SystemExit(90)

def pause(seconds):
    """Readiness barriers reject even a partial premature line or key."""
    if _pending:
        fail("input arrived before the next readiness prompt")
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    pending = termios.tcgetattr(fd)
    pending[3] &= ~termios.ICANON
    pending[6][termios.VMIN] = 1
    pending[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, pending)
    deadline = time.monotonic() + seconds
    try:
        while time.monotonic() < deadline:
            remaining = deadline - time.monotonic()
            ready, _, _ = select.select([sys.stdin], [], [], min(0.10, max(0, remaining)))
            if ready:
                data = os.read(fd, 4096)
                global _skip_lf
                if _skip_lf and data.startswith(b"\n"):
                    _skip_lf = False
                    data = data[1:]
                    if not data:
                        continue
                if data:
                    fail("input arrived before the next readiness prompt")
                fail("terminal input closed during startup")
    finally:
        termios.tcsetattr(fd, termios.TCSANOW, old)


from collections import deque
from contextlib import contextmanager
import tty

_pending = deque()
_skip_lf = False

@contextmanager
def raw_terminal():
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    # Keep signals active, but handle line editing and all newline forms ourselves.
    tty.setraw(fd)
    attrs = termios.tcgetattr(fd)
    attrs[3] |= termios.ISIG
    termios.tcsetattr(fd, termios.TCSANOW, attrs)
    try:
        yield
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)

def byte():
    global _skip_lf
    while True:
        b = _pending.popleft() if _pending else os.read(sys.stdin.fileno(), 1)
        if not b:
            fail("terminal closed")
        if _skip_lf:
            _skip_lf = False
            if b == b"\n":
                continue
        return b

def key():
    first = byte()
    if first != b"\x1b":
        return first
    second = byte()
    if second not in (b"[", b"O"):
        return first + second
    third = byte()
    result = first + second + third
    if second == b"[" and third == b"M":
        return result + b"".join(byte() for _ in range(3))
    if third == b"<":
        while result[-1:] not in (b"M", b"m"):
            result += byte()
            if len(result) > 64:
                fail("invalid mouse event")
    elif third.isdigit():
        while not (0x40 <= result[-1] <= 0x7e):
            result += byte()
            if len(result) > 64:
                fail("invalid key sequence")
    # Normalize terminal-mode variants into one semantic representation.
    aliases = {b"\x1bO" + c: b"\x1b[" + c for c in (b"A", b"B", b"C", b"D")}
    aliases.update({b"\x1b[Q": b"\x1bOQ", b"\x1b[12~": b"\x1bOQ"})
    return aliases.get(result, result)

def normalize_newlines(data):
    return data.replace(b"\r\n", b"\n").replace(b"\r", b"\n")

def edit(data, event):
    if event in (b"\x08", b"\x7f"):
        if data:
            cut = len(data) - 1
            while cut > 0 and data[cut] & 0xc0 == 0x80:
                cut -= 1
            del data[cut:]
    elif event == b"\x15":
        data.clear()
    else:
        data.extend(event)

def raw_line():
    global _skip_lf
    data = bytearray()
    while True:
        event = key()
        if event == b"\x1b[200~":
            pasted = bytearray()
            while not pasted.endswith(b"\x1b[201~"):
                pasted.extend(byte())
                if len(pasted) > 32768:
                    fail("input too long")
            # Queue the paste contents without recording its transport markers.
            _pending.extendleft(reversed([bytes([b]) for b in normalize_newlines(bytes(pasted[:-6]))]))
            continue
        if event in (b"\r", b"\n"):
            _skip_lf = event == b"\r"
            out("\r")
            return data.decode("utf-8")
        edit(data, event)
        if len(data) > 32768:
            fail("input too long")

def ask(prompt):
    # Set input mode before advertising readiness.
    with raw_terminal():
        out(prompt, end="")
        return raw_line()

def mouse(event):
    if event.startswith(b"\x1b[<") and event[-1:] in (b"M", b"m"):
        try:
            button, x, y = map(int, event[3:-1].split(b";"))
        except ValueError:
            fail("malformed mouse event")
        return button, x, y, event.endswith(b"M") and (button & 3) != 3
    if event.startswith(b"\x1b[M") and len(event) == 6:
        button, x, y = (v - 32 for v in event[3:])
        return button, x, y, (button & 3) != 3
    fail("Apply requires a primary mouse press")

def expect(prompt, wanted):
    value = ask(prompt)
    if value != wanted:
        fail("expected " + repr(wanted) + ", received " + repr(value))
    return value

mode = sys.argv[2]
if mode == "report":
    role, status = sys.argv[3], int(sys.argv[4])
    expected = {"keeper": 0, "fault": 143}[role]
    previous = load(role)
    if status != expected or (role == "keeper" and previous["exit"] != 0) or (role == "fault" and previous["trip"] is not True):
        fail("wrong actual shell status")
    save("signal-report-" + role, {"status": status})
    out("EXIT " + role + "=" + str(status))
    raise SystemExit(0)
if mode == "keeper":
    count, phase = 0, "boot"
    def interrupt(signum, frame):
        global count
        count += 1
        if count == 1 and phase == "ready":
            out("\nSIGINT ignored #1; type drain to prepare shutdown")
        elif count == 2 and phase == "drained":
            save("keeper", {"interrupts": 2, "drained": True, "exit": 0})
            out(f"\nKEEPER CLOSED code=0 interrupts={count}")
            raise SystemExit(0)
        else:
            fail("interrupt arrived in the wrong shutdown phase")
    signal.signal(signal.SIGINT, interrupt)
    out("Keeper booting ...")
    pause(4)
    phase = "ready"
    out("KEEPER READY")
    expect("Keeper> ", "drain")
    if count != 1:
        fail("drain must follow one ignored Ctrl-C")
    phase = "draining"
    out("Draining journal; interrupts are not safe yet.")
    pause(6)
    phase = "drained"
    out("SHUTDOWN READY")
    while True:
        signal.pause()
elif mode == "fault":
    out("Fault probe initializing ...")
    pause(3)
    out("FAULT ARMED type trip")
    expect("Fault> ", "trip")
    save("fault", {"trip": True, "signal": "SIGTERM"})
    out("FAULT injecting SIGTERM now")
    os.kill(os.getpid(), signal.SIGTERM)
else:
    fail("unknown signal role")
