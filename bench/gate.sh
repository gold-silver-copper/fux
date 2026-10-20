#!/usr/bin/env bash
# bench/gate.sh [REF]: the speed gate. The working tree against REF
# (default: the latest master), in instructions, judged by this machine's
# floors: `fux-bench --against REF --floors FLOORS`. Run it on a quiet
# machine; it refuses one whose CPUs may change speed under it.
#
#   FUX_GATE_FLOORS  this machine's calibration (default
#                    bench/target/fux-bench/floors-HOST.json); made first,
#                    by `fux-bench --calibrate`, if it is not there
#   FUX_PERF_CPUS    the CPUs to count on (bench/perf.sh): one core type,
#                    and best ones nothing else is given
#   FUX_GATE_LOCK    a file to hold a lock on (flock), so that two gates
#                    on one machine never measure at once
#   FUX_GATE_MINUTES how long the measurement may take (default 10)
#
# The JSON goes to bench/target/fux-bench/gate.json, perf's reports beside
# it. Exit as fux-bench: 0 nothing slower, 1 slower, 2 a broken floor or an
# error (a side that does not build: the output says which); 5 a CPU whose
# governor is not `performance`; 124 past the time allowed.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/bench/target/fux-bench
floors=${FUX_GATE_FLOORS:-$out/floors-$(hostname -s).json}
mkdir -p "$out"

governors=(/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor)
if [[ -r ${governors[0]} ]] && grep -qv '^performance$' "${governors[@]}"; then
  echo "gate: a CPU's governor is not 'performance' ($(sort -u "${governors[@]}" | tr '\n' ' '))" >&2
  exit 5
fi

if [[ -n ${FUX_GATE_LOCK:-} ]]; then
  exec 9>"$FUX_GATE_LOCK"
  flock -n 9 || { echo "gate: waiting for $FUX_GATE_LOCK" >&2; flock 9; }
fi

ref=${1:-}
if [[ -z $ref ]]; then
  git -C "$root" fetch --quiet origin master || echo "gate: master not fetched; the last fetched is used" >&2
  ref=$(git -C "$root" rev-parse --verify "origin/master^{commit}")
fi
cargo build --release --locked --quiet --manifest-path "$root/bench/Cargo.toml"
bench=${CARGO_TARGET_DIR:-$root/bench/target}/release/fux-bench
limit=()
command -v timeout >/dev/null && limit=(timeout "$((${FUX_GATE_MINUTES:-10} * 60))")

cd "$root"
if [[ ! -f $floors ]]; then
  echo "gate: calibrating this machine first, into $floors" >&2
  ${limit[@]+"${limit[@]}"} "$bench" --calibrate --json "$floors"
fi
${limit[@]+"${limit[@]}"} "$bench" --against "$ref" --floors "$floors" --json "$out/gate.json"
