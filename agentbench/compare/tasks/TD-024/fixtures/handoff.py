import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import threading
from terminal_io import Terminal, emit, value


def main():
    with Terminal("handoff", sys.argv[1]) as t:
        expected = (t.work / "draft.txt").read_text().splitlines()
        source = "\n".join(expected) + "\n"
        checksum = value("TD-024", "checksum", source, 20)
        token = value("TD-024", "token", source, 12)
        challenge = value("TD-024", "challenge", source, 15)
        receipt = value("TD-024", "receipt", challenge + ":" + token, 22)
        emit("handoff-editor: opening draft buffer\n")
        t.quiet(2.8, "editor startup")
        emit("DRAFT INPUT OPEN: enter draft.txt lines, then .END on its own line.\n")
        received = []
        while True:
            line = t.line()
            if line == ".END":
                break
            received.append(line)
        t.save(lines=received)
        if received != expected:
            t.fail("draft differs from draft.txt")
        emit("You said: .END\nDRAFT stored checksum=" + checksum + "\nEditor locked while indexing.\n")
        t.quiet(6.1, "draft indexing")
        emit("\r\x1b[2K\aARMED token=" + token + "\r\x1b[2KPress F2 to seal (no Enter). ")
        t.expect_key("F2")
        t.save(key="F2")
        thread = threading.Thread(target=lambda: emit("\nSEAL challenge=" + challenge + ". Enter seal <challenge> <token>: "))
        thread.start()
        thread.join()
        submitted = t.line()
        t.save(seal_command=submitted)
        if submitted != "seal " + challenge + " " + token:
            t.fail("seal command mismatch")
        emit("You said: " + submitted + "\n")
        t.save(ok=True, stage="sealed")
        emit("SEALED receipt=" + receipt + "\n")


if __name__ == "__main__":
    main()
