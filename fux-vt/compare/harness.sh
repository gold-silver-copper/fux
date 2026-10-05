#!/usr/bin/env bash
# The harness's commands, run by `run.sh COMMAND` once it has set up the
# environment and built fux-vt-compare:
#
#   quick           before a commit; at most a minute
#   full            before a PR; at most ten minutes
#   deep            before a release, or when hunting; prints its estimate
#   fuzz [MINUTES]  every fuzz target in turn, a share of MINUTES (10) each
#   scoreboard      the last runs' numbers, as JSON and Markdown
#
# Results, logs and the fuzz ledger go to $FUX_HARNESS_OUT (default
# fux-vt/compare/target/harness). Checks in a group run side by side; what
# times itself or counts against main runs alone. Each command ends with a
# one-screen summary, and exits 1 if any check failed.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${FUX_HARNESS_OUT:-$here/target/harness}
compare=$here/target/release/fux-vt-compare
bench=$root/bench/target/release/fux-bench
mkdir -p "$out/logs" "$out/status"

commit=$(git -C "$root" rev-parse --short HEAD)
if [[ -n $(git -C "$root" status --porcelain --untracked-files=no) ]]; then
  commit=$commit-dirty
fi

# step NAME COMMAND...: runs COMMAND with its output in logs/NAME.log, and
# leaves its exit status and seconds in status/NAME and a stamp in
# NAME.stamp for the scoreboard.
step() {
  local name=$1 start rc=0
  shift
  start=$(date +%s)
  "$@" >"$out/logs/$name.log" 2>&1 || rc=$?
  local seconds=$(($(date +%s) - start))
  echo "$rc $seconds" >"$out/status/$name"
  echo "$commit $(date -u +%Y-%m-%dT%H:%MZ) ${seconds}s exit $rc" >"$out/$name.stamp"
  printf '  %-14s %s %4ss\n' "$name" "$([[ $rc == 0 ]] && echo ok || echo FAIL)" "$seconds"
}

# together STEP...: each STEP ("name command args") side by side.
together() {
  local spec pids=()
  for spec in "$@"; do
    # shellcheck disable=SC2086 # each spec is words, split on purpose
    step $spec &
    pids+=($!)
  done
  wait "${pids[@]}"
}

ran=()
quick() {
  # The corpus and the named cases are judged by xterm and every engine
  # that runs here, not the in-process ones alone: xterm decides the fields
  # where the panel splits (a blank's colour), and xterm.js is the only
  # voter beside Ghostty that can tell synchronized output. Both still fit.
  # The corpus again with Ghostty judged as fux-vt is, fux-vt voting in its
  # place: both scores and where each departs alone (corpus-ghostty.json).
  # It runs beside the corpus, an xterm of its own, and still fits.
  # The oracle (diff/oracle.sh) holds fux-vt to what it did at the merge
  # base with main: every observable, after every step, over the corpus,
  # 10,000 random cases and 50 resize streams.
  # The input encoders beside libghostty-vt's (encoders.rs) take seconds.
  echo "quick: corpus beside xterm, with Ghostty judged too, transparency, 2,000 random cases, the named cases, the encoders, the oracle"
  ran+=(corpus corpus-ghostty transparency random cases encoders oracle)
  together \
    "corpus $compare corpus --json $out/corpus.json" \
    "corpus-ghostty $compare corpus --subject ghostty --json $out/corpus-ghostty.json" \
    "transparency $compare transparency --json $out/transparency.json" \
    "random $compare run --cases 2000" \
    "cases $compare cases" \
    "encoders $compare encoders" \
    "oracle $root/diff/oracle.sh"
}

build_bench() {
  cargo build --release --locked --quiet --manifest-path "$root/bench/Cargo.toml"
}

