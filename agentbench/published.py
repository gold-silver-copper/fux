"""Execute the guide's source unchanged on its first invocation.

File arguments supply pane/version inputs exactly as the published instructions
say. The expert-driver tier remains in solutions.py.
"""
import os
from pathlib import Path
import shlex

from drivers import Unsupported, read_data, short

GUIDE = Path(__file__).resolve().parent.parent / "docs" / "agents.md"


def block(label, language="grift"):
    section = GUIDE.read_text().split(label, 1)[1]
    source = section.split("```" + language + "\n", 1)[1].split("```", 1)[0]
    return "\n".join(line.removeprefix("  ") for line in source.splitlines()) + "\n"


def recipe(driver, label, *args, filename=None, before=""):
    if driver.name != "fux":
        raise Unsupported("this tier executes published grift source")
    source = before + block(label)
    command = ["timeout", "30", driver.bin]
    if filename is None:
        return driver.run([*command, "-"], input=source, timeout=35).stdout
    path = Path(driver.work) / filename
    path.write_text(source)
    return driver.run([*command, "--file", filename, *args], timeout=35).stdout


def prepare(driver, recipe_name):
    """Install delay fixtures before the private server inherits its environment."""
    if driver.name != "fux":
        return
    binary = Path(driver.work) / "bin"
    binary.mkdir()
    (binary / "fux").symlink_to(Path(driver.bin).resolve())
    driver.env["PATH"] = str(binary) + os.pathsep + driver.env["PATH"]
    if recipe_name == "published_prompt":
        startup = Path(driver.work) / "startup.py"
        startup.write_text('import time\nprint("BOOT", flush=True)\ntime.sleep(0.15)\n')
        driver.env["PYTHONSTARTUP"] = str(startup)
    elif recipe_name == "published_exit":
        shell = binary / "sh"
        shell.write_text('#!/bin/sh\nsleep 0.15\nexec /bin/sh "$@"\n')
        shell.chmod(0o700)


def prompt(driver, args, fx):
    initial = recipe(driver, "**Launch and wait for a prompt.**")
    if "BOOT" not in initial or ">>>" not in initial:
        raise RuntimeError("the launch recipe did not read the delayed prompt")
    version = initial.splitlines()[-1]
    reply = recipe(driver, "**Guard input against the screen you read.**", version, filename="reply.grift")
    if "\n42\n" not in reply:
        raise RuntimeError("the guarded recipe did not read the result")
    return "42"


def exit_status(driver, args, fx):
    output = recipe(driver, "**Run a command, read its output and actual exit status.**")
    if output != "result=42\n7\n":
        raise RuntimeError("the exit recipe lost output or status: " + repr(output))
    return "7"


def tui(driver, args, fx):
    if driver.name != "fux":
        raise Unsupported("this tier executes published grift source")
    pane = driver.spawn(shlex.split(fx) + ["guide_menu"])
    initial = recipe(driver, "**A TUI's initial selection:**", pane, filename="selection.grift")
    if "⟦Alpha⟧" not in initial:
        raise RuntimeError("the first capture did not hold the initial selection")
    # The guide says to put `until-marked` first.
    selection = block("**A TUI's initial selection:**")
    until_marked = selection.split("(define! seen (until-marked", 1)[0]
    output = recipe(driver, "**Change that TUI's selection:**", initial.splitlines()[-1],
                    filename="select-next.grift", before=until_marked)
    if "⟦Beta⟧" not in output:
        raise RuntimeError("the capture did not hold the new selection")
    driver.send_wait_exit(pane, ["Enter"])


def first_exit(driver, args, fx):
    if driver.name != "fux":
        raise Unsupported("this tier executes the published journal recipe")
    # Keep the private server/journal alive after the recipe cleans its jobs.
    keeper = driver.spawn(["cat"])
    try:
        cursor = driver.events_cursor()
        path = Path(driver.work) / "first-exit.grift"
        path.write_text(block("**First recorded exit across jobs:**"))
        output = driver.run(["timeout", "30", driver.bin, "--file", path.name], timeout=35).stdout
        _, jobs, first = read_data(output)
        # Replay independently: sleep lengths and launch latency are not an oracle.
        panes = {short(pane): short(pane) for pane in jobs}
        recorded = driver.wait_first_exit(panes, cursor)
        if first["event"] != "exited" or short(first["pane"]) != recorded:
            raise RuntimeError("published result differs from the first journal exit")
        return "recorded"
    finally:
        driver.kill(keeper)
