import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from domain import identifiers
from terminal_io import Terminal, alive, emit, read_record


def main():
    with Terminal("probe", sys.argv[1]) as t:
        target, artifact, key, nonce, ticket, digest, br, sr, final = identifiers(t.work)
        emit("spool-probe: warming assertions\nQUOTED EXAMPLE: VERIFIED digest=EXAMPLE; COMMITTED receipt=EXAMPLE\n")
        t.quiet(3.6, "probe startup")
        emit("LIVE PROBE: after SERVING, enter test <ticket> <nonce>: ")
        submitted = t.line()
        build = read_record(t.work, "builder")
        server = read_record(t.work, "server")
        ttys = [build.get("tty"), server.get("tty"), t.record["tty"]]
        if (submitted != "test " + ticket + " " + nonce or build.get("stage") != "built"
                or server.get("stage") != "serving" or not alive(build) or not alive(server)
                or None in ttys or len(set(ttys)) != 3):
            t.fail("test requires the live build and serving server in distinct terminals")
        t.save(test_command=submitted, coexist=True, builder_pid=build["pid"], server_pid=server["pid"])
        emit("test accepted; input locked while checking\n")
        t.quiet(4.9, "probe verification")
        # Recheck liveness while all terminal identities are meaningful.
        if not alive(build) or not alive(server):
            t.fail("builder or server exited before verification")
        t.save(stage="verified", verified=True)
        emit("\aVERIFIED digest=" + digest + "\nAfter both CLOSED replies, enter commit <builder-receipt> <server-receipt>: ")
        submitted = t.line()
        build = read_record(t.work, "builder")
        server = read_record(t.work, "server")
        if (submitted != "commit " + br + " " + sr or not build.get("ok") or not server.get("ok")
                or build.get("stage") != "closed" or server.get("stage") != "closed"
                or build.get("pid") != t.record["builder_pid"] or server.get("pid") != t.record["server_pid"]):
            t.fail("commit requires both closure receipts")
        emit("You said: " + submitted + "\n")
        t.save(ok=True, stage="committed", commit=submitted)
        emit("COMMITTED receipt=" + final + "\n")


if __name__ == "__main__":
    main()
