"""Execute the guide's source unchanged on its first invocation.

File arguments supply pane/version inputs exactly as the published instructions
say. The expert-driver tier remains in solutions.py.
"""
import os
from pathlib import Path
import shlex
import subprocess

from drivers import Unsupported

GUIDE = Path(__file__).resolve().parent.parent / "docs" / "agents.md"


def block(label, language="fux"):
    section = GUIDE.read_text().split(label, 1)[1]
    source = section.split("```" + language + "\n", 1)[1].split("```", 1)[0]
    return "\n".join(line.removeprefix("  ") for line in source.splitlines()) + "\n"


def recipe(driver, label, *args):
    if driver.name != "fux":
        raise Unsupported("this tier executes published fuxth source")
    path = Path(driver.work) / "published.fux"
    path.write_text(block(label))
    return driver.fux("--file", str(path), *args, timeout=30).stdout


def prepare(driver, recipe_name):
    """Install delay fixtures before the private server inherits its environment."""
    if driver.name != "fux":
        return
    if recipe_name == "published_prompt":
        startup = Path(driver.work) / "startup.py"
        startup.write_text('import time\nprint("BOOT", flush=True)\ntime.sleep(0.15)\n')
        driver.env["PYTHONSTARTUP"] = str(startup)
    elif recipe_name == "published_exit":
        binary = Path(driver.work) / "bin"
        binary.mkdir()
        shell = binary / "sh"
        shell.write_text('#!/bin/sh\nsleep 0.15\nexec /bin/sh "$@"\n')
        shell.chmod(0o700)
        driver.env["PATH"] = str(binary) + os.pathsep + driver.env["PATH"]


def prompt(driver, args, fx):
    initial = recipe(driver, "**Launch and wait for a prompt.**")
    if "BOOT" not in initial or ">>>" not in initial:
        raise RuntimeError("the launch recipe did not read the delayed prompt")
    version = initial.splitlines()[-1]
    reply = recipe(driver, "**Guard input against the screen you read.**", version)
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
        raise Unsupported("this tier executes published fuxth source")
    pane = driver.spawn(shlex.split(fx) + ["guide_menu"])
    initial = recipe(driver, "**A TUI's initial selection:**", pane)
    if "⟦Alpha⟧" not in initial:
        raise RuntimeError("the first capture did not hold the initial selection")
    output = recipe(driver, "**Change that TUI's selection:**", initial.splitlines()[-1])
    if "⟦Beta⟧" not in output:
        raise RuntimeError("the capture did not hold the new selection")
    driver.send_wait_exit(pane, ["Enter"])


def first_exit(driver, args, fx):
    if driver.name != "fux":
        raise Unsupported("this tier executes the published journal recipe")
    namespace = {"__name__": "published_recipe"}
    exec(compile(block("**First recorded exit across jobs:**", "python"),
                 str(GUIDE), "exec"), namespace)

    def fux(*words, stream=False):
        if stream:
            command = ["timeout", "30", driver.bin, *words]
            driver.calls += 1
            driver.log.append(" ".join(command))
            return subprocess.Popen(command, cwd=driver.work, env=driver.env,
                                    stdout=subprocess.PIPE, text=True)
        return driver.fux(*words, timeout=30).stdout

    return namespace["first_exit"](fux)
