#!/usr/bin/env python3
"""bench-161, part C: actions the hypotheses touch, both builds alternating,
medians of N rounds, in milliseconds. Each round on a fresh private server.

  speed_extra.py BASE_BIN CAND_BIN [ROUNDS]
"""
import os, shutil, statistics, subprocess, sys, time

D = os.path.join(os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench")), "C", "xwork")
EDITOR = (  # a line editor that redraws its whole line, prompt and all, on every key
    "import os, time, tty\ntty.setraw(0)\nline = ''\nos.write(1, b'desk> ')\n"
    "while True:\n    c = os.read(0, 1).decode()\n"
    "    if c in '\\r\\n':\n        os.write(1, b'\\r\\n'); time.sleep(0.2)\n"
    "        os.write(1, ('OK ' + line + '\\r\\ndesk> ').encode()); line = ''; continue\n"
    "    line += c; os.write(1, ('\\r\\x1b[Kdesk> ' + line).encode())\n")
LINES = "while True: print('got', input('> '))"
FLOOD = "import sys; sys.stdout.write(('x'*79+'\\n')*13000 + 'END\\n'); sys.stdout.flush(); input()"
SMALL = "import os\nfor i in range(70000): os.write(1, b'FLOOD-%08d\\n' % i)\nprint('END', flush=True)\ninput()"


def env(tag):
    d = os.path.join(D, tag)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d, mode=0o700)
    e = {k: v for k, v in os.environ.items() if k not in ("FUX_SOCKET", "FUX_PANE", "FUX_SERVER")}
    e.update(XDG_RUNTIME_DIR=d, FUX_SOCKET=os.path.join(d, "s"), XDG_STATE_HOME=os.path.join(d, "st"))
    return e


def run(b, e, *a):
    return subprocess.run([b, *a], env=e, capture_output=True, text=True, timeout=120)


def ms(f):
    t = time.perf_counter(); r = f(); return 1000 * (time.perf_counter() - t), r


def start(b, e, prog, ready):
    out = run(b, e, "opts", "str", "python3", "program", "str", "-c", "arg", "str", prog, "arg",
              "str", ready, "wait-text", "run").stdout
    return out.split()[0] if out else "%0"


def jobs(b, tag):
    e = env(tag)
    run(b, e, "start")
    out = {}
    p = start(b, e, LINES, ">")
    out["erase and retype a line, wait for its reply"] = ms(lambda: run(
        b, e, p, "keys", "str", "NEEDLE", "text", "str", "C-u", "key", "str", "hello", "text", "str", "Enter",
        "key", "str", "got hello", "wait-text", "send-keys"))[0]
    p = start(b, e, EDITOR, "desk>")
    out["line editor redrawing its line: wait for the reply"] = ms(lambda: run(
        b, e, p, "keys", "str", "inspect", "text", "str", "Enter", "key", "str", "OK inspect", "wait-text",
        "send-keys"))[0]
    out["1 MB in one write, wait for its last line"] = ms(lambda: start(b, e, FLOOD, "END"))[0]
    out["70,000 small writes, wait for the last line"] = ms(lambda: start(b, e, SMALL, "END"))[0]
    run(b, e, "force-kill-server")
    return out


def main():
    base, cand = sys.argv[1], sys.argv[2]
    rounds = int(sys.argv[3]) if len(sys.argv) > 3 else 10
    res = {"base": [], "cand": []}
    for _ in range(rounds):
        res["base"].append(jobs(base, "base"))
        res["cand"].append(jobs(cand, "cand"))
    print(f"{'job':52} {'master ms':>10} {'#161 ms':>10} {'#161/m':>7}")
    for k in res["base"][0]:
        b = statistics.median(x[k] for x in res["base"]); c = statistics.median(x[k] for x in res["cand"])
        print(f"{k:52} {b:10.1f} {c:10.1f} {c / b:7.2f}")


if __name__ == "__main__":
    main()
