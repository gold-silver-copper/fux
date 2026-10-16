#!/usr/bin/env python3
"""Codex as the agent: one run of one task with one tool (fux, tmux, zellij
or herdr), in an environment of its own (runtime dir, sockets, config,
state, TMPDIR, HOME), with logging wrappers first in PATH. The multiplexer's
server is started before the agent and stopped after, by its own socket,
and anything left that carries the run's environment is killed; what the run
did is read from the wrappers' log (each call's time, arguments, exit status
and error) and Codex's event stream (tokens, commands, a readable
transcript); whether it succeeded, from the task's checker.

Used as a module (run_one) by the batch driver, or by hand:
  cxagent.py TOOL FUX_BIN TASK_DIR_OR_AGENTBENCH_ID WORK
"""
import importlib.util
import json
import os
import shutil
import shlex
import signal
import subprocess
import sys
import threading
import time

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))

# Test programs get SIGQUIT and the like on purpose: no core dumps (and no
# crash reports) from anything a run starts, which inherits this limit.
import resource  # noqa: E402
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
# The scripted tier (agentbench/run.py and drivers) beside this folder.
HARNESS = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, HARNESS)
import run as ab  # noqa: E402  (the frozen agentbench)
import drivers  # noqa: E402

MODEL = "gpt-6.1-sol"
CODEX = shutil.which("codex") or "codex"
SCHEMA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "agent.schema.json")
TIMEOUT = int(os.environ.get("TIMEOUT", "1800"))
REAL = {n: shutil.which(n) for n in ("tmux", "zellij", "herdr", "sleep")}
TOOLS = ("fux", "tmux", "zellij", "herdr")

LEARN = {
    "fux": "fux, a terminal multiplexer. Learn it with `fux help-agents` (start there), `fux help` "
           "and `fux str WORD explain`. A private fux server is already "
           "running for you: FUX_SOCKET points to it, and nothing in this environment reaches anyone "
           "else's session, so you may use any fux command, kill-server included.",
    "tmux": "tmux, a terminal multiplexer. Learn it with `man tmux` or `tmux list-commands`. A private tmux server is already "
            "running for you: always pass `-L ab` (for example `tmux -L ab list-panes -a`).",
    "zellij": "zellij, a terminal multiplexer. Learn it with `zellij --help` and `zellij action --help`. A private "
              "zellij session named `ab` is already running for you: pass `--session ab` "
              "(for example `zellij --session ab action list-panes --json`).",
    "herdr": "herdr, a terminal multiplexer. Learn it with `herdr --skill`, `herdr --help` and `herdr pane`. A private "
             "herdr session named `ab` is already running for you: pass `--session ab` to every "
             "command (for example `herdr --session ab pane list`). You are allowed to control "
             "it although HERDR_ENV is not set: it is a test session made for you.",
}

PROMPT = """You are an AI agent driving terminal programs with {learn}

Your task:
{goal}

Rules:
- Run programs inside the multiplexer (in its panes), not directly in your own shell, and interact
  with them only through the multiplexer's own commands: start, drive, read and wait with it.
- Work in the current directory ({work}). Write any answer to answer.txt there, with nothing else in it.
- Learn the tool only from its own help and docs named above. Do not read the tool's source code,
  and do not read the source of the programs you are asked to drive: treat them as black boxes.
- Never build anything (no cargo, no make). No network. Do not write outside this directory.
- When you are done, fill in the report: what you did, your grade, the one decisive missing piece
  if any, every bug you met (with a minimal reproduction), and the friction you met.

Grades for your report:
- "worked": done, plainly, with the tool's documented commands: waiting on conditions, not on time;
  reading what you needed directly; no blind retries.
- "hack": done, but only through a workaround (sleep or polling loops in place of a wait, scraping the
  screen for something the tool should report, raw escape sequences, killing and restarting to get
  unstuck, running the program outside the multiplexer, shell tricks around a missing command,
  guessing timing).
- "impossible": you could not do it with this tool.
For hack and impossible, name the ONE decisive missing piece and its layer: "cli" (a command, flag or
word), "waiting" (a condition the tool can't wait on), "reading" (capture format, scrollback,
structure), "input" (keys, paste, control), "session" (attach, naming, finding panes), "docs" (it
exists but the docs didn't say), "errors" (an error that misled), "server" (a bug), or "design_rule".
For worked, "none".
"""


