#!/usr/bin/env python3
"""Runs the sealed oracle fixtures (F/run/FX-*) through a fux binary, once.

For each scenario: the fixture starts in a pane of a private server with its
side log on (FUX_ORACLE_LOG, frozen format in F/side-log-format.md); each
step's keys go in as an agent sends them (`keys ... send-keys`, settling);
then every word of the step is asked of fux with an instant wait (`0
timeout wait`, which counts from the step's input and sends nothing):

- a word the side log shows the program wrote as output since the step's
  input must be met (else: missed-output);
- a word typed in the step that came back only as echo (no output write
  holds it from a word's start) must not be met (else: echo-taken-as-output).

Each disagreement names the kept rule that decides it, if one does
(notes kept-rules.md): 1 full screen is all output; 2 before Enter a text
made only of typed characters is echo.

  fixture_run.py BIN FIXTURE_DIR OUT.json

bench-161: any fixture folder (FX-*/ or FG-*/ subfolders with scenarios.json),
each check keyed (fixture, scenario, step, word, want) to pair builds.
"""
import base64, json, os, re, shutil, subprocess, sys, tempfile, time
from collections import Counter

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
WORD = re.compile(r"[A-Za-z0-9_]{3,}")
NAMES = {"\r": "Enter", "\x7f": "BSpace", "\x08": "C-h", "\t": "Tab", "\x1b": "Escape",
         "\x1b[A": "Up", "\x1b[B": "Down", "\x1b[C": "Right", "\x1b[D": "Left",
         "\x1b[H": "Home", "\x1b[F": "End", "\x1b[3~": "Delete"}


def keys_words(keys):
    """`keys` as fux words: printable runs as text, the rest as key names."""
    out, text, i = [], "", 0
    while i < len(keys):
        seq = next((s for s in ("\x1b[3~", "\x1b[A", "\x1b[B", "\x1b[C", "\x1b[D", "\x1b[H", "\x1b[F")
                    if keys.startswith(s, i)), None)
        c = keys[i]
        if seq or not c.isprintable():
            if text:
                out += ["str", text, "text"]; text = ""
            name = NAMES.get(seq or c) or f"C-{chr(ord(c) + 96)}"
            out += ["str", name, "key"]
            i += len(seq) if seq else 1
        else:
            text += c; i += 1
    if text:
        out += ["str", text, "text"]
    return out


def records(log):
    try:
        return [json.loads(l) for l in open(log).read().splitlines() if l.strip()]
    except (OSError, json.JSONDecodeError):
        return []


ESCAPES = re.compile(r"\x1b\[[0-9;?<=>]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[ -/]*[0-~]")


def written(recs, role):
    """What the fixture wrote in `role`, its escape sequences as blanks: the
    text a screen shows."""
    raw = b"".join(base64.b64decode(r["b64"]) for r in recs
                   if r.get("ev") == "write" and r.get("role") == role).decode("utf-8", "replace")
    return ESCAPES.sub(" ", raw)


def has(text, word):
    return re.search(r"(?<![A-Za-z0-9_])" + re.escape(word), text, re.I) is not None


def submultiset(word, typed):
    return not (Counter(word.lower()) - Counter(typed.lower()))


def main():
    bin_, RUN, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
    work = tempfile.mkdtemp(dir=os.path.join(H, "A")); os.chmod(work, 0o700)
    env = {k: v for k, v in os.environ.items() if k not in ("FUX_SOCKET", "FUX_PANE", "FUX_SERVER")}
    env.update(XDG_RUNTIME_DIR=work, XDG_STATE_HOME=work + "/st", FUX_SOCKET=work + "/s")
    fux = lambda *a, t=120: subprocess.run([bin_, *a], env=env, capture_output=True, text=True, timeout=t)
    fux("start")
    results = []
    for fx in sorted(d for d in os.listdir(RUN) if d.startswith(("FX-", "FG-"))):
        for sc in json.load(open(os.path.join(RUN, fx, "scenarios.json"))):
            log = os.path.join(work, f"{fx}-{sc['name']}.jsonl")
            cols, rows = sc.get("size", [80, 24])
            start = ["opts", str(cols), str(rows), "size", "str", "FUX_ORACLE_LOG", "str", log, "env",
                     "str", "python3", "program"]
            for a in [os.path.join(RUN, fx, "fixture.py")] + [str(x) for x in sc.get("args", [])]:
                start += ["str", a, "arg"]
            r = fux(*start, "run")
            pane = (r.stdout.split() or ["?"])[0]
            typed_line, steps = "", []
            for n, st in enumerate(sc["steps"]):
                before = len(records(log))
                keys = st.get("keys", "")
                s = fux("str", pane, "pane", "keys", *keys_words(keys), "send-keys", "json") if keys else None
                time.sleep(st.get("pause_ms", 0) / 1000)
                recs = records(log)[before:]
                output, echo = written(recs, "output"), written(recs, "echo")
                alt = next((r["alt"] for r in reversed(records(log)) if r.get("ev") == "mode"), False)
                parts = re.split(r"[\r\n]", keys)
                typed_before_enter = typed_line + parts[0]
                typed_line = parts[-1] if len(parts) > 1 else typed_line + keys
                outs = sorted({w for w in WORD.findall(output)})
                typed = sorted({w for w in WORD.findall(re.sub(r"\x1b\[[0-9;~A-Z]*|[\x00-\x1f\x7f]", " ", keys))})
                quiet = [w for w in typed if not has(output, w)]
                checks, dis = [], []
                for w, want in [(w, True) for w in outs[:8]] + [(w, False) for w in quiet[:8]]:
                    q = fux("str", pane, "pane", "opts", "str", w, "wait-text", "0", "timeout", "wait", "json")
                    met = q.returncode == 0 and '"met":"text"' in q.stdout
                    late = written(records(log)[before:], "output")
                    if not want and has(late, w):
                        continue  # output came for it after all, while asking
                    checks.append({"word": w, "want": want, "met": met})
                    if met != want:
                        cls = "missed-output" if want else "echo-taken-as-output"
                        rule = None
                        if not want and alt:
                            rule = "kept 1: full screen is all output"
                        elif want and "\r" not in keys and submultiset(w, typed_before_enter + keys):
                            rule = "kept 2: before Enter, typed characters only is echo"
                        dis.append({"word": w, "class": cls, "rule": rule, "answer": q.stdout.strip()[:300],
                                    "status": q.returncode})
                steps.append({"step": n, "keys": keys, "send_status": s.returncode if s else None,
                              "output_words": outs[:8], "echo_words": quiet[:8], "echo_written": bool(echo),
                              "checks": checks, "disagreements": dis})
            fux("str", pane, "pane", "kill")
            results.append({"fixture": fx, "scenario": sc["name"], "steps": steps,
                            "log_records": len(records(log))})
            dis = sum(len(s["disagreements"]) for s in steps)
            print(f"{fx} {sc['name']}: {len(steps)} steps, {dis} disagreements", flush=True)
    fux("force-kill-server")
    json.dump(results, open(out_path, "w"), indent=1)
    shutil.copytree(work, out_path + ".logs", dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns("s", "st"))
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