full() {
  quick
  # Ghostty's core runs esctest with fux-vt beside it (about a minute,
  # inside the group's time): the scoreboard's conformance against Ghostty.
  echo "full: 20,000 random cases with reflow and without, esctest, esctest of Ghostty's core"
  ran+=(random-wide random-no-reflow esctest esctest-ghostty)
  together \
    "random-wide $compare run --cases 20000" \
    "random-no-reflow $compare run --cases 20000 --no-reflow" \
    "esctest $compare esctest --json $out/esctest.json" \
    "esctest-ghostty $compare esctest --terminal ghostty --beside fux-vt --json $out/esctest-ghostty.json"
  echo "full: instructions against main (alone)"
  ran+=(against)
  build_bench
  # Nine repeats, not the bench's five: on a busy machine five left a
  # workload's spread above the 3% it judges at.
  step against "$bench" --against main --repeats 9 --json "$out/against.json"
  # Each engine's memory, and its instructions per byte beside Ghostty's:
  # every measure a child process of its own. About 5 s and 45-60 s.
  echo "full: memory and instructions per byte, every engine in process (alone)"
  ran+=(footprint instructions)
  step footprint "$compare" footprint --json "$out/footprint.json"
  step instructions "$compare" bench --instructions --repeats 5 --json "$out/instructions.json"
}

# What `deep` takes beyond `full`, from the last run of each part here if
# there was one, else from a first measure on an M-series Mac.
seeds=${FUX_DEEP_SEEDS:-20}
fuzz_minutes=10
last_seconds() {
  local name=$1 fallback=$2
  if [[ -f $out/status/$name ]]; then
    cut -d' ' -f2 "$out/status/$name"
  else
    echo "$fallback"
  fi
}

deep() {
  local verdicts esctest muxes feel info full_s
  verdicts=$(last_seconds verdicts-1 240)
  esctest=$(last_seconds esctest-in-fux 80)
  muxes=$(last_seconds multiplexers 500)
  feel=$(last_seconds feel 300)
  info=$(last_seconds info 20)
  full_s=480
  local estimate=$((full_s + seeds * verdicts + esctest + muxes + feel + info + fuzz_minutes * 60 + 60))
  echo "deep: about $((estimate / 60)) minutes (full, $seeds verdict seeds at ${verdicts}s, esctest in fux, tmux and zellij, feel, MB/s, $fuzz_minutes minutes of fuzzing)"
  full
  echo "deep: verdicts beside xterm, seeds 1-$seeds"
  local seed
  for seed in $(seq 1 "$seeds"); do
    ran+=("verdicts-$seed")
    step "verdicts-$seed" "$compare" verdicts --seed "$seed"
  done
  echo "deep: esctest in a fux pane; tmux and zellij; feel; MB/s (each alone)"
  ran+=(esctest-in-fux multiplexers feel info)
  step esctest-in-fux "$compare" esctest --in-fux --json "$out/esctest-in-fux.json"
  step multiplexers "$compare" transparency --multiplexers --json "$out/multiplexers.json"
  step feel "$bench" feel --json "$out/feel.json"
  step info "$bench" info --json "$out/info.json"
  fuzz "$fuzz_minutes"
}

# The fuzz targets: DIR TARGET [DICT] [ASAN_OPTIONS].
targets=(
  "fuzz protocol"
  "fux-vt/fuzz keys"
  "fuzz paint"
  "fuzz layout"
  "fuzz config fuzz/config.dict"
  "fuzz session fuzz/session.dict quarantine_size_mb=16"
  "fux-vt/fuzz terminal fux-vt/fuzz/terminal.dict"
  "fux-vt/fuzz graphemes"
  "fux-vt/fuzz cells"
  "diff/fuzz oracle fux-vt/fuzz/terminal.dict"
)