# Inside Codex's sandbox (bubblewrap, a user namespace) root-owned
# directories show as uid 65534, so fux's private-socket check refuses every
# socket path there. The tool's command is therefore a proxy: it hands its
# arguments, directory and stdin to a helper outside the sandbox, which runs
# the real binary against the run's server, logs the call, and hands back
# the output and exit status. Everything else the agent runs stays sandboxed.
PROXY = r"""#!/usr/bin/python3 -I
# Hands a call to the helper outside the sandbox and streams back its output
# as it comes; if this call is interrupted (a timeout, Ctrl-C), the real
# command is stopped too.
import os, signal, sys, tempfile, time
d = tempfile.mkdtemp(prefix="c.", dir=os.environ["FXH_RPC"])
with open(os.path.join(d, "argv"), "wb") as f:
    f.write(b"".join(os.fsencode(a) + b"\0" for a in sys.argv[1:]))
with open(os.path.join(d, "cwd"), "w") as f:
    f.write(os.getcwd())
try:
    if not os.isatty(0):
        import stat
        m = os.fstat(0).st_mode
        if stat.S_ISFIFO(m) or stat.S_ISREG(m):
            with open(os.path.join(d, "stdin"), "wb") as f:
                f.write(sys.stdin.buffer.read())
except OSError:
    pass
def stop(*_):
    open(os.path.join(d, "kill"), "w").close()
    os._exit(143)
for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    signal.signal(sig, stop)
open(os.path.join(d, "ready"), "w").close()
pos = {"out": 0, "err": 0}
def pump():
    for name, fd in (("out", 1), ("err", 2)):
        try:
            with open(os.path.join(d, name), "rb") as f:
                f.seek(pos[name])
                data = f.read()
        except OSError:
            continue
        if data:
            pos[name] += len(data)
            os.write(fd, data)
while not os.path.exists(os.path.join(d, "done")):
    pump()
    time.sleep(0.005)
pump()
code = int(open(os.path.join(d, "code")).read() or 1)
import shutil
shutil.rmtree(d, ignore_errors=True)
sys.exit(code)
"""


# The new fux runs inside the sandbox as it is: its wrapper only logs the
# call and execs the real binary, which reaches the run's server through its
# door (a FIFO), since the sandbox refuses Unix sockets.
DIRECT = r"""#!/bin/sh
t=$(date +%s.%N)
e=$(mktemp)
{real} "$@" 2> "$e"
c=$?
cat "$e" >&2
h=$(head -c 300 "$e" | tr '\t\n' '  ')
rm -f "$e"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$t" fux "$*" "$c" "$(date +%s.%N)" "$h" >> "{log}"
exit $c
"""


def direct(tool, fux_bin):
    """Whether the agent calls the binary itself: every fux but the base."""
    return tool == "fux" and os.path.basename(fux_bin) != "fux-base"


def shims(env_dir, tool, log, fux_bin):
    path = os.path.join(env_dir, "bin")
    os.makedirs(path, exist_ok=True)
    with open(os.path.join(path, tool), "w") as f:
        if direct(tool, fux_bin):
            f.write(DIRECT.format(log=log, real=shlex.quote(os.path.realpath(fux_bin))))
        else:
            f.write(PROXY)
    with open(os.path.join(path, "sleep"), "w") as f:
        f.write(f'#!/bin/sh\nprintf "%s\\t%s\\t%s\\n" "$(date +%s.%N)" sleep "$*" >> "{log}"\n'
                f'exec /usr/bin/sleep "$@"\n')
    for name in (tool, "sleep"):
        os.chmod(os.path.join(path, name), 0o755)
    return path


