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
        source = (root / "manifest.txt").read_text().strip()
        artifact = value("TD-025", "artifact", source, 14)
        key = value("TD-025", "key", source, 12)
        nonce = value("TD-025", "nonce", source, 11)
        ticket = value("TD-025", "ticket", artifact + ":" + nonce, 15)
        digest = value("TD-025", "digest", ticket + ":" + key, 24)
        br = value("TD-025", "builder", digest, 13)
        sr = value("TD-025", "server", digest, 13)
        final = value("TD-025", "final", br + ":" + sr, 20)
        build, server, probe = (record(root, name) for name in ("builder", "server", "probe"))
        if (build.get("compile_command") != "compile " + source or build.get("release") != "release " + digest
                or server.get("attach") != "attach " + artifact + " " + key or server.get("release") != "release " + digest
                or probe.get("test_command") != "test " + ticket + " " + nonce
                or probe.get("commit") != "commit " + br + " " + sr or not probe.get("coexist") or not probe.get("verified")
                or probe.get("builder_pid") != build.get("pid") or probe.get("server_pid") != server.get("pid")
                or server.get("builder_pid") != build.get("pid")
                or build.get("stage") != "closed" or server.get("stage") != "closed" or probe.get("stage") != "committed"):
            return False, "the ordered concurrent handoff was not completed"
        # Concurrency was verified and recorded by probe while both peers were alive.
        return answer(root, f"artifact={artifact}\nticket={ticket}\ndigest={digest}\nbuilder={br}\nserver={sr}\nfinal={final}\n")
    except (OSError, ValueError, KeyError) as error:
        return False, str(error)