# fuzz MINUTES: each target for its share, from its stored corpus and what
# earlier runs here found (kept in $out/fuzz-corpus, so the stored corpus
# is left as it is). A crash is minimized; it and the minimized input are
# listed, to be made a test. A line per target goes to fuzz.jsonl.
fuzz() {
  local minutes=${1:-10} spec dir target dict asan share
  share=$((minutes * 60 / ${#targets[@]}))
  ((share > 0)) || share=1
  echo "fuzz: ${#targets[@]} targets, ${share}s each"
  (cd "$root" && cargo +nightly fuzz build --fuzz-dir fuzz -O -a >/dev/null 2>&1 &&
    cargo +nightly fuzz build --fuzz-dir fux-vt/fuzz -O -a >/dev/null 2>&1 &&
    cargo +nightly fuzz build --fuzz-dir diff/fuzz -O -a >/dev/null 2>&1) ||
    { echo "fuzz: the targets do not build (cargo +nightly fuzz build)"; return 1; }
  # The oracle's target starts from the oracle's own random cases, written
  # as its stored corpus (ignored by git), which reach far more than bytes.
  "$root/diff/oracle.sh" --seeds "$root/diff/fuzz/corpus/oracle" --cases 1000 --streams 5 >/dev/null ||
    { echo "fuzz: the oracle's seeds could not be written"; return 1; }
  for spec in "${targets[@]}"; do
    read -r dir target dict asan <<<"$spec"
    local found=$out/fuzz-corpus/$target stored=$root/$dir/corpus/$target
    local args=(-max_total_time="$share" -rss_limit_mb=2048 -timeout=25)
    [[ -n ${dict:-} ]] && args+=(-dict="$root/$dict")
    mkdir -p "$found"
    local corpora=("$found")
    [[ -d $stored ]] && corpora+=("$stored")
    ran+=("fuzz-$target")
    (cd "$root" && ASAN_OPTIONS=${asan:-} step "fuzz-$target" \
      cargo +nightly fuzz run "$target" --fuzz-dir "$dir" -O -a "${corpora[@]}" -- "${args[@]}")
    local rc seconds crash=""
    read -r rc seconds <"$out/status/fuzz-$target"
    if [[ $rc != 0 ]]; then
      crash=$(grep -o "$dir/artifacts/$target/[a-z-]*[0-9a-f]*" "$out/logs/fuzz-$target.log" | tail -1 || true)
      if [[ -n $crash ]]; then
        (cd "$root" && cargo +nightly fuzz tmin "$target" "$crash" --fuzz-dir "$dir" -O -a \
          >"$out/logs/fuzz-$target-tmin.log" 2>&1) || true
        echo "    crash: $crash (minimized: logs/fuzz-$target-tmin.log)"
      fi
    fi
    printf '{"commit":"%s","date":"%s","target":"%s","seconds":%s,"crashed":%s,"artifact":"%s"}\n' \
      "$commit" "$(date -u +%Y-%m-%dT%H:%MZ)" "$target" "$seconds" \
      "$([[ $rc == 0 ]] && echo false || echo true)" "$crash" >>"$out/fuzz.jsonl"
  done
  echo "fuzz: $minutes minutes"
}

# The scoreboard, kept in the repository too (scoreboard/): the page for
# this commit, and a line of history for it.
scoreboard() {
  "$compare" scoreboard "$out" --keep "$here/scoreboard" "$commit" "$(date -u +%Y-%m-%d)"
}

# The summary: each check that ran, and the command's time beside its budget.
summary() {
  local command=$1 budget=$2 started=$3 name rc seconds failed=0
  local took=$(($(date +%s) - started))
  echo
  echo "$command: $commit, ${took}s$([[ -n $budget ]] && echo " (budget ${budget}s)")"
  for name in "${ran[@]}"; do
    read -r rc seconds <"$out/status/$name"
    if [[ $rc != 0 ]]; then
      failed=$((failed + 1))
      echo "  FAIL $name (${seconds}s): $out/logs/$name.log"
      tail -3 "$out/logs/$name.log" | sed 's/^/       /'
    fi
  done
  echo "  ${#ran[@]} checks, $failed failed; results in $out"
  if [[ -n $budget ]] && ((took > budget)); then
    echo "  over budget: a check that does not fit moves to a deeper command"
  fi
  ((failed == 0))
}

command=${1:-}
shift || true
started=$(date +%s)
case $command in
  quick) quick; summary quick 60 "$started" ;;
  full) full; summary full 600 "$started" ;;
  deep) deep; summary deep "" "$started" ;;
  fuzz) fuzz "${1:-10}"; summary fuzz "" "$started" ;;
  scoreboard) scoreboard ;;
  *) echo "harness.sh: quick, full, deep, fuzz [MINUTES] or scoreboard" >&2; exit 2 ;;
esac