class Helper(threading.Thread):
    """Runs the proxied calls of one run, outside the sandbox."""

    def __init__(self, rpc, real, name, env, log):
        super().__init__(daemon=True)
        self.rpc, self.real, self.name, self.env, self.log = rpc, real, name, env, log
        self.stop = threading.Event()
        self.taken = set()

    def run(self):
        while not self.stop.is_set():
            try:
                entries = os.listdir(self.rpc)
            except OSError:
                entries = []
            for e in entries:
                d = os.path.join(self.rpc, e)
                if e in self.taken or not os.path.exists(os.path.join(d, "ready")):
                    continue
                self.taken.add(e)
                threading.Thread(target=self.serve, args=(d,), daemon=True).start()
            time.sleep(0.01)

    def serve(self, d):
        try:
            argv = open(os.path.join(d, "argv"), "rb").read().split(b"\0")[:-1]
            argv = [a.decode(errors="surrogateescape") for a in argv]
            cwd = open(os.path.join(d, "cwd")).read().strip()
            sin = os.path.join(d, "stdin")
            if not os.path.isdir(cwd):
                cwd = self.env["XDG_RUNTIME_DIR"]
            started = time.time()
            with open(os.path.join(d, "out"), "wb") as out, open(os.path.join(d, "err"), "wb") as err, \
                    open(sin if os.path.exists(sin) else os.devnull, "rb") as inp:
                p = subprocess.Popen([self.real, *argv], stdin=inp, stdout=out, stderr=err, cwd=cwd,
                                     env=self.env, start_new_session=True)
                code = None
                while code is None:
                    try:
                        code = p.wait(0.02)
                    except subprocess.TimeoutExpired:
                        killed = os.path.exists(os.path.join(d, "kill"))
                        if killed or time.time() - started > 1800:
                            try:
                                os.killpg(p.pid, signal.SIGKILL)
                            except OSError:
                                pass
                            code = p.wait()
                            if not killed:
                                err.write(b"proxy: the command ran past 30 minutes\n")
                                code = 124
            code = code if code >= 0 else 128 - code
            head = open(os.path.join(d, "err"), "rb").read(300).decode(errors="replace")
            head = head.replace("\t", " ").replace("\n", " ")
            with open(self.log, "a") as f:
                f.write(f"{started:.6f}\t{self.name}\t{shlex.join(argv)}\t{code}\t{time.time():.6f}\t{head}\n")
            open(os.path.join(d, "code"), "w").write(str(code))
            open(os.path.join(d, "done"), "w").close()
        except OSError:
            pass


def make_env(work, tool, fux_bin):
    e = os.path.join(work, ".env")
    dirs = {"XDG_RUNTIME_DIR": "rt", "TMUX_TMPDIR": "tmux", "XDG_CONFIG_HOME": "config",
            "XDG_STATE_HOME": "state", "TMPDIR": "tmp", "ZELLIJ_SOCKET_DIR": "zs",
            "ZELLIJ_CONFIG_DIR": "zc"}
    # A HOME of its own, so the login shell Codex runs commands in loads no
    # profile of the user's (which would put another fux first in PATH).
    home = os.path.join(e, "home")
    os.makedirs(home, exist_ok=True)
    env = {"HOME": home, "CODEX_HOME": os.path.expanduser("~/.codex"),
           "USER": os.environ.get("USER", ""), "LANG": "en_US.UTF-8",
           "TERM": "xterm-256color", "PYTHONDONTWRITEBYTECODE": "1"}
    # Unix socket paths are limited to 108 bytes, and a socket under a deep
    # run folder doesn't fit (herdr keeps its sockets in its config folder).
    # The tools that run outside the sandbox get their folders under a short
    # path of the run's own; fux, called inside the sandbox, keeps them in
    # the work folder, which the sandbox lets it write.
    short = e
    if tool != "fux":
        import hashlib
        short = os.path.join(H, "s", hashlib.sha1(work.encode()).hexdigest()[:10])
        shutil.rmtree(short, ignore_errors=True)
        os.makedirs(short, mode=0o700)
    for k, sub in dirs.items():
        p = os.path.join(e if k in ("TMPDIR",) else short, sub)
        os.makedirs(p, mode=0o700, exist_ok=True)
        os.chmod(p, 0o700)
        env[k] = p
    os.makedirs(os.path.join(e, "fux"), mode=0o700, exist_ok=True)
    env["FUX_SOCKET"] = os.path.join(e, "fux", "s")
    log = os.path.join(work, ".calls.log")
    env["FXH_RPC"] = os.path.join(work, ".rpc")
    os.makedirs(env["FXH_RPC"], exist_ok=True)
    env["PATH"] = ":".join([shims(e, tool, log, fux_bin), os.path.dirname(os.path.realpath(CODEX)),
                            os.path.dirname(CODEX), "/usr/local/bin", "/usr/bin", "/bin"])
    for k in list(dirs) + ["FUX_SOCKET"]:
        assert env[k].startswith(H), k
    return env


