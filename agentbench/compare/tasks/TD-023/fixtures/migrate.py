import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import threading
from terminal_io import Terminal, emit, value


def main():
    with Terminal("migrate", sys.argv[1]) as t:
        secret = (t.work / "access.txt").read_text().rstrip("\n")
        token = value("TD-023", "token", secret, 13)
        status = str(11 + int(value("TD-023", "status", secret, 6), 16) % 70)
        receipt = value("TD-023", "receipt", token + ":" + status, 18)
        emit("migration-client: loading journal\nREPLAY: Password for staging: accepted lease=OLD\nEND REPLAY\n")
        t.quiet(4.6, "journal load")
        emit("LIVE Password for staging (access.txt, Enter; no echo): ")
        submitted = t.line(echo=False)
        if submitted != secret:
            t.fail("password mismatch")
        t.save(password_ok=True)
        thread = threading.Thread(target=lambda: emit("accepted lease=" + value("TD-023", "lease", secret, 9) + "\n"))
        thread.start()
        thread.join()
        emit("PREVIEW ONLY: Enter ack <token> <status>; window is CLOSED\n")
        t.quiet(3.3, "migration index pass")
        emit("\aINDEX settled token=" + token + "\n")
        t.quiet(5.8, "migration stop")
        emit("MIGRATION stopped status=" + status + "\nACK WINDOW OPEN. Enter ack <token> <status>: ")
        ack = t.line()
        t.save(ack=ack)
        if ack != "ack " + token + " " + status:
            t.fail("acknowledgement mismatch")
        emit("You said: " + ack + "\n")
        t.save(ok=True, stage="confirmed")
        emit("CONFIRMED receipt=" + receipt + "\n")


if __name__ == "__main__":
    main()
