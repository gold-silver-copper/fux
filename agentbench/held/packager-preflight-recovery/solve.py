"""Two runs in a set directory, environment and size: the first's failing
status observed, the second answered at its prompt once ready."""
import re


def solve(d, work, fixtures):
    def start(mode):
        return d.spawn(["env", "-C", f"{work}/project", "BUILD_FLAVOR=release",
                        "python3", "-I", f"{fixtures}/packager.py", work, mode], size=(96, 26))

    preflight = d.wait_exit(start("preflight"), timeout=30)
    p = start("package")
    # The key comes after nine seconds of initialization.
    d.wait_text(p, "enter this key to continue", timeout=30)
    key = re.findall(r"PACKAGING KEY ([0-9a-f]+)", d.screen(p, history=True))[-1]
    d.text(p, key)
    d.keys(p, "Enter")
    package = d.wait_exit(p)
    artifact = re.findall(r"ARTIFACT=([a-z0-9-]+)", d.screen(p, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"preflight_exit={preflight}\npackage_exit={package}\nartifact={artifact}\n")
