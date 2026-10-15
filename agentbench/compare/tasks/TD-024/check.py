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
        lines = (root / "draft.txt").read_text().splitlines()
        source = "\n".join(lines) + "\n"
        r = record(root, "handoff")
        checksum = value("TD-024", "checksum", source, 20)
        token = value("TD-024", "token", source, 12)
        challenge = value("TD-024", "challenge", source, 15)
        receipt = value("TD-024", "receipt", challenge + ":" + token, 22)
        if r.get("lines") != lines or r.get("key") != "F2" or r.get("seal_command") != "seal " + challenge + " " + token or r.get("stage") != "sealed":
            return False, "draft, key, or seal exchange was not completed correctly"
        return answer(root, f"checksum={checksum}\ntoken={token}\nchallenge={challenge}\nreceipt={receipt}\n")
    except (OSError, ValueError, KeyError) as error:
        return False, str(error)
