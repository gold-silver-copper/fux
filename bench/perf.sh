#!/bin/bash
# perf.sh PROGRAM [ARGS...]: counts the user-space instructions PROGRAM
# retires, for fux-bench's and fux-vt-compare's counters (`count.rs`).
#
# It runs `perf stat -v -x , --no-scale -e instructions:u` with the child
# pinned to one CPU set and without address randomization (setarch -R,
# where it works), and writes to stderr, after anything the child
# wrote there:
#
#   fux-perf: perf VERSION
#   fux-perf: cpus SET
#   fux-perf: aslr off|on
#   fux-perf: pmu NAME CPUS        (one line per PMU, from sysfs)
#   fux-perf: cpu N MODEL          (one line per CPU)
#   fux-perf: begin
#   ...perf's own output, unedited...
#   fux-perf: end
#
# It sums and checks nothing: `count.rs` reads perf's lines, checks each
# PMU's enabled and running times, and refuses a count that is not whole.
#
# On a machine with two core types (two PMUs), a child that moves between
# them is counted in part by each PMU, and perf scales each part up by its
# running share. So the child is pinned to the CPUs of one PMU: FUX_PERF_CPUS
# if set, else the PMU with the most CPUs. With one PMU nothing is pinned.
#
# perf is FUX_PERF if set (its ../lib is added to LD_LIBRARY_PATH, for perf
# extracted from a package), else `perf` on PATH.
set -u
perf=${FUX_PERF:-perf}
if [[ $perf == */* && -d ${perf%/*}/../lib ]]; then
  export LD_LIBRARY_PATH=${perf%/*}/../lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
fi
if [[ $# -eq 0 ]]; then
  echo "usage: perf.sh PROGRAM [ARGS...]" >&2
  exit 2
fi

# The PMUs that count for some CPUs only (a hybrid machine's core types).
pmus=() widest= most=0
for f in /sys/bus/event_source/devices/*/cpus; do
  [[ -r $f ]] || continue
  name=${f%/cpus} name=${name##*/} set=$(<"$f")
  pmus+=("$name $set")
  n=0
  IFS=, read -ra parts <<<"$set"
  for p in "${parts[@]}"; do
    if [[ $p == *-* ]]; then n=$((n + ${p#*-} - ${p%-*} + 1)); else n=$((n + 1)); fi
  done
  if ((n > most)); then most=$n widest=$set; fi
done
cpus=${FUX_PERF_CPUS:-}
if [[ -z $cpus && ${#pmus[@]} -gt 1 ]]; then cpus=$widest; fi

raw=$(mktemp "${TMPDIR:-/tmp}/fux-perf.XXXXXX") || exit 2
trap 'rm -f "$raw"' EXIT
pin=()
[[ -n $cpus ]] && pin=(taskset -c "$cpus")
# Without address randomization: the addresses a child sees (and so the
# work of formatting and parsing them, as /proc's files hold them) differ
# in every process with it, by 0.3% of `fux-bench run ready/probe`.
aslr=on
if setarch "$(uname -m)" -R true 2>/dev/null; then
  pin+=(setarch "$(uname -m)" -R)
  aslr=off
fi
"${pin[@]}" "$perf" stat -v -x , --no-scale -e instructions:u -o "$raw" -- "$@"
rc=$?
{
  echo "fux-perf: $("$perf" --version 2>/dev/null | tr -d '\n')"
  echo "fux-perf: cpus ${cpus:-all}"
  echo "fux-perf: aslr $aslr"
  for p in "${pmus[@]}"; do echo "fux-perf: pmu $p"; done
  for c in $(ls -d /sys/devices/system/cpu/cpu[0-9]* | sort -V); do
    model=$(tr '\0' ' ' 2>/dev/null <"$c/of_node/compatible")
    model=${model% }
    echo "fux-perf: cpu ${c##*cpu} ${model:-unknown}"
  done
  echo "fux-perf: begin"
  cat "$raw"
  echo "fux-perf: end"
} >&2
exit "$rc"
