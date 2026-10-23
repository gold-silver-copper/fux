"""One driver per multiplexer, each the best way its documented CLI offers to
do what the tasks need: start a program, send it keys, read its screen, wait
for it, learn how it ended. A driver counts what it costs: every command it
runs (`calls`), every sleep (`sleep_s`), every poll of a screen (`polls`). A
tool with no way to do something raises `Unsupported`, which the task scores
as a failure, and the report as a finding.

The drivers never reach the user's own sessions: every command runs in the
benchmark's environment (`env.sh`: no inherited socket, a runtime directory and
sockets of its own), which `run.py` checks before anything starts.
"""
import fcntl
import json
import os
import pty
import re
import select
import shlex
import struct
import subprocess
import termios
import time

POLL = 0.1


class Unsupported(Exception):
    """The tool has no way to do this."""


class Timeout(Exception):
    """What was waited for did not come in time."""


class Exited(Exception):
    """The program exited before what was waited for came."""


ANSI = re.compile(r"\x1b\[([0-9;:?]*)([A-Za-z])")


def styled_rows(ansi_text):
    """Rows of text with ANSI SGR, each as (text, [(col, len, attrs)]), attrs
    a frozenset like {'reverse', 'fg31'}: enough to find a selection or a
    colour that means something."""
    rows = []
    for line in ansi_text.split("\n"):
        text, runs, attrs, col = "", [], frozenset(), 0
        pos = 0
        for m in ANSI.finditer(line):
            chunk = line[pos:m.start()]
            if chunk:
                if attrs:
                    runs.append((col, len(chunk), attrs))
                text += chunk
                col += len(chunk)
            pos = m.end()
            if m.group(2) != "m":
                continue
            params = [p for p in re.split("[;:]", m.group(1)) if p] or ["0"]
            new = set(attrs)
            i = 0
            while i < len(params):
                p = params[i]
                if p == "0":
                    new = set()
                elif p == "7":
                    new.add("reverse")
                elif p == "27":
                    new.discard("reverse")
                elif p == "1":
                    new.add("bold")
                elif p in ("38", "48") and i + 1 < len(params):
                    kind = params[i + 1]
                    n = 3 if kind == "5" else 5
                    new = {a for a in new if not a.startswith("fg" if p == "38" else "bg")}
                    new.add(("fg" if p == "38" else "bg") + ":".join(params[i + 1 : i + n]))
                    i += n - 1
                elif p.isdigit() and (30 <= int(p) <= 37 or 90 <= int(p) <= 97):
                    new = {a for a in new if not a.startswith("fg")}
                    new.add("fg" + p)
                elif p == "39":
                    new = {a for a in new if not a.startswith("fg")}
                i += 1
            attrs = frozenset(new)
        chunk = line[pos:]
        if chunk:
            if attrs:
                runs.append((col, len(chunk), attrs))
            text += chunk
        rows.append((text, runs))
    return rows


def is_red(attrs):
    return any(a in ("fg31", "fg91", "fg5:1", "fg5:9") or (a.startswith("fg2:") and _rgb_red(a))
               for a in attrs)


def is_green(attrs):
    return any(a in ("fg32", "fg92", "fg5:2", "fg5:10") or (a.startswith("fg2:") and _rgb_green(a))
               for a in attrs)


def _rgb(a):
    nums = [int(x) for x in a[3:].split(":") if x.isdigit()][-3:]
    return nums if len(nums) == 3 else [0, 0, 0]


def _rgb_red(a):
    r, g, b = _rgb(a)
    return r > 150 and g < 120 and b < 120


def _rgb_green(a):
    r, g, b = _rgb(a)
    return g > 150 and r < 120 and b < 150


