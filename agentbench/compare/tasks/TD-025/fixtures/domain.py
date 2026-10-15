import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from terminal_io import value


def identifiers(work):
    source = (work / "manifest.txt").read_text().strip()
    artifact = value("TD-025", "artifact", source, 14)
    key = value("TD-025", "key", source, 12)
    nonce = value("TD-025", "nonce", source, 11)
    ticket = value("TD-025", "ticket", artifact + ":" + nonce, 15)
    digest = value("TD-025", "digest", ticket + ":" + key, 24)
    builder = value("TD-025", "builder", digest, 13)
    server = value("TD-025", "server", digest, 13)
    final = value("TD-025", "final", builder + ":" + server, 20)
    return source, artifact, key, nonce, ticket, digest, builder, server, final


if __name__ == "__main__":
    raise SystemExit("Support module only; launch the program named in the goal in a terminal")
