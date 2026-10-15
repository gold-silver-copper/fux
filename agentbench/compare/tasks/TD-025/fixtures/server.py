import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import threading
from domain import identifiers
from terminal_io import Terminal, alive, emit, read_record


def main():
    with Terminal("server", sys.argv[1]) as t:
        target, artifact, key, nonce, ticket, digest, br, sr, final = identifiers(t.work)
        emit("spool-server: opening store\nPREVIEW attach OLD OLD -> bound ticket=OLD\n")
        t.quiet(6.4, "store startup")
        emit("LIVE SERVER READY nonce=" + nonce + ". Enter attach <artifact> <key> after BUILT: ")
        submitted = t.line()
        build = read_record(t.work, "builder")
        t.save(attach=submitted)
        if submitted != "attach " + artifact + " " + key or build.get("stage") != "built" or not alive(build):
            t.fail("attach requires a live completed build")
        emit(submitted + " -> bound ticket=" + ticket + "\nBinding; no input until SERVING.\n")
        t.quiet(4.7, "server binding")
        t.save(stage="serving", ticket=ticket, nonce=nonce, builder_pid=build["pid"])
        thread = threading.Thread(target=lambda: emit("SERVING ticket=" + ticket + "\nAfter VERIFIED, enter release <digest>: "))
        thread.start()
        thread.join()
        release = t.line()
        verification = read_record(t.work, "probe")
        if (release != "release " + digest or not verification.get("verified")
                or verification.get("server_pid") != t.record["pid"]):
            t.fail("release requires the live VERIFIED digest")
        t.save(ok=True, stage="closed", release=release)
        emit("SERVER CLOSED receipt=" + sr + "\n")


if __name__ == "__main__":
    main()