class Driver:
    name = "?"

    def __init__(self, work, env):
        self.work, self.env = work, env
        self.calls = 0
        self.sleep_s = 0.0
        self.polls = 0
        self.log = []
        self.commands = 0

    # ------------------------------------------------------------ plumbing

    def run(self, argv, check=True, timeout=120, input=None):
        self.calls += 1
        self.log.append(" ".join(shlex.quote(a) for a in argv))
        out = subprocess.run(argv, env=self.env, cwd=self.work, capture_output=True,
                             text=True, timeout=timeout, input=input)
        if check and out.returncode != 0:
            raise RuntimeError(f"{argv[0]} {argv[1:4]}: {out.returncode}: {out.stderr.strip()[:300]}")
        return out

    def nap(self, seconds=POLL):
        self.sleep_s += seconds
        time.sleep(seconds)

    def poll(self, test, timeout, exited=None):
        """Polls `test` until it gives a value, as a tool without a wait
        must; `exited` ends it early, as the program's end."""
        end = time.time() + timeout
        while True:
            self.polls += 1
            value = test()
            if value:
                return value
            if exited and exited():
                # What came just before the end counts.
                value = test()
                if value:
                    return value
                raise Exited()
            if time.time() > end:
                raise Timeout()
            self.nap()

    # ------------------------------------------------------------ the API

    def start(self):
        """Starts a server of its own."""

    def stop(self):
        """Stops it, and whatever runs in it."""

    def spawn(self, argv, size=None, name=None):
        raise Unsupported("spawn")

    def shell(self, size=None):
        """An interactive shell, for tasks that type commands."""
        return self.spawn(["sh"], size)

    def keys(self, pane, *keys):
        raise Unsupported("keys")

    def text(self, pane, text):
        raise Unsupported("text")

    def paste(self, pane, text):
        raise Unsupported("paste")

    def raw(self, pane, data):
        """Bytes as they are, for what no key name says (mouse reports)."""
        raise Unsupported("raw bytes")

    def screen(self, pane, history=False):
        raise Unsupported("screen")

    def ansi(self, pane):
        raise Unsupported("styled screen")

    def exit_status(self, pane):
        """The status the pane's program exited with, or None while it runs."""
        raise Unsupported("exit status")

    # Waits: polling, unless a driver has better.

    def wait_text(self, pane, text, timeout=10):
        return self.poll(lambda: text in self.screen(pane), timeout,
                         lambda: self.exit_status(pane) is not None)

    def wait_gone(self, pane, text, timeout=10):
        return self.poll(lambda: text not in self.screen(pane), timeout)

    def wait_idle(self, pane, ms=300, timeout=10):
        last = [None, time.time()]

        def settled():
            now = self.screen(pane)
            if now != last[0]:
                last[0], last[1] = now, time.time()
                return False
            return time.time() - last[1] >= ms / 1000

        return self.poll(settled, timeout)

    def wait_exit(self, pane, timeout=10):
        return self.poll(lambda: self.exit_status(pane) is not None and str(self.exit_status(pane)),
                         timeout) and self.exit_status(pane)

    def wait_history(self, pane, text, timeout=10):
        """Waits for text that may scroll off the screen at once: the
        screen and its history are searched."""
        return self.poll(lambda: text in self.screen(pane, history=True), timeout,
                         lambda: self.exit_status(pane) is not None)

    def wait_bell(self, pane, timeout=10):
        raise Unsupported("bell")

    def send_and_wait_text(self, pane, keys, text, timeout=10):
        """Sends keys, then waits for text that appears after them."""
        before = self.screen(pane).count(text)
        for k in keys:
            self.keys(pane, k) if not k.startswith("text:") else self.text(pane, k[5:])
        return self.poll(lambda: self.screen(pane).count(text) > before, timeout)

    def mouse(self, pane, button, row, col):
        """A click (button 0), or the wheel (64 up, 65 down), at a cell from 0;
        sent as an SGR report, as the program asked for."""
        press = f"\x1b[<{button};{col + 1};{row + 1}M"
        release = f"\x1b[<{button};{col + 1};{row + 1}m" if button < 64 else ""
        self.raw(pane, press + release)

    def resize(self, pane, cols, rows):
        raise Unsupported("set a size")

    def resize_pct(self, pane, pct):
        raise Unsupported("resize to a percentage")

    def split(self, pane, argv):
        raise Unsupported("split")

    def find(self, name):
        raise Unsupported("find a pane by name")

    def respawn(self, pane):
        raise Unsupported("respawn")

    def kill(self, pane):
        raise Unsupported("kill")

    def cursor_hidden(self, pane):
        raise Unsupported("cursor visibility")

    def size_of(self, pane):
        raise Unsupported("pane size")

    def done(self, pane, timeout=10):
        """The next shell command's end and status (OSC 133)."""
        raise Unsupported("command completion")

    def run_command(self, pane, cmd, timeout=15):
        """Runs a command in a shell; its output and status. With no way to
        know a command's end, markers around it, numbered so that an earlier
        command's do not count, and split by quotes so that the command's
        echo does not read as them."""
        self.commands += 1
        n = self.commands
        self.text(pane, f"echo __BE''GIN{n}; {cmd}; echo __DO''NE{n} $?")
        self.keys(pane, "Enter")
        done = re.compile(rf"^__DONE{n} (\d+)$", re.M)
        self.poll(lambda: done.search(self.screen(pane, history=True)), timeout)
        rows = [l.rstrip() for l in self.screen(pane, history=True).split("\n")]
        end = max(i for i, l in enumerate(rows) if done.match(l))
        begin = max(i for i, l in enumerate(rows[:end]) if l == f"__BEGIN{n}")
        return "\n".join(rows[begin + 1:end]), rows[end].split()[1]

    def find_title(self, title):
        """The pane whose program set this title."""
        raise Unsupported("find a pane by its title")

    def attach_argv(self):
        """How a person attaches a terminal client to the server."""
        raise Unsupported("attach a client")

    def reattach(self, timeout=10):
        """A terminal client attaches, as a person's would, then detaches
        with the prefix then d; the programs run on without it."""
        argv = self.attach_argv()
        self.calls += 1
        self.log.append(" ".join(shlex.quote(a) for a in argv) + "  # attached on a pty, then C-b d")
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        client = subprocess.Popen(argv, env={**self.env, "TERM": "xterm-256color"}, cwd=self.work,
                                  stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        end = time.time() + timeout
        try:
            # Attached once it has drawn with its terminal raw: the prefix
            # then reaches it, not the terminal's line editing.
            drawn = False
            while not (drawn and not termios.tcgetattr(master)[3] & termios.ICANON):
                if client.poll() is not None or time.time() > end:
                    raise Timeout("the client did not attach")
                if select.select([master], [], [], POLL)[0]:
                    drawn = bool(os.read(master, 65536)) or drawn
            os.write(master, b"\x02d")
            while client.poll() is None and time.time() < end:
                if select.select([master], [], [], POLL)[0]:
                    try:
                        os.read(master, 65536)
                    except OSError:
                        break
            if client.wait(max(0.1, end - time.time())) != 0:
                raise RuntimeError(f"the client exited {client.returncode}")
        finally:
            if client.poll() is None:
                client.kill()
                client.wait()
            os.close(master)


# ---------------------------------------------------------------------- fux


class _Later:
    """A pane fux starts when the driver knows what to wait for after: the
    action that makes it waits, so its id comes from that answer."""

    def __init__(self, driver):
        self.driver, self.id = driver, None

    def __str__(self):
        if self.id is None:
            self.driver.flush()
        return self.id

    __repr__ = __str__

    def __eq__(self, other):
        return str(self) == str(other)

    def __hash__(self):
        return hash(str(self))


class Fux(Driver):
    """fux, driven with fuxth: each call one program, `str` before each
    text. An action answers at once and a wait is a word you say, armed on
    the action it follows: so the driver holds an action (start, keys, a
    click, a split, a respawn) until it knows whether a wait comes next, and
    sends both in one call (`%2 str ok: wait-text str quit text str Enter key
    send-keys`). Anything else sends a held action first, alone."""
    name = "fux"

    def __init__(self, work, env, binary):
        super().__init__(work, env)
        self.bin = binary
        self.server = None
        # The held action: (pane it acts on, or the _Later it makes; words
        # before its wait; the word that acts).
        self.pending = None

    def fux(self, *words, check=True, timeout=120):
        return self.run([self.bin, *words], check=check, timeout=timeout)

    def start(self):
        # Its saved sessions stay with the run, not in the user's home.
        self.env["XDG_STATE_HOME"] = os.path.join(self.work, ".fux-state")
        self.run([self.bin, "force-kill-server"], check=False)
        self.calls -= 1
        self.server = subprocess.Popen([self.bin, "__serve", self.env["FUX_SOCKET"]],
                                       env=self.env, cwd=self.work,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        end = time.time() + 5
        while not os.path.exists(self.env["FUX_SOCKET"]) and time.time() < end:
            time.sleep(0.02)

    def stop(self):
        self.flush()
        self.fux("force-kill-server", check=False)
        if self.server:
            try:
                self.server.wait(5)
            except subprocess.TimeoutExpired:
                self.server.kill()

    # ------------------------------------------------- held actions

    def hold(self, pane, words, act, makes=False):
        """Holds an action on a pane, or one that `makes` it. Keys held are
        not joined with the next keys: written together, a program may read
        them as a paste, or Escape and the next key as one key."""
        self.flush()
        self.pending = (pane, words if makes else self.pane(pane) + words, act)

    def act(self, cond, timeout, after=()):
        """Sends the held action with the wait words `cond`: its output."""
        pane, words, act = self.pending
        self.pending = None
        out = self.fux(*words, *cond, str(timeout), "timeout", act, *after,
                       check=False, timeout=timeout + 10)
        if isinstance(pane, _Later):
            if not out.stdout.strip():
                raise RuntimeError(f"fux {act}: {out.returncode}: {out.stderr.strip()[:300]}")
            first = out.stdout.split()[0]
            pane.id = json.loads(out.stdout)["pane"] if first.startswith("{") else first
        return out

    def flush(self):
        """Sends the held action alone: it answers at once."""
        if not self.pending:
            return
        pane, words, act = self.pending
        self.pending = None
        out = self.fux(*words, act)
        if isinstance(pane, _Later):
            pane.id = out.stdout.split()[0]

    def held_for(self, pane):
        """Whether the held action is on this pane (or makes it)."""
        return self.pending is not None and self.pending[0] is pane

    def pane(self, pane):
        return ["str", str(pane), "pane"]

    @staticmethod
    def keyed(keys):
        """The words for keys: each a key by its name, or text (`text:…`, or
        a word no key is called)."""
        words = []
        for k in keys:
            if k.startswith("text:"):
                words += ["str", k[5:], "text"]
            elif is_key_name(k):
                words += ["str", k, "key"]
            else:
                words += ["str", k, "text"]
        return words

    @staticmethod
    def checked(out):
        """A wait's output, or why it failed: the program exited first (3),
        its pane closed (4), the time ran out (124), or a mistake (1)."""
        if out.returncode == 0:
            return out.stdout
        why = out.stderr.strip() or out.stdout.strip()
        if out.returncode in (3, 4):
            raise Exited(why)
        if out.returncode == 124:
            raise Timeout(why)
        raise RuntimeError(f"fux: {out.returncode}: {why[:300]}")

    def wait(self, pane, *cond, timeout=10, after=()):
        """Waits for any of `cond`: armed on the action held for the pane,
        else from now."""
        if self.held_for(pane):
            return self.checked(self.act(cond, timeout, after))
        self.flush()
        out = self.fux(*self.pane(pane), *cond, str(timeout), "timeout", "wait", *after,
                       check=False, timeout=timeout + 10)
        return self.checked(out)

    def wait_or_shown(self, pane, cond, shown, timeout=10, history=False):
        """A wait for what may have come before it: `shown(screen)`, read
        first unless an action is held to arm the wait on, and read again
        if the wait fails, for what came between the read and the wait."""
        if not self.held_for(pane):
            found = shown(self.screen(pane, history))
            if found:
                return found
        try:
            return self.wait(pane, *cond, timeout=timeout)
        except (Exited, Timeout):
            found = shown(self.screen(pane, history))
            if found:
                return found
            raise

    # ------------------------------------------------- the API

    def spawn(self, argv, size=None, name=None):
        words = ["opts"]
        if name:
            words += ["str", name, "name"]
        if size:
            words += [str(size[0]), str(size[1]), "size"]
        words += ["str", argv[0], "program"]
        for a in argv[1:]:
            words += ["str", a, "arg"]
        p = _Later(self)
        self.hold(p, words, "run", makes=True)
        return p

    def shell(self, size=None):
        return self.spawn(["bash"], size=size)

    def keys(self, pane, *keys):
        self.hold(pane, self.keyed(keys), "send-keys")

    def text(self, pane, text):
        self.hold(pane, ["str", text, "text"], "send-keys")

    def paste(self, pane, text):
        self.hold(pane, ["str", text, "paste"], "send-keys")

    def raw(self, pane, data):
        self.text(pane, data)

    def mouse(self, pane, button, row, col):
        kind = {0: "click", 64: "wheel-up", 65: "wheel-down"}[button]
        self.hold(pane, ["str", kind, str(row), str(col), "mouse"], "send-mouse")

    def screen(self, pane, history=False):
        self.flush()
        return self.fux(*self.pane(pane), *(["all-history"] if history else []), "capture").stdout

    def ansi(self, pane):
        self.flush()
        return self.fux(*self.pane(pane), "styles", "capture").stdout

    def state(self, pane):
        self.flush()
        return json.loads(self.fux(*self.pane(pane), "capture", "json").stdout)

    def exit_status(self, pane):
        s = self.state(pane)
        if s["exited"] is not None:
            return s["exited"]
        return 128 + s["signal"] if s.get("signal") else None

    def wait_text(self, pane, text, timeout=10):
        return self.wait_or_shown(pane, ["str", text, "wait-text"], lambda s: text in s, timeout)

    def wait_history(self, pane, text, timeout=10):
        return self.wait_or_shown(pane, ["str", text, "wait-text"], lambda s: text in s, timeout,
                                  history=True)

    def wait_gone(self, pane, text, timeout=10):
        """No word waits for a text to go: each change the program writes,
        then the screen read again."""
        end = time.time() + timeout
        while text in self.screen(pane):
            left = int(end - time.time())
            if left < 1:
                raise Timeout(f"{text!r} still shown")
            try:
                self.wait(pane, "wait-change", timeout=1)
            except Timeout:
                pass
        return True

    def wait_idle(self, pane, ms=300, timeout=10):
        """It writes nothing for a while (a second at least: fux counts
        whole seconds), or it exits."""
        quiet = max(1, -(-ms // 1000))
        return self.wait(pane, "wait-exit", str(quiet), "wait-quiet", timeout=timeout)

    def wait_exit(self, pane, timeout=10):
        """The program's status: `exited`, or 128 and the signal that killed it."""
        s = json.loads(self.wait(pane, "wait-exit", timeout=timeout, after=["json"]))
        return s["exited"] if s.get("exited") is not None else 128 + s["signal"]

    def wait_bell(self, pane, timeout=10):
        return self.wait(pane, "wait-bell", timeout=timeout)

    def send_and_wait_text(self, pane, keys, text, timeout=10):
        self.hold(pane, self.keyed(keys), "send-keys")
        return self.wait(pane, "str", text, "wait-text", timeout=timeout)

    def send_wait(self, pane, keys, *until, timeout=20):
        """Sends keys and waits, the wait armed on them; the screen after.
        `until` is old-style: `--wait-idle MS`, `--wait-text T`, `--wait-exit`."""
        cond = []
        it = iter(until)
        for flag in it:
            if flag == "--wait-idle":
                cond += [str(max(1, -(-int(next(it)) // 1000))), "wait-quiet"]
            elif flag == "--wait-text":
                cond += ["str", next(it), "wait-text"]
            elif flag == "--wait-exit":
                cond += ["wait-exit"]
        self.hold(pane, self.keyed(keys), "send-keys")
        self.wait(pane, *cond, timeout=timeout)
        return self.screen(pane)

    def click_and_wait(self, pane, row, col, text, timeout=10):
        """Clicks, and waits for the text the click brings: the line it is
        on, the answer's second line."""
        self.mouse(pane, 0, row, col)
        return self.wait(pane, "str", text, "wait-text", timeout=timeout).partition("\n")[2].strip()

    def run_command(self, pane, cmd, timeout=15):
        """A shell command, waited for as `fux help-agents` says to without
        marks: a text only its end shows, quoted so the typed line does not
        hold it; numbered, so an earlier command's does not count. Its
        output is what the history holds between its two markers."""
        self.commands += 1
        n = self.commands
        # The status goes before the end's marker: written once it is found.
        self.hold(pane, ["str", f"echo __BE''GIN{n}; {cmd}; echo $? __DO''NE{n}", "text",
                         "str", "Enter", "key"], "send-keys")
        out = self.wait(pane, "str", f"__DONE{n}", "wait-text", timeout=timeout)
        done = re.compile(rf"(\d+) __DONE{n}$")
        status = done.search(out.split("\n")[1].rstrip()).group(1)
        rows = [l.rstrip() for l in self.screen(pane, history=True).split("\n")]
        end = max(i for i, l in enumerate(rows) if done.search(l))
        begin = max(i for i, l in enumerate(rows[:end]) if l == f"__BEGIN{n}")
        return "\n".join(rows[begin + 1:end]), status

    def wait_first_exit(self, panes, timeout=15):
        """The name of the first of several panes whose program exits."""
        end = time.time() + timeout
        while time.time() < end:
            for name, p in panes.items():
                if self.exit_status(p) is not None:
                    return name
            self.nap()
        raise Timeout("no program exited")

    def wait_any_text(self, pane, texts, timeout=15):
        """Which of several texts the pane shows first."""
        cond = [w for t in texts for w in ("str", t, "wait-text")]
        shown = lambda s: next((t for t in texts if t in s), None)
        if not self.held_for(pane):
            found = shown(self.screen(pane))
            if found:
                return found
        try:
            return json.loads(self.wait(pane, *cond, timeout=timeout, after=["json"]))["text"]
        except (Exited, Timeout):
            found = shown(self.screen(pane))
            if found:
                return found
            raise

    def cursor_hidden(self, pane):
        return not self.state(pane)["cursor_visible"]

    def main_screen(self, pane):
        self.flush()
        return self.fux(*self.pane(pane), "main-screen", "all-history", "capture").stdout

    def respawn(self, pane):
        self.hold(pane, [], "respawn")

    def resize(self, pane, cols, rows):
        self.flush()
        self.fux(*self.pane(pane), "tab", str(cols), str(rows), "resize")

    def resize_pct(self, pane, pct):
        self.flush()
        self.fux(*self.pane(pane), str(pct), "percent", "resize-x")

    def split(self, pane, argv):
        words = [*self.pane(pane), "horizontal", "str", argv[0], "program"]
        for a in argv[1:]:
            words += ["str", a, "arg"]
        q = _Later(self)
        self.hold(q, words, "split", makes=True)
        return q

    def kill(self, pane):
        self.flush()
        self.fux(*self.pane(pane), "kill")

    def size_of(self, pane):
        s = self.state(pane)
        return s["cols"], s["rows"]

    def find(self, name):
        self.flush()
        return self.fux("str", name, "pane").stdout.strip()

    def find_title(self, title):
        self.flush()
        # Every pane `run` starts is in workspace agent.
        tabs = json.loads(self.fux("str", "agent", "workspace", "str", "tabs", "get").stdout)
        return next(p["id"] for t in tabs for p in t["panes"] if p["title"] == title)

    def attach_argv(self):
        self.flush()
        return [self.bin]


#: Keys by name, as fux (and tmux) call them: the rest is text.
KEY_NAMES = {"Enter", "Escape", "Tab", "BTab", "BSpace", "Space", "Up", "Down", "Left", "Right",
             "Home", "End", "PageUp", "PageDown", "PPage", "NPage", "DC", "IC", "Delete", "Insert",
             "Backspace"}


def is_key_name(word):
    return (len(word) == 1 or word in KEY_NAMES or re.fullmatch(r"F([1-9]|1[0-2])", word) is not None
            or re.fullmatch(r"([CMS]-)+.+", word) is not None)


# ---------------------------------------------------------------------- tmux


class Tmux(Driver):
    name = "tmux"

    def __init__(self, work, env):
        super().__init__(work, env)
        self.sock = "ab"
        self.n = 0

    def tmux(self, *args, check=True, timeout=120):
        return self.run(["tmux", "-L", self.sock, *args], check=check, timeout=timeout)

    def start(self):
        self.tmux("kill-server", check=False)
        self.calls -= 1
        self.tmux("-f", "/dev/null", "new-session", "-d", "-s", "base", "-x", "80", "-y", "24")
        self.tmux("set", "-g", "remain-on-exit", "on")
        self.tmux("set", "-g", "history-limit", "50000")

    def stop(self):
        self.tmux("kill-server", check=False)

    def spawn(self, argv, size=None, name=None):
        # A session each, so each gets its own size; remain-on-exit keeps the
        # pane, and its status, once the program exits.
        self.n += 1
        cols, rows = size or (80, 24)
        session = name or f"s{self.n}"
        return self.tmux("new-session", "-d", "-s", session, "-x", str(cols), "-y", str(rows),
                         "-P", "-F", "#{pane_id}", "--", *argv).stdout.strip()

    def keys(self, pane, *keys):
        self.tmux("send-keys", "-t", pane, *keys)

    def text(self, pane, text):
        self.tmux("send-keys", "-t", pane, "-l", text)

    def raw(self, pane, data):
        self.tmux("send-keys", "-t", pane, "-l", data)

    def paste(self, pane, text):
        self.run(["tmux", "-L", self.sock, "load-buffer", "-b", "ab", "-"], input=text)
        self.tmux("paste-buffer", "-p", "-d", "-b", "ab", "-t", pane)

    def screen(self, pane, history=False):
        args = ["capture-pane", "-p", "-t", pane] + (["-S", "-"] if history else [])
        lines = self.tmux(*args).stdout.rstrip("\n").split("\n")
        return "\n".join(l.rstrip() for l in lines).rstrip("\n") + "\n"

    def ansi(self, pane):
        return self.tmux("capture-pane", "-p", "-e", "-t", pane).stdout

    def fmt(self, pane, f):
        return self.tmux("display-message", "-p", "-t", pane, f).stdout.strip()

    def exit_status(self, pane):
        dead, _, status = self.fmt(pane, "#{pane_dead} #{pane_dead_status}").partition(" ")
        return int(status) if dead == "1" and status.strip() else None

    def wait_bell(self, pane, timeout=10):
        return self.poll(lambda: self.fmt(pane, "#{window_bell_flag}") == "1", timeout)

    def resize(self, pane, cols, rows):
        self.tmux("resize-window", "-t", pane, "-x", str(cols), "-y", str(rows))

    def resize_pct(self, pane, pct):
        self.tmux("resize-pane", "-t", pane, "-x", f"{pct}%")

    def split(self, pane, argv):
        return self.tmux("split-window", "-h", "-d", "-t", pane, "-P", "-F", "#{pane_id}", "--",
                         *argv).stdout.strip()

    def kill(self, pane):
        # kill-pane hangs the program up; one that ignores it is killed by
        # its pid, which tmux tells.
        pid = self.fmt(pane, "#{pane_pid}")
        self.tmux("kill-pane", "-t", pane)
        self.nap(0.3)
        if pid.isdigit():
            try:
                os.kill(int(pid), 9)
            except ProcessLookupError:
                pass

    def main_screen(self, pane):
        return self.tmux("capture-pane", "-p", "-a", "-S", "-", "-t", pane, check=False).stdout

    def size_of(self, pane):
        cols, rows = self.fmt(pane, "#{pane_width} #{pane_height}").split()
        return int(cols), int(rows)

    def find(self, name):
        return self.fmt(f"={name}:", "#{pane_id}")

    def respawn(self, pane):
        self.tmux("respawn-pane", "-t", pane)

    def cursor_hidden(self, pane):
        return self.fmt(pane, "#{cursor_flag}") == "0"

    def find_title(self, title):
        rows = self.tmux("list-panes", "-a", "-F", "#{pane_id}\t#{pane_title}").stdout.splitlines()
        return next(r.split("\t")[0] for r in rows if r.split("\t", 1)[1:] == [title])

    def attach_argv(self):
        return ["tmux", "-L", self.sock, "attach"]


# -------------------------------------------------------------------- zellij


class Zellij(Driver):
    name = "zellij"

    def __init__(self, work, env):
        super().__init__(work, env)
        self.session = "ab"
        self.first_closed = False

    def z(self, *args, check=True, timeout=120):
        return self.run(["zellij", "--session", self.session, *args], check=check, timeout=timeout)

    def start(self):
        self.stop()
        self.calls -= 2
        self.run(["zellij", "attach", "--create-background", self.session])
        time.sleep(1)

    def stop(self):
        self.run(["zellij", "kill-session", self.session], check=False)
        self.run(["zellij", "delete-session", self.session, "--force"], check=False)

    def panes(self):
        return [p for p in json.loads(self.z("action", "list-panes", "--json").stdout)
                if not p["is_plugin"]]

    def spawn(self, argv, size=None, name=None):
        # Beside the others in the session's tab, the session's own shell
        # closed once a program is there, so that one program has the whole
        # screen; a floating pane is the only kind that takes a size.
        args = ["action", "new-pane"]
        if size:
            args += ["--floating", "--width", str(size[0] + 2), "--height", str(size[1] + 2)]
        if name:
            args += ["--name", name]
        out = self.z(*args, "--", *argv).stdout.strip()
        pane = out.split()[-1]
        if not self.first_closed:
            self.z("action", "close-pane", "-p", "terminal_0")
            self.first_closed = True
        return pane

    def keys(self, pane, *keys):
        names = {"Enter": "Enter", "Escape": "Esc", "Tab": "Tab", "BSpace": "Backspace",
                 "Up": "Up", "Down": "Down", "Left": "Left", "Right": "Right", "BTab": "Shift Tab"}
        for k in keys:
            if k in names:
                name = names[k]
            elif re.fullmatch(r"F\d+", k):
                name = k
            elif k.startswith("C-"):
                name = "Ctrl " + k[2:]
            elif k.startswith("M-"):
                name = "Alt " + k[2:]
            else:
                self.z("action", "write-chars", "-p", pane, k)
                continue
            self.z("action", "send-keys", "-p", pane, name)

    def split(self, pane, argv):
        self.z("action", "focus-pane-id", pane)
        out = self.z("action", "new-pane", "--direction", "right", "--", *argv).stdout.strip()
        return out.split()[-1] if out else f"terminal_{self.panes()[-1]['id']}"

    def text(self, pane, text):
        self.z("action", "write-chars", "-p", pane, text)

    def raw(self, pane, data):
        self.z("action", "write", "-p", pane, *[str(b) for b in data.encode()])

    def paste(self, pane, text):
        self.raw(pane, "\x1b[200~" + text + "\x1b[201~")

    def screen(self, pane, history=False):
        args = ["action", "dump-screen", "-p", pane] + (["--full"] if history else [])
        lines = self.z(*args).stdout.rstrip("\n").split("\n")
        return "\n".join(l.rstrip() for l in lines).rstrip("\n") + "\n"

    def ansi(self, pane):
        return self.z("action", "dump-screen", "-p", pane, "--ansi").stdout

    def info(self, pane):
        n = int(pane.split("_")[-1])
        for p in self.panes():
            if p["id"] == n:
                return p
        raise RuntimeError(f"no pane {pane}")

    def exit_status(self, pane):
        p = self.info(pane)
        return p["exit_status"] if p["exited"] else None

    def wait_text(self, pane, text, timeout=10):
        # subscribe streams the viewport as it changes: no polling.
        self.calls += 1
        proc = subprocess.Popen(["zellij", "--session", self.session, "subscribe", "-p", pane,
                                 "--format", "json"], env=self.env, stdout=subprocess.PIPE)
        end = time.time() + timeout
        seen = b""
        try:
            while time.time() < end:
                ready, _, _ = select.select([proc.stdout], [], [], max(0.0, end - time.time()))
                if not ready:
                    break
                chunk = os.read(proc.stdout.fileno(), 65536)
                if not chunk:
                    break
                seen = (seen + chunk)[-200000:]
                decoded = seen.decode(errors="replace")
                if text in decoded or json.dumps(text)[1:-1] in decoded:
                    return True
        finally:
            proc.kill()
            proc.wait()
        if self.exit_status(pane) is not None:
            raise Exited()
        raise Timeout()

    def size_of(self, pane):
        p = self.info(pane)
        return p["pane_content_columns"], p["pane_content_rows"]

    def kill(self, pane):
        # A session whose last pane closes ends: a shell is left in its place.
        if len(self.panes()) <= 1:
            self.z("action", "new-pane", "--", "sh")
        self.z("action", "close-pane", "-p", pane)

    def find(self, name):
        for p in self.panes():
            if p["title"] == name:
                return f"terminal_{p['id']}"
        raise RuntimeError(f"no pane named {name}")


# --------------------------------------------------------------------- herdr


class Herdr(Driver):
    name = "herdr"

    def __init__(self, work, env):
        super().__init__(work, env)
        self.session = "ab"
        self.server = None

    def h(self, *args, check=True, timeout=120):
        out = self.run(["herdr", "--session", self.session, *args], check=check, timeout=timeout)
        return json.loads(out.stdout) if out.stdout.strip().startswith("{") else out

    def start(self):
        self.run(["herdr", "--session", self.session, "server", "stop"], check=False)
        self.calls -= 1
        # A session's panes are restored from its saved state: each run
        # starts from none.
        import shutil
        shutil.rmtree(os.path.join(self.env["XDG_CONFIG_HOME"], "herdr", "sessions", self.session),
                      ignore_errors=True)
        self.server = subprocess.Popen(["herdr", "--session", self.session, "server"], env=self.env,
                                       cwd=self.work, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL)
        time.sleep(1.5)

    def stop(self):
        self.run(["herdr", "--session", self.session, "server", "stop"], check=False)
        if self.server:
            try:
                self.server.wait(5)
            except subprocess.TimeoutExpired:
                self.server.kill()

    def spawn(self, argv, size=None, name=None):
        # A pane is a shell: the program is typed into it, its status echoed
        # after it as a marker the wait reads.
        result = self.h("workspace", "create", "--cwd", self.work, "--no-focus")["result"]
        pane = result["root_pane"]["pane_id"]
        if name:
            self.h("pane", "rename", pane, name)
        self.h("pane", "wait-output", pane, "--regex", r"[$#>] *$", "--timeout", "5000", check=False)
        cmd = " ".join(shlex.quote(a) for a in argv)
        self.h("pane", "run", pane, f"clear; {cmd}; echo __EX''IT $?")
        return pane

    def shell(self, size=None):
        result = self.h("workspace", "create", "--cwd", self.work, "--no-focus")["result"]
        return result["root_pane"]["pane_id"]

    def split(self, pane, argv):
        new = self.h("pane", "split", pane, "--direction", "right", "--cwd", self.work,
                     "--no-focus")["result"]["pane"]["pane_id"]
        self.h("pane", "wait-output", new, "--regex", r"[$#>] *$", "--timeout", "5000", check=False)
        cmd = " ".join(shlex.quote(a) for a in argv)
        self.h("pane", "run", new, f"clear; {cmd}; echo __EX''IT $?")
        return new

    def keys(self, pane, *keys):
        names = {"BSpace": "backspace", "Escape": "esc", "Enter": "enter", "Tab": "tab",
                 "BTab": "shift+tab", "Up": "up", "Down": "down", "Left": "left", "Right": "right"}
        for k in keys:
            if len(k) == 1:
                self.h("pane", "send-text", pane, k)
                continue
            name = names.get(k, k.lower())
            if k.startswith("C-"):
                name = "ctrl+" + k[2:].lower()
            elif k.startswith("M-"):
                name = "alt+" + k[2:].lower()
            self.h("pane", "send-keys", pane, name)

    def text(self, pane, text):
        self.h("pane", "send-text", pane, text)

    def raw(self, pane, data):
        self.h("pane", "send-text", pane, data)

    def paste(self, pane, text):
        self.raw(pane, "\x1b[200~" + text + "\x1b[201~")

    def screen(self, pane, history=False):
        args = ["pane", "read", pane, "--source", "recent" if history else "visible"]
        if history:
            args += ["--lines", "100000"]
        out = self.h(*args)
        text = out["result"]["read"]["text"] if isinstance(out, dict) else out.stdout
        return "\n".join(l.rstrip() for l in text.split("\n")).rstrip("\n") + "\n"

    def ansi(self, pane):
        out = self.h("pane", "read", pane, "--format", "ansi")
        return out["result"]["read"]["text"] if isinstance(out, dict) else out.stdout

    def wait_text(self, pane, text, timeout=10):
        out = self.h("pane", "wait-output", pane, "--match", text, "--source", "recent-unwrapped",
                     "--timeout", str(int(timeout * 1000)), check=False, timeout=timeout + 10)
        if isinstance(out, dict) and "result" in out:
            return True
        raise Timeout()

    def wait_history(self, pane, text, timeout=10):
        out = self.h("pane", "wait-output", pane, "--match", text, "--source", "recent-unwrapped",
                     "--lines", "100000", "--timeout", str(int(timeout * 1000)), check=False,
                     timeout=timeout + 10)
        if isinstance(out, dict) and "result" in out:
            return True
        raise Timeout()

    def exit_status(self, pane):
        m = re.findall(r"__EXIT (\d+)", self.screen(pane, history=True))
        return int(m[-1]) if m else None

    def wait_exit(self, pane, timeout=10):
        self.wait_text(pane, "__EXIT ", timeout)
        return self.exit_status(pane)

    def kill(self, pane):
        self.h("pane", "close", pane)

    def find(self, name):
        for p in self.h("pane", "list")["result"]["panes"]:
            if p.get("label") == name:
                return p["pane_id"]
        raise RuntimeError(f"no pane named {name}")
