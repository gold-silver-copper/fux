#!/usr/bin/env python3
"""Record exact final-tree checks and focused probes (macOS, repository root)."""
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
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    if platform.system() != "Darwin":
        raise SystemExit("macOS-only verification contract")
    output = options.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    checks = [
        ("root-fmt", ["cargo", "fmt", "--all", "--check"], 0),
        ("root-clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"], 0),
        ("root-tests", ["cargo", "test", "--workspace", "--locked"], 0),
        ("root-build", ["cargo", "build", "--workspace", "--locked"], 0),
        ("harness-fmt", ["cargo", "fmt", "--manifest-path", "fux-fuzz/Cargo.toml", "--all", "--check"], 0),
        ("harness-clippy", ["cargo", "clippy", "--manifest-path", "fux-fuzz/Cargo.toml", "--all-targets", "--locked", "--", "-D", "warnings"], 0),
        ("harness-tests", ["cargo", "test", "--manifest-path", "fux-fuzz/Cargo.toml", "--locked"], 0),
        ("harness-build", ["cargo", "build", "--manifest-path", "fux-fuzz/Cargo.toml", "--locked"], 0),
        ("fuzz-fmt", ["cargo", "fmt", "--manifest-path", "fux-vt/fuzz/Cargo.toml", "--all", "--check"], 0),
        ("fuzz-build", ["cargo", "+nightly", "fuzz", "build", "terminal", "--fuzz-dir", "fux-vt/fuzz"], 0),
        ("dependency-tree", ["cargo", "tree", "--workspace", "--locked"], 0),
        ("source-audit", ["rg", "-n", "-i", "--hidden", "-g", "!target/**", "-g", "!**/target/**", "-g", "!*.md", "-g", "!**/fuzz/corpus/**", "-g", "!**/fuzz/artifacts/**", "vt100", "src", "fux-vt", "Cargo.toml", "Cargo.lock"], 1),
        ("root-test-audit", ["rg", "-n", "-i", "vt100", "tests"], 1),
        ("history-copy-oracle", ["cargo", "run", "--manifest-path", "fux-fuzz/Cargo.toml", "--example", "history_copy", "--locked"], 0),
        ("memory-plateau", ["cargo", "test", "-p", "fux-vt", "--lib", "measured_storage_plateau_and_transactional_resize_peak_include_metadata", "--locked", "--", "--nocapture"], 0),
        ("parser-performance", ["cargo", "test", "--release", "-p", "fux-vt", "measure_ascii_run_against_scalar_dispatch", "--locked", "--", "--ignored", "--nocapture"], 0),
        ("row-reuse-performance", ["cargo", "test", "--release", "--bin", "fux", "measure_alternating_width_and_history_row_reuse", "--locked", "--", "--ignored", "--nocapture"], 0),
    ]
    report = {"platform": platform.platform(), "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "results": []}
    for name, command, expected in checks:
        log = output / (name + ".log")
        timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
        started = time.monotonic()
        with log.open("wb") as stream:
            result = subprocess.run(command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT)
        success = result.returncode == expected and (expected != 1 or log.stat().st_size == 0)
        report["results"].append({"name": name, "command": command, "started_utc": timestamp,
                                  "seconds": round(time.monotonic() - started, 3), "exit": result.returncode,
                                  "expected_exit": expected, "pass": success, "log": str(log), "log_sha256": digest(log)})
        (output / "checks.json").write_text(json.dumps(report, indent=2) + "\n")
        print(f"{name}: {'PASS' if success else 'FAIL'} exit={result.returncode} seconds={report['results'][-1]['seconds']}", flush=True)
        if not success:
            raise SystemExit(1)
    report["binary_sha256"] = digest(ROOT / "target/debug/fux")
    report["harness_sha256"] = digest(ROOT / "fux-fuzz/target/debug/fux-fuzz")
    report["fuzz_binary_sha256"] = digest(ROOT / "fux-vt/fuzz/target/aarch64-apple-darwin/release/terminal")
    (output / "checks.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
