"""The scripted tier's solutions: one recipe per kind of task, written once
against the drivers' common operations, so what a tool costs is what its
driver needs to do each operation (a native wait, or a poll). A recipe returns
the task's answer, if it asks for one."""
import re
import shlex
import time

from drivers import Exited, Timeout, is_green, is_red


def argv(fx, prog):
    """A fixture command line, as argv."""
    return shlex.split(fx) + shlex.split(prog)


def lines(text):
    return [l for l in text.split("\n")]


def last_matching(text, prefix):
    found = [l.strip() for l in lines(text) if l.strip().startswith(prefix)]
    return found[-1] if found else None


# ------------------------------------------------------------- start


def spawn_sized(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]), (a["cols"], a["rows"]))
    d.wait_text(p, "size")


def exit_status(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    return str(d.wait_exit(p, timeout=15))


def exit_status_argv(d, a, fx):
    p = d.spawn(a["argv"])
    return str(d.wait_exit(p, timeout=15))


def last_line_matching(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_exit(p)
    return last_matching(d.screen(p, history=True), a["prefix"])


def restart(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_exit(p)
    try:
        d.respawn(p)
        d.wait_exit(p)
    except Exception:
        d.kill(p)
        q = d.spawn(argv(fx, a["prog"]))
        d.wait_exit(q)


def first_of(d, a, fx):
    panes = {name: d.spawn(argv(fx, prog), name=name) for name, prog in zip("abc", a["progs"])}
    finished = []

    def any_done():
        for name, p in panes.items():
            if d.exit_status(p) is not None:
                finished.append(name)
                return True
        return False

    d.poll(any_done, 15)
    return finished[0]


def which_failed(d, a, fx):
    panes = {name: d.spawn(argv(fx, prog), name=name) for name, prog in zip("abc", a["progs"])}
    for name, p in panes.items():
        if d.wait_exit(p) != 0:
            return name


def find_by_name(d, a, fx):
    d.spawn(argv(fx, a["prog"]), name=a["name"])
    p = d.find(a["name"])
    d.wait_text(p, "READY")
    d.text(p, "quit")
    d.keys(p, "Enter")
    d.poll(lambda: d.exit_status(p) is not None, 5)


def stop_stubborn(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "running")
    d.kill(p)
    time.sleep(0.5)


# ------------------------------------------------------------- shell


def _shell(d, a):
    p = d.shell()
    d.wait_idle(p, 300)
    for cmd in [a.get("before"), a["cmd"]]:
        if cmd:
            d.text(p, cmd)
            d.keys(p, "Enter")
    return p


def _sh_result(d, a):
    """Runs the task's command with a marker after it, waits for the marker,
    and gives the output between."""
    p = d.shell()
    d.wait_idle(p, 300)
    if hasattr(d, "run_command"):
        if a.get("before"):
            d.run_command(p, a["before"])
        return (p, *d.run_command(p, a["cmd"]))
    if a.get("before"):
        d.text(p, a["before"])
        d.keys(p, "Enter")
    # Markers around the output, split by quotes so that the command's echo
    # does not read as them.
    d.text(p, f"echo __BE''GIN; {a['cmd']}; echo __DO''NE $?")
    d.keys(p, "Enter")
    d.poll(lambda: re.search(r"^__DONE \d+$", d.screen(p, history=True), re.M), 15)
    rows = [l.rstrip() for l in d.screen(p, history=True).split("\n")]
    end = max(i for i, l in enumerate(rows) if re.match(r"^__DONE \d+$", l))
    begin = max(i for i, l in enumerate(rows[:end]) if l == "__BEGIN")
    return p, "\n".join(rows[begin + 1:end]), rows[end].split()[1]


def sh_status(d, a, fx):
    _, _, status = _sh_result(d, a)
    return str(status)


def sh_output(d, a, fx):
    if a.get("setup_lines"):
        with open("data.txt", "w") as f:
            f.write("".join(f"{i}\n" for i in range(a["setup_lines"])))
    _, body, _ = _sh_result(d, a)
    out = [l.strip() for l in (body or "").split("\n") if l.strip()]
    return out[-1] if out else None


def sh_value(d, a, fx):
    _, body, _ = _sh_result(d, a)
    m = re.search(re.escape(a["key"]) + r"(\S+)", body or "")
    return m.group(1) if m else None


def sh_line_before(d, a, fx):
    _, body, _ = _sh_result(d, a)
    out = [l.strip() for l in (body or "").split("\n")]
    return out[out.index(a["marker"]) - 1]


def sh_confirm(d, a, fx):
    p = d.shell()
    d.wait_idle(p, 300)
    d.text(p, a["cmd"].replace("{fx}", fx))
    d.keys(p, "Enter")
    d.wait_text(p, "[y/N]")
    d.keys(p, "y")
    d.wait_text(p, "done")


def sh_interrupt(d, a, fx):
    p = d.shell()
    d.wait_idle(p, 300)
    d.text(p, a["long"])
    d.keys(p, "Enter")
    d.wait_idle(p, 300)
    d.keys(p, "C-c")
    d.wait_idle(p, 300)
    return sh_output(d, {"cmd": a["after"]}, fx)


def shell_exit(d, a, fx):
    p = d.spawn(["sh"])
    d.wait_idle(p, 300)
    d.text(p, f"exit {a['status']}")
    d.keys(p, "Enter")
    return str(d.wait_exit(p))


# ------------------------------------------------------------- waiting


def line_before(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_history(p, a["marker"])
    out = [l.strip() for l in lines(d.screen(p, history=True))]
    return out[out.index(a["marker"]) - 1]


def line_after(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_idle(p, 300)
    out = [l.strip() for l in lines(d.screen(p, history=True))]
    return out[out.index(a["marker"]) + 1]


def find_line(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_idle(p, 300)
    return last_matching(d.screen(p, history=True), a["prefix"])


def gone_then_type(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, a["gone"])
    d.wait_gone(p, a["gone"])
    d.text(p, a["line"])
    d.keys(p, "Enter")
    d.wait_text(p, "ok:")


def text_then_type(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, a["text"])
    d.text(p, a["line"])
    d.keys(p, "Enter")
    d.wait_text(p, "ok:")


def text_answer(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, a["text"])
    return a["text"]


def first_text(d, a, fx):
    p = d.spawn(a["argv"] if "argv" in a else argv(fx, a["prog"]))
    found = d.poll(lambda: next((t for t in a["texts"] if t in d.screen(p)), None), 15)
    return found


def chat_send(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, ">")
    d.text(p, a["msg"])
    d.keys(p, "Enter")
    d.wait_text(p, "sent:")
    return "ok"


def chat_paste(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, ">")
    d.paste(p, a["text"])
    d.wait_text(p, "[pasted]")
    d.keys(p, "Enter")
    d.wait_text(p, "sent:")


def timeout(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    try:
        d.wait_text(p, a["text"], timeout=a["within"])
        return "ready"
    except Timeout:
        return "timeout"


def ready_or_status(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    try:
        d.wait_text(p, a["text"], timeout=10)
        return "ready"
    except (Exited, Timeout):
        return str(d.exit_status(p))


def bell(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_bell(p, timeout=10)
    return "rang"


def agent_answer(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "agent ready")
    d.text(p, a["msg"])
    d.keys(p, "Enter")
    d.wait_text(p, "answer:", timeout=20)
    return re.findall(r"answer: (\S+)", d.screen(p))[-1]


def agent_permission(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "agent ready")
    d.text(p, a["msg"])
    d.keys(p, "Enter")
    d.wait_text(p, "[y/n]")
    d.keys(p, "y")
    d.wait_text(p, "answer:", timeout=20)
    return re.findall(r"answer: (\S+)", d.screen(p))[-1]


def agent_interrupt(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "agent ready")
    d.text(p, a["msg"])
    d.keys(p, "Enter")
    d.wait_text(p, "working")
    d.keys(p, a["key"])
    d.wait_text(p, "interrupted")
    return "interrupted"


def agent_interrupt_then(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "agent ready")
    d.text(p, a["first"])
    d.keys(p, "Enter")
    d.wait_text(p, "working")
    d.keys(p, "Escape")
    d.wait_text(p, "interrupted")
    d.text(p, a["second"])
    d.keys(p, "Enter")
    d.wait_text(p, "answer:", timeout=20)
    return re.findall(r"answer: (\S+)", d.screen(p))[-1]


def agent_two(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "agent ready")
    for msg in (a["first"], a["second"]):
        before = d.screen(p, history=True).count("answer:")
        d.text(p, msg)
        d.keys(p, "Enter")
        d.poll(lambda: d.screen(p, history=True).count("answer:") > before, 20)
    return re.findall(r"answer: (\S+)", d.screen(p))[-1]


def claude(d, a, fx):
    p = d.spawn(["claude", "--model", "haiku"], (110, 30))
    d.wait_idle(p, 2000, timeout=40)
    if "trust this folder" in d.screen(p):
        d.keys(p, "Down")
        d.keys(p, "Enter")
        d.wait_idle(p, 2000, timeout=40)
    d.text(p, a["msg"])
    d.keys(p, "Enter")
    d.wait_text(p, "esc to interrupt", timeout=15)
    d.wait_gone(p, "esc to interrupt", timeout=90)
    d.wait_idle(p, 1000, timeout=30)
    text = d.screen(p)
    answer = a["expect"] if re.search(r"●\s*" + re.escape(a["expect"]), text) else None
    d.kill(p)
    return answer


# ------------------------------------------------------------- TUIs


def _selected(d, p):
    for text, runs in d_styled(d, p):
        for col, n, attrs in runs:
            if "reverse" in attrs and text[col:col + n].strip():
                return text[col:col + n].strip()
    return None


def d_styled(d, p):
    from drivers import styled_rows
    return styled_rows(d.ansi(p))


def menu(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "Choose")
    for _ in range(12):
        sel = _selected(d, p)
        if sel == a["target"]:
            break
        items = [l.strip() for l in lines(d.screen(p)) if l.startswith("  ") and l.strip()]
        key = "j" if items.index(a["target"]) > items.index(sel) else "k"
        d.keys(p, key)
        d.wait_idle(p, 150)
    d.keys(p, "Enter")
    d.wait_exit(p)


def selected(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "Choose")
    for _ in range(a.get("presses", 0)):
        d.keys(p, "j")
    d.wait_idle(p, 200)
    return _selected(d, p)


def modal(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "Update available")
    d.keys(p, "Escape")
    d.wait_text(p, "command:")
    d.text(p, a["line"])
    d.keys(p, "Enter")
    d.wait_exit(p)


def form(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "Sign up")
    d.text(p, a["name"])
    d.keys(p, "Tab")
    d.text(p, a["age"])
    d.wait_text(p, a["age"])
    d.keys(p, "Enter")
    d.wait_exit(p)


def click(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, a["label"])
    for row, line in enumerate(lines(d.screen(p))):
        col = line.find(a["label"])
        if col >= 0:
            d.mouse(p, 0, row, col + 2)
            break
    d.poll(lambda: _exists("clicked.txt") and open("clicked.txt").read(), 5)


def wheel(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "wheel:")
    for _ in range(abs(a["down"])):
        d.mouse(p, 65 if a["down"] > 0 else 64, 2, 2)
    d.wait_text(p, f"wheel: {a['down']}")


def keys_reads(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "ready")
    for k in a["keys"]:
        d.keys(p, k)
    d.wait_idle(p, 300)


def keys_repeat(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "ready")
    d.keys(p, *[a["key"]] * a["count"])
    d.wait_idle(p, 300)


def type_line(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, a["ready"])
    d.text(p, a["line"])
    d.keys(p, "Enter")
    d.wait_text(p, "saved")


def less_search(d, a, fx):
    with open("lines.data", "w") as f:
        f.write("".join(f"row {i}{' needle here' if i == 338 else ''}\n" for i in range(1, 600)))
    p = d.spawn(["less", "lines.data"], (80, 24))
    d.wait_text(p, "row 1")
    d.text(p, "/" + a["needle"])
    d.keys(p, "Enter")
    d.wait_text(p, "needle here")
    found = last_matching(d.screen(p), "row 338")
    d.keys(p, "q")
    return found


def nvim_edit(d, a, fx):
    open("edit.txt", "w").close()
    p = d.spawn(["nvim", "--clean", "edit.txt"], (80, 24))
    d.wait_idle(p, 500)
    d.keys(p, "i")
    d.text(p, a["text"])
    d.keys(p, "Escape")
    d.text(p, ":wq")
    d.keys(p, "Enter")
    d.wait_exit(p)


def repl(d, a, fx):
    p = d.spawn(["python3", "-q"])
    d.wait_text(p, ">>>")
    d.text(p, a["expr"])
    d.keys(p, "Enter")
    d.poll(lambda: re.search(r"^\d+$", d.screen(p), re.M), 5)
    value = re.findall(r"^(\d+)$", d.screen(p), re.M)[-1]
    d.text(p, "exit()")
    d.keys(p, "Enter")
    return value


# ------------------------------------------------------------- reading


def table_red(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "SERVICE")
    for text, runs in d_styled(d, p):
        if any(is_red(attrs) for _, _, attrs in runs):
            return text.split()[0]


def table_green(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "SERVICE")
    return str(sum(1 for text, runs in d_styled(d, p) if any(is_green(at) for _, _, at in runs)))


def first_line(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_idle(p, 300)
    return lines(d.screen(p))[0].rstrip()


def last_line(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_idle(p, 300)
    return [l for l in lines(d.screen(p)) if l.strip()][-1].strip()


def wrapped_length(d, a, fx):
    p = d.spawn(a["argv"], (80, 24))
    d.wait_text(p, "7777")
    joined = "".join(l.strip() for l in lines(d.screen(p)) if set(l.strip()) == {"7"})
    return str(len(joined))


def behind_alt(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "FULL SCREEN")
    if hasattr(d, "main_screen"):
        return last_matching(d.main_screen(p), a["prefix"])
    return last_matching(d.screen(p, history=True), a["prefix"])


def sized_text(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]), (a["cols"], a["rows"]))
    d.wait_text(p, "size")
    return last_matching(d.screen(p), "size")


def cursor(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]))
    d.wait_text(p, "Choose")
    return "hidden" if d.cursor_hidden(p) else "shown"


# ------------------------------------------------------------- sizes


def resize(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]), (a["cols"], a["rows"]))
    d.wait_text(p, "size")
    d.resize(p, a["to_cols"], a["to_rows"])
    d.wait_text(p, f"size {a['to_cols']}x{a['to_rows']}")


def percent(d, a, fx):
    p = d.spawn(argv(fx, a["left"]), (a["cols"], a["rows"]))
    q = d.split(p, argv(fx, a["right"]))
    d.wait_text(q, "size")
    d.resize_pct(q, a["pct"])
    d.wait_text(q, f"size {a['pct']}x")


def side_by_side(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]), (a["cols"], a["rows"]))
    q = d.split(p, argv(fx, a["prog"]))
    d.wait_text(q, "size")
    d.wait_idle(p, 300)


def report_size(d, a, fx):
    p = d.spawn(argv(fx, a["prog"]), (a["cols"], a["rows"]))
    d.wait_text(p, "loading")
    cols, rows = d.size_of(p)
    return f"{cols}x{rows}"


# ------------------------------------------------------------- cleanup


def stop_all(d, a, fx):
    panes = [d.spawn(argv(fx, a["prog"])) for _ in range(a["count"])]
    for p in panes:
        d.wait_text(p, "loading")
    for p in panes:
        d.kill(p)
    time.sleep(0.5)


def dead_output(d, a, fx):
    p = d.spawn(a["argv"])
    d.wait_exit(p)
    return a["text"] if a["text"] in d.screen(p, history=True) else None


def count_running(d, a, fx):
    panes = [d.spawn(argv(fx, a["prog"])) for _ in range(a["count"])]
    for p in panes:
        d.wait_text(p, "loading")
    return str(sum(1 for p in panes if d.exit_status(p) is None))


def _exists(path):
    import os
    return os.path.exists(path)
