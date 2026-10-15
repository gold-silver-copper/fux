import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from domain import identifiers
from terminal_io import Terminal, emit, read_record


def main():
    with Terminal("builder", sys.argv[1]) as t:
        target, artifact, key, nonce, ticket, digest, br, sr, final = identifiers(t.work)
        emit("build-runner: loading toolchain\nREPLAY BUILT artifact=OLD key=OLD (not current)\n")
        t.quiet(2.4, "toolchain startup")
        emit("LIVE BUILD: enter compile <target>, target is in manifest.txt: ")
        submitted = t.line()
        t.save(compile_command=submitted)
        if submitted != "compile " + target:
            t.fail("compile target mismatch")
        emit("Compiling; no input until the release prompt.\n")
        t.quiet(7.2, "compilation")
        t.save(stage="built", artifact=artifact, key=key)
        emit("\r\x1b[2KBUILT artifact=" + artifact + " key=" + key + "\r\x1b[2KBuild idle. After VERIFIED, enter release <digest>: ")
        release = t.line()
        verification = read_record(t.work, "probe")
        if (release != "release " + digest or not verification.get("verified")
                or verification.get("builder_pid") != t.record["pid"]):
            t.fail("release requires the live VERIFIED digest")
        t.save(ok=True, stage="closed", release=release)
        emit("BUILDER CLOSED receipt=" + br + "\n")


if __name__ == "__main__":
    main()
