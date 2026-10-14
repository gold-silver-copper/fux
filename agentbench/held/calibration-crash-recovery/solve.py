"""A program started at a size, in a directory, with a variable set: its
crash's status read, then passed to a second run that prompts."""
import re


def solve(d, work, fixtures):
    def calibrate(mode):
        return d.spawn(["env", "-C", f"{work}/station", "CAL_PROFILE=amber",
                        "python3", "-I", f"{fixtures}/calibrate.py", work, mode], size=(96, 28))

    probe = d.wait_exit(calibrate("probe"), timeout=20)
    p = calibrate("recover")
    d.wait_text(p, "Recovery status", timeout=20)
    d.send_and_wait_text(p, [f"text:recover {probe}", "Enter"], "Confirm seal>", timeout=20)
    seal = re.findall(r"Calibration seal: (\S+)", d.screen(p, history=True))[-1]
    d.send_and_wait_text(p, ["text:" + seal, "Enter"], "Calibration accepted")
    recover = d.wait_exit(p)
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"probe={probe}\nrecover={recover}\nseal={seal}\n")