def load_task(ref):
    """A task: a held-out spec directory, or an agentbench task id."""
    if os.path.isdir(ref):
        spec = json.load(open(os.path.join(ref, "spec.json")))
        spec["dir"] = os.path.abspath(ref)
        return spec
    t = next(t for t in ab.tasks(os.path.join(HARNESS, "tasks.toml")) if t["id"] == ref)
    t["category"] = t["cat"]
    return t


def fixtures_dir(work):
    """Where a run's fixtures are copied: beside its work directory, so the
    agent's directory holds nothing of the spec (no checker, no solution)."""
    return work.rstrip("/") + ".fixtures"


def goal_text(task, work):
    g = task["goal"].replace("{fx}", ab.FX)
    if task.get("dir"):
        g = g.replace("{fixtures}", fixtures_dir(work)).replace("{work}", work)
    return g


def setup(task, work):
    if task.get("dir"):
        dst = fixtures_dir(work)
        shutil.rmtree(dst, ignore_errors=True)
        shutil.copytree(os.path.join(task["dir"], "fixtures"), dst)
        os.makedirs(os.path.join(work, ".fx"), exist_ok=True)
    for name, text in (task.get("setup") or {}).get("files", {}).items():
        p = os.path.join(work, name)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write(text)


def check(task, work):
    if task.get("dir"):
        spec = importlib.util.spec_from_file_location("check_" + task["id"].replace("-", "_"),
                                                      os.path.join(task["dir"], "check.py"))
        mod = importlib.util.module_from_spec(spec)
        try:
            spec.loader.exec_module(mod)
            ok, why = mod.check(work)
            return bool(ok), str(why)[:500]
        except Exception as e:  # a broken checker is the spec's fault: recorded as such
            return False, f"checker error: {type(e).__name__}: {e}"[:500]
    return ab.check(task, work)


def reap_env(env):
    """Kills whatever is left of a run: every process whose environment holds
    the run's own runtime folder (a path no other process has), such as a
    second server an agent started under another name."""
    mark = ("XDG_RUNTIME_DIR=" + env["XDG_RUNTIME_DIR"]).encode() + b"\0"
    me = os.getpid()
    for _ in range(3):
        found = False
        for pid in os.listdir("/proc"):
            if not pid.isdigit() or int(pid) == me:
                continue
            try:
                with open(f"/proc/{pid}/environ", "rb") as f:
                    if mark not in f.read() + b"\0":
                        continue
                os.kill(int(pid), signal.SIGKILL)
                found = True
            except (OSError, ValueError):
                pass
        if not found:
            break
        time.sleep(0.2)


def stop_all(tool, env, fux_bin, work, driver=None):
    """Stops this run's server, by its own socket, and whatever its programs
    left by the pids they recorded, and reaps the server the driver started
    (no zombie left). Never matches processes by name."""
    if driver is not None:
        driver.stop()
    cmds = {"fux": [[fux_bin, "force-kill-server"]],
            "tmux": [["tmux", "-L", "ab", "kill-server"]],
            "zellij": [["zellij", "kill-session", "ab"], ["zellij", "delete-session", "ab", "--force"]],
            "herdr": [["herdr", "--session", "ab", "server", "stop"]]}[tool]
    for cmd in cmds:
        try:
            subprocess.run(cmd, env=env, capture_output=True, timeout=30)
        except subprocess.TimeoutExpired:
            pass
    ab.reap(work)
    reap_env(env)
    for root, _, files in os.walk(os.path.join(work, ".fx")):
        for name in files:
            if name.endswith(".pid"):
                try:
                    os.kill(int(open(os.path.join(root, name)).read().split()[0]), signal.SIGKILL)
                except (ValueError, IndexError, ProcessLookupError, PermissionError, OSError):
                    pass


