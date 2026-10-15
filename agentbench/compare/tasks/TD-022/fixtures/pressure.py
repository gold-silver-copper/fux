import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from terminal_io import Terminal, emit, value


def main():
    with Terminal("pressure", sys.argv[1]) as t:
        source = (t.work / "sensor.txt").read_text().strip()
        ticket = value("TD-022", "ticket", source, 11)
        peak = str(1000 + int(value("TD-022", "peak", source, 8), 16) % 8000) + ".125"
        seal = value("TD-022", "seal", source, 16)
        emit("pressure-watch: calibrating; retain status transitions, not just the final screen\n")
        t.quiet(2.9, "sensor warmup")
        emit("\r\x1b[2KSNAPSHOT rejected ticket=" + value("TD-022", "preview", source, 11) + " peak=9999.999\r\x1b[2Kcalibration | idle=0000.000")
        t.quiet(3.8, "calibration")
        # The accepted sample must survive observation as emitted bytes, not as a final screen row.
        emit("\r\x1b[2K\aSNAPSHOT accepted ticket=" + ticket + " peak=" + peak + "\r\x1b[2K\x1b[1Gdisplay | idle=0000.000")
        t.quiet(1.9, "render drain")
        emit("\r\x1b[2KSNAPSHOT rejected ticket=" + value("TD-022", "trailer", source, 11) + " peak=0000.001\r\x1b[2Kdisplay | idle=0000.000\n")
        t.save(ok=True, stage="complete")
        emit("RENDER COMPLETE seal=" + seal)


if __name__ == "__main__":
    main()
