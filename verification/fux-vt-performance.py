#!/usr/bin/env python3
"""Paired macOS scale/stream measurements; preserve every run, never trim outliers."""
import argparse
import datetime
import hashlib
import json
from pathlib import Path
import platform
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    if platform.system() != "Darwin":
        raise SystemExit("macOS-only measurement contract")
    output = options.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    harness = ROOT / "fux-fuzz/target/debug/fux-fuzz"
    binaries = {"before": options.baseline.resolve(), "after": ROOT / "target/debug/fux"}
    hashes = {name: digest(path) for name, path in binaries.items()}
    harness_hash = digest(harness)
    report = {
        "platform": platform.platform(), "profile": "debug", "seed": 1,
        "baseline_commit": "9140af1", "after_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "binary_sha256": hashes, "harness_sha256": harness_hash,
        "warmups_per_scenario_version": 1, "measurements_per_scenario_version": 5,
        "results": [],
    }
    destination = output / "performance.json"

    def save():
        destination.write_text(json.dumps(report, indent=2) + "\n")

    for scenario in ["scale", "stream"]:
        for run in range(6):
            # Alternate order so the measured pairs do not always favour one
            # version's cache/thermal position. Index zero is a warm-up only.
            for version in (["before", "after"] if run % 2 == 0 else ["after", "before"]):
                assert digest(harness) == harness_hash, "harness changed during comparison"
                assert digest(binaries[version]) == hashes[version], "binary changed during comparison"
                name = f"{scenario}-{version}-{run}"
                command = [str(harness), "--fux", str(binaries[version]), "--scenario", scenario,
                           "--seed", "1", "--seconds", "3600", "--output", str(output / name)]
                log = output / (name + ".log")
                print("START " + name, flush=True)
                timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
                started = time.monotonic()
                with log.open("wb") as stream:
                    process = subprocess.run(command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT)
                record = {"scenario": scenario, "version": version, "run": run, "warmup": run == 0,
                          "command": command, "started_utc": timestamp, "seconds": round(time.monotonic() - started, 3),
                          "exit": process.returncode, "log": str(log), "log_sha256": digest(log)}
                marker = scenario.upper() + "-TIMES "
                timings = [json.loads(line.removeprefix(marker)) for line in log.read_text(errors="replace").splitlines() if line.startswith(marker)]
                if timings:
                    assert len(timings) == 1, "unexpected multiple cases"
                    record["timings"] = timings[0]
                report["results"].append(record)
                save()
                print(f"END {name}: exit={process.returncode} seconds={record['seconds']}", flush=True)
                if process.returncode:
                    raise SystemExit(process.returncode)
                assert timings, "successful scenario omitted timing evidence"
    summary = {}
    labels = [row["label"] for row in report["results"][0]["timings"]]
    for label in labels:
        summary[label] = {}
        for version in binaries:
            values = [next(row["ms"] for row in result["timings"] if row["label"] == label)
                      for result in report["results"] if result["scenario"] == "scale" and result["version"] == version and not result["warmup"]]
            assert len(values) == 5
            summary[label][version] = {"raw_ms": values, "median_ms": statistics.median(values), "range_ms": [min(values), max(values)]}
        summary[label]["delta_ms"] = summary[label]["after"]["median_ms"] - summary[label]["before"]["median_ms"]
    assert "with 201 panes" in summary and "move to new tab #1000" in summary
    report["scale_summary"] = summary
    report["median_regressions"] = [label for label, values in summary.items() if values["delta_ms"] > 0]
    streams = {}
    for version in binaries:
        measured = [r["timings"] for r in report["results"] if r["scenario"] == "stream" and r["version"] == version and not r["warmup"]]
        assert len(measured) == 5 and all(r["final_frame_equal"] for r in measured)
        streams[version] = {key: {"raw_ms": [r[key] for r in measured], "median_ms": statistics.median(r[key] for r in measured),
                                  "range_ms": [min(r[key] for r in measured), max(r[key] for r in measured)]}
                            for key in ["catch_up_ms", "advancing_ms", "convergence_ms"]}
    report["stream_summary"] = streams
    save()
    if report["median_regressions"]:
        print("INVESTIGATE (rerun both versions for noise): " + repr(report["median_regressions"]), flush=True)
        raise SystemExit(1)
    print("PASS: all measured scale medians non-regressing; every stream converged", flush=True)


if __name__ == "__main__":
    main()