def run_one(tool, fux_bin, ref, work):
    task = load_task(ref)
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    env = make_env(work, tool, fux_bin)
    setup(task, work)
    # The server, its panes and the helper see a plain PATH: the proxies
    # are for the agent's sandboxed shell only.
    plain = dict(env, PATH="/usr/local/bin:/usr/bin:/bin")
    d = ab.driver_for(tool, work, dict(plain), fux_bin)
    d.start()
    real = fux_bin if tool == "fux" else REAL[tool]
    helper = Helper(env["FXH_RPC"], real, tool, dict(plain), os.path.join(work, ".calls.log"))
    if not direct(tool, fux_bin):
        helper.start()
    learn = LEARN[tool]
    # A guide named after the binary (bench-161 gave each build an opaque
    # name and its own docs/agents.md) is offered beside the built-in help.
    guide = os.path.join(H, "guides", os.path.basename(fux_bin) + ".md")
    if tool == "fux" and os.path.exists(guide):
        learn = learn.replace("`fux str WORD explain`.", f"`fux str WORD explain`, and the guide at {guide}.")
    prompt = PROMPT.format(learn=learn, goal=goal_text(task, work), work=work)
    open(os.path.join(work, ".prompt.md"), "w").write(prompt)
    rec = {"id": task["id"], "tool": tool, "bin": tool,
           "direct": direct(tool, fux_bin),
           "category": task.get("category")}
    started = time.time()
    try:
        with open(os.path.join(work, ".codex.jsonl"), "w") as log:
            p = subprocess.run([CODEX, "exec", "-m", MODEL, "-c", 'service_tier="default"',
                                "-c", 'model_reasoning_effort="high"', "-s", "workspace-write",
                                "--ephemeral", "--skip-git-repo-check", "--json", "-C", work,
                                "--output-schema", SCHEMA, "-o", os.path.join(work, ".report.json"), "-"],
                               input=prompt, stdout=log, stderr=subprocess.STDOUT, text=True,
                               env=env, cwd=work, timeout=TIMEOUT)
        rec["codex_exit"] = p.returncode
        rec["timed_out"] = False
    except subprocess.TimeoutExpired:
        rec["timed_out"] = True
    rec["seconds"] = round(time.time() - started, 1)
    try:
        ok, why = check(task, work)
    finally:
        helper.stop.set()
        stop_all(tool, plain, fux_bin, work, d)
    rec.update(ok=ok, why=why)
    calls = []
    if os.path.exists(os.path.join(work, ".calls.log")):
        calls = [l.split("\t") for l in open(os.path.join(work, ".calls.log")).read().splitlines()]
    rec["tool_calls"] = sum(1 for c in calls if len(c) > 1 and c[1] == tool)
    rec["tool_errors"] = sum(1 for c in calls if len(c) > 3 and c[1] == tool and c[3] not in ("0", ""))
    rec["tool_seconds"] = round(sum(float(c[4]) - float(c[0]) for c in calls
                                    if len(c) > 4 and c[1] == tool and _num(c[4])), 1)
    rec["sleeps"] = sum(1 for c in calls if len(c) > 1 and c[1] == "sleep")
    rec["sleep_s"] = round(sum(_secs(c[2]) for c in calls if len(c) > 2 and c[1] == "sleep"), 1)
    try:
        rec["report"] = json.load(open(os.path.join(work, ".report.json")))
    except (OSError, json.JSONDecodeError):
        rec["report"] = None
    rec.update(transcript(work))
    json.dump(rec, open(os.path.join(work, ".result.json"), "w"), indent=1)
    return rec


def _secs(text):
    try:
        return float(text.split()[0])
    except (ValueError, IndexError):
        return 0.0


