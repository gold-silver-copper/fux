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
        secret = (root / "access.txt").read_text().rstrip("\n")
        r = record(root, "migrate")
        token = value("TD-023", "token", secret, 13)
        status = str(11 + int(value("TD-023", "status", secret, 6), 16) % 70)
        receipt = value("TD-023", "receipt", token + ":" + status, 18)
        if not r.get("password_ok") or r.get("ack") != "ack " + token + " " + status or r.get("stage") != "confirmed":
            return False, "password or ordered acknowledgement was not completed"
        return answer(root, f"token={token}\nstatus={status}\nreceipt={receipt}\n")
    except (OSError, ValueError, KeyError) as error:
        return False, str(error)
