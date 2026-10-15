import hashlib
import json
from pathlib import Path


def value(namespace, label, source, width=12):
    return hashlib.sha256((namespace + "|" + label + "|" + source).encode()).hexdigest()[:width].upper()


def record(root, name):
    result = json.loads((root / ".fx" / (name + ".json")).read_text())
    if not result.get("stdin_tty") or not result.get("stdout_tty") or not result.get("ok"):
        raise ValueError(name + " did not finish successfully in a terminal")
    return result


def answer(root, expected):
    if (root / "answer.txt").read_bytes() != expected.encode("ascii"):
        return False, "answer.txt does not exactly match the observed values and required newlines"
    return True, "terminal run and exact answer verified"

def check(work: str) -> tuple[bool, str]:
    try:
        root = Path(work)
        source = (root / "sensor.txt").read_text().strip()
        r = record(root, "pressure")
        if r.get("stage") != "complete":
            return False, "render never completed"
        ticket = value("TD-022", "ticket", source, 11)
        peak = str(1000 + int(value("TD-022", "peak", source, 8), 16) % 8000) + ".125"
        seal = value("TD-022", "seal", source, 16)
        return answer(root, f"ticket={ticket}\npeak={peak}\nseal={seal}\n")
    except (OSError, ValueError, KeyError) as error:
        return False, str(error)
