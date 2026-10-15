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
        source = (root / "request.txt").read_text().rstrip("\n")
        r = record(root, "audit")
        if r.get("submitted") != source or r.get("stage") != "complete":
            return False, "live audit query was not completed"
        tag = value("TD-021", "tag", source, 10)
        digest = value("TD-021", "digest", source, 24)
        seal = value("TD-021", "seal", source, 14)
        return answer(root, f"tag={tag}\ndigest={digest}\nseal={seal}\n")
    except (OSError, ValueError, KeyError) as error:
        return False, str(error)