def _num(text):
    try:
        float(text)
        return True
    except ValueError:
        return False


def transcript(work):
    """Reads Codex's event stream: token usage, the agent's shell commands,
    and a readable transcript (.codex.log) for the judges."""
    usage = {"input": 0, "cached": 0, "output": 0, "reasoning": 0}
    cmds = failed = 0
    lines = []
    try:
        events = open(os.path.join(work, ".codex.jsonl"), errors="replace").read().splitlines()
    except OSError:
        events = []
    for raw in events:
        try:
            e = json.loads(raw)
        except json.JSONDecodeError:
            lines.append(raw)
            continue
        if e.get("type") == "turn.completed":
            u = e.get("usage") or {}
            usage["input"] += u.get("input_tokens", 0)
            usage["cached"] += u.get("cached_input_tokens", 0)
            usage["output"] += u.get("output_tokens", 0)
            usage["reasoning"] += u.get("reasoning_output_tokens", 0)
        if e.get("type") != "item.completed":
            continue
        item = e.get("item") or {}
        kind = item.get("type")
        if kind == "agent_message":
            lines.append("codex\n" + item.get("text", ""))
        elif kind == "reasoning":
            lines.append("thinking\n" + item.get("text", ""))
        elif kind == "command_execution":
            cmds += 1
            failed += item.get("exit_code") not in (0, None)
            out = item.get("aggregated_output", "")
            if len(out) > 6000:
                out = out[:3000] + f"\n... [{len(out) - 6000} bytes cut] ...\n" + out[-3000:]
            lines.append(f"exec\n{item.get('command')}\nexit {item.get('exit_code')}\n{out}")
        elif kind == "file_change":
            lines.append("file change\n" + json.dumps(item.get("changes")))
        elif kind == "error":
            lines.append("error\n" + item.get("message", ""))
    with open(os.path.join(work, ".codex.log"), "w") as f:
        f.write("\n\n".join(lines) + "\n")
    return {"tokens": usage["input"] + usage["output"], "usage": usage,
            "shell_commands": cmds, "shell_failed": failed}


if __name__ == "__main__":
    tool, fux_bin, ref, work = sys.argv[1:5]
    r = run_one(tool, os.path.abspath(fux_bin), ref, os.path.abspath(work))
    print(json.dumps({k: v for k, v in r.items() if k != "report"}, indent=1))


def writer_env(work, tools):
    """An environment for a Codex writer run whose `tools` (name -> real
    executable) are proxied to helpers outside the sandbox. Returns (env,
    helpers); stop the helpers when the run ends."""
    e = os.path.join(work, ".env")
    home = os.path.join(e, "home")
    rpc_root = os.path.join(e, "rpc")
    os.makedirs(home, exist_ok=True)
    # BENCH_DIR goes with it: the helpers' HOME is the run's own, so the
    # default (under ~) would point elsewhere.
    env = {"HOME": home, "CODEX_HOME": os.path.expanduser("~/.codex"), "USER": os.environ.get("USER", ""),
           "LANG": "en_US.UTF-8", "TERM": "xterm-256color", "PYTHONDONTWRITEBYTECODE": "1", "BENCH_DIR": H}
    binp = os.path.join(e, "bin")
    os.makedirs(binp, exist_ok=True)
    helpers = []
    plain = dict(env, PATH="/usr/local/bin:/usr/bin:/bin")
    for name, real in tools.items():
        rpc = os.path.join(rpc_root, name)
        os.makedirs(rpc, exist_ok=True)
        with open(os.path.join(binp, name), "w") as f:
            f.write(PROXY.replace('os.environ["FXH_RPC"]', repr(rpc)))
        os.chmod(os.path.join(binp, name), 0o755)
        h = Helper(rpc, real, name, plain, os.path.join(work, ".calls.log"))
        h.start()
        helpers.append(h)
    env["PATH"] = ":".join([binp, os.path.dirname(os.path.realpath(CODEX)), os.path.dirname(CODEX),
                            "/usr/local/bin", "/usr/bin", "/bin"])
    return env, helpers
