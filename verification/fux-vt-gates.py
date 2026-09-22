#!/usr/bin/env python3
"""Reproducible final macOS harness gates; stop and preserve evidence on failure."""
import argparse
import datetime
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("group", choices=["scenarios", "traces", "stress", "smoke"])
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    if platform.system() != "Darwin":
        raise SystemExit("This verification contract is macOS-only")
    output = options.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    harness = ROOT / "fux-fuzz/target/debug/fux-fuzz"
    binary = ROOT / "target/debug/fux"
    cases = []
    if options.group == "scenarios":
        cases = [(name, ["--scenario", name, "--seed", "1", "--seconds", "3600"])
                 for name in ["terminal_edge", "adversarial", "selection", "history", "resize", "process", "stream"]]
    elif options.group == "traces":
        cases = [(path.stem, ["--replay", str(path), "--seconds", "3600"])
                 for path in sorted((ROOT / "fux-fuzz/traces").glob("*.json"))]
        assert cases, "no saved traces found"
    elif options.group == "stress":
        cases = [
            ("walk-100x20", ["--scenario", "walk", "--seed", "100", "--iterations", "20", "--seconds", "3600"]),
            ("walk-777x3000", ["--scenario", "walk", "--seed", "777", "--actions", "3000", "--seconds", "3600"]),
            ("raw-600x20", ["--scenario", "raw", "--seed", "600", "--iterations", "20", "--seconds", "3600"]),
            ("scene-fuzz-300x20", ["--scenario", "scene_fuzz", "--seed", "300", "--iterations", "20", "--seconds", "3600"]),
        ]
    else:
        cases = [("smoke", ["--seconds", "600"])]
    report = {
        "group": options.group,
        "platform": platform.platform(),
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "binary_sha256": digest(binary), "harness_sha256": digest(harness),
        "enumerated": [name for name, _ in cases], "results": [],
    }
    for name, arguments in cases:
        command = [str(harness), "--fux", str(binary), *arguments, "--output", str(output / name)]
        log = output / (name + ".log")
        started = time.monotonic()
        timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
        print(f"START {options.group}/{name}", flush=True)
        with log.open("wb") as stream:
            result = subprocess.run(command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT)
        report["results"].append({
            "name": name, "command": command, "started_utc": timestamp,
            "seconds": round(time.monotonic() - started, 3), "exit": result.returncode,
            "log": str(log), "log_sha256": digest(log),
        })
        (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
        print(f"END {name}: exit={result.returncode} seconds={report['results'][-1]['seconds']}", flush=True)
        if result.returncode:
            print("\n".join(log.read_text(errors="replace").splitlines()[-8:]), flush=True)
            raise SystemExit(result.returncode)
    print(f"PASS {options.group}: {len(cases)} invocations", flush=True)


if __name__ == "__main__":
    main()
