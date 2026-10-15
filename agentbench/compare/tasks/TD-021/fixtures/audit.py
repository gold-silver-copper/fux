import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import threading
from terminal_io import Terminal, emit, value


def main():
    with Terminal("audit", sys.argv[1]) as t:
        source = (t.work / "request.txt").read_text().rstrip("\n")
        tag = value("TD-021", "tag", source, 10)
        digest = value("TD-021", "digest", source, 24)
        seal = value("TD-021", "seal", source, 14)
        stale = value("TD-021", "replay", source, 10)
        emit("audit-client: restoring last session\nREPLAY ONLY\nYou said: " + source + " | tag=" + stale + "\nEND REPLAY\n")
        t.quiet(3.7, "startup")
        emit("LIVE FILTER: type the single line in request.txt, then Enter: ")
        submitted = t.line()
        t.save(submitted=submitted)
        if submitted != source:
            t.fail("filter did not match request.txt")
        reply = threading.Thread(target=lambda: emit("You said: " + submitted + " | tag=" + tag + "\n"))
        reply.start()
        reply.join()
        emit("Compiling audit; input is locked until exit.\n")
        t.quiet(5.3, "report compilation")
        for i in range(140):
            emit("HISTORY row=%04d tag=%s digest=%s\n" % (i, stale, value("TD-021", str(i), source, 24)))
        emit("\x1b[36mCURRENT tag=" + tag + " label=" + ("warehouse/" * 32) + " digest=" + digest + "\x1b[0m\n")
        for i in range(540):
            emit("AUDIT DETAIL %04d historical checksum=%s\n" % (i, value("TD-021", "detail" + str(i), source, 20)))
        emit("QUOTED EXAMPLE (not a result): You said: " + source + " | tag=" + stale + "\n")
        t.save(ok=True, stage="complete", tag=tag, digest=digest, seal=seal)
        emit("\aREPORT END seal=" + seal + "\n")


if __name__ == "__main__":
    main()
