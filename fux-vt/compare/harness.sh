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
# quick, full, deep and fuzz take:
#
#   --jobs N        at most N checks at once, a check that runs threads of
#                   its own alone with N of them, and N cargo jobs; without
#                   it, a group's checks all run at once, each with every core
#   --against REF   what the oracle and the instruction counts compare the
#                   working tree with (default: the latest master); a run
#                   whose results are kept names a fixed commit
#
# Results, logs and the fuzz ledger go to $FUX_HARNESS_OUT (default
# fux-vt/compare/target/harness). Checks in a group run side by side; what
# times itself or counts against REF runs alone. Each command ends with a
# one-screen summary, and exits 1 if any check failed.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${FUX_HARNESS_OUT:-$here/target/harness}
# Where cargo builds them: CARGO_TARGET_DIR if set, as run.sh builds.
compare=${CARGO_TARGET_DIR:-$here/target}/release/fux-vt-compare
bench=${CARGO_TARGET_DIR:-$root/bench/target}/release/fux-bench
# The perf the instruction counts use (bench/src/count.rs): the one on
# PATH, else the extracted package this machine keeps, if any.
if [[ -z ${FUX_PERF:-} ]] && ! command -v perf >/dev/null; then
  for p in "$HOME/.cache/claude-tmp/perfpkg/x/usr/bin/perf"; do
    [[ -x $p ]] && export FUX_PERF=$p
  done
fi
# The A/A floors `fux-bench --against` judges each workload by: the
# calibration FUX_BENCH_FLOORS names, else the one committed beside this
# script (floors.json), else the bench's own.
floors=${FUX_BENCH_FLOORS:-}
[[ -z $floors && -f $here/floors.json ]] && floors=$here/floors.json
mkdir -p "$out/logs" "$out/status"

# The last commit that changed the paths given (along the first parents,
# so a merge is the commit its branch's changes came in), with -dirty if
# any of them differ from it (untracked files too): a commit that changes
# only the harness leaves the product's the same.
revision() {
  local last
  last=$(git -C "$root" log -1 --first-parent --format=%H -- "$@")
  if [[ -n $(git -C "$root" status --porcelain -- "$@") ]]; then
    last=$last-dirty
  fi
  echo "$last"
}
# The product measured: fux-vt and the fux it runs in, unless
# FUX_PRODUCT_COMMIT names another (a run of this harness against another
# commit's fux-vt, built by a path override, says which). The harness: the
# tools that measure it.
product=${FUX_PRODUCT_COMMIT:-$(revision fux-vt ':(exclude)fux-vt/compare' ':(exclude)fux-vt/fuzz' \
  ':(exclude)fux-vt/gen' src fuxix fuxth Cargo.toml Cargo.lock)}
harness=$(revision fux-vt/compare bench diff fuzz fux-vt/fuzz references/fetch.sh)
commit=$(git -C "$root" rev-parse --short HEAD)
[[ $product == *-dirty || $harness == *-dirty ]] && commit=$commit-dirty

# A JSON string.
quoted() {
  local s=${1//\\/\\\\}
  s=${s//\"/\\\"}
  printf '"%s"' "${s//$'\t'/\\t}"
}

# step NAME COMMAND...: runs COMMAND with its output in logs/NAME.log, and
# leaves its exit status and seconds in status/NAME, and its stamp in
# NAME.stamp.json for the scoreboard: the product and harness commits, the
# binary's SHA-256, the exact command, when, how long, and its exit status.
# What an earlier run left (NAME.json, the stamp) is removed first, so no
# result outlives the run that made it.
step() {
  local name=$1 start rc=0 binary=$2 sha=""
  shift
  rm -f "$out/$name.json" "$out/$name.stamp.json"
  [[ -f $binary ]] && sha=$(shasum -a 256 "$binary" | cut -d' ' -f1)
  start=$(date +%s)
  "$@" >"$out/logs/$name.log" 2>&1 || rc=$?
  local seconds=$(($(date +%s) - start))
  echo "$rc $seconds" >"$out/status/$name"
  printf '{"check":%s,"product":%s,"harness":%s,"binary":%s,"binary_sha256":%s,"command":%s,"date":%s,"seconds":%s,"exit":%s}\n' \
    "$(quoted "$name")" "$(quoted "$product")" "$(quoted "$harness")" "$(quoted "$binary")" \
    "$(quoted "$sha")" "$(quoted "$(printf '%q ' "$@")")" "$(quoted "$(date -u +%Y-%m-%dT%H:%M:%SZ)")" \
    "$seconds" "$rc" >"$out/$name.stamp.json"
  printf '  %-14s %s %4ss\n' "$name" "$([[ $rc == 0 ]] && echo ok || echo FAIL)" "$seconds"
}

# together STEP...: each STEP ("name command args") side by side. With
# --jobs N, at most N at a time, and a STEP marked "+" (one that runs N
# threads itself) alone.
together() {
  local spec pids=()
  for spec in "$@"; do
    if [[ -n $jobs ]] && { [[ $spec == +* ]] || ((${#pids[@]} >= jobs)); }; then
      wait "${pids[@]}"
      pids=()
    fi
    if [[ -n $jobs && $spec == +* ]]; then
      # shellcheck disable=SC2086 # each spec is words, split on purpose
      step ${spec#+}
      continue
    fi
    # shellcheck disable=SC2086
    step ${spec#+} &
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
  # The oracle (diff/oracle.sh) holds fux-vt to what it does at the latest
  # master: every observable, after every step, over the corpus,
  # 10,000 random cases and 50 resize streams.
  # The input encoders beside libghostty-vt's (encoders.rs) take seconds.
  echo "quick: corpus beside xterm, with Ghostty judged too, transparency, 2,000 random cases, the named cases, the encoders, the lists and their diff, the oracle"
  # Every judged list, as JSON (lists.json), and the list-diff report:
  # each change since checkpoint 0's lists (lists-c0.json) beside its
  # citation in list-changes.txt.
  ran+=(corpus corpus-ghostty transparency random cases encoders lists lists-diff oracle)
  together \
    "corpus $compare corpus --json $out/corpus.json" \
    "corpus-ghostty $compare corpus --subject ghostty --json $out/corpus-ghostty.json" \
    "transparency $compare transparency --json $out/transparency.json" \
    "random $compare run --cases 2000" \
    "cases $compare cases --json $out/cases.json" \
    "encoders $compare encoders" \
    "lists $compare lists --json $out/lists.json" \
    "lists-diff $compare lists --against $here/lists-c0.json --esctest $out" \
    "+oracle $root/diff/oracle.sh ${against:-} ${jobs:+--threads $jobs}"
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
    "+esctest $compare esctest ${jobs:+--jobs $jobs} --json $out/esctest.json" \
    "+esctest-ghostty $compare esctest ${jobs:+--jobs $jobs} --terminal ghostty --beside fux-vt --json $out/esctest-ghostty.json"
  echo "full: instructions against ${against:-master} (alone)"
  ran+=(against)
  build_bench
  # Nine repeats, not the bench's five: on a busy machine five left a
  # workload's spread above the 3% it judges at.
  step against "$bench" --against "${against:-master}" ${jobs:+--jobs "$jobs"} ${floors:+--floors "$floors"} --repeats 9 \
    --json "$out/against.json"
  # Each engine's memory, and its instructions per byte beside Ghostty's:
  # every measure a child process of its own. About 5 s and 45-60 s.
  echo "full: memory and instructions per byte, every engine in process (alone)"
  ran+=(footprint instructions)
  step footprint "$compare" footprint ${jobs:+--jobs "$jobs"} --json "$out/footprint.json"
  step instructions "$compare" bench --instructions ${jobs:+--jobs "$jobs"} --repeats 5 \
    --json "$out/instructions.json"
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
  # In a fux pane, in xterm, and on each engine (measured at 4 jobs).
  esctest=0
  local part
  for part in in-fux:110 xterm:140 tmux:110 alacritty:40 libvterm:45 avt:200 wezterm:50 vt100:105; do
    esctest=$((esctest + $(last_seconds "esctest-${part%:*}" "${part#*:}")))
  done
  muxes=$(last_seconds multiplexers 500)
  feel=$(last_seconds feel 300)
  info=$(last_seconds info 20)
  full_s=480
  # The ledger's subjects: six named-case runs and five corpus runs.
  local subjects=150
  local today_s
  today_s=$(last_seconds random-wide 60)
  local estimate=$((full_s + today_s + seeds * verdicts + esctest + subjects + muxes + feel + info + fuzz_minutes * 60 + 60))
  echo "deep: about $((estimate / 60)) minutes (full, 20,000 random cases at the day's seed, $seeds verdict seeds at ${verdicts}s, esctest in fux, in xterm and on each engine, the cases and corpus with each engine the subject and the ledger, tmux and zellij, feel, MB/s, $fuzz_minutes minutes of fuzzing)"
  full
  echo "deep: verdicts beside xterm, seeds 1-$seeds"
  local seed
  for seed in $(seq 1 "$seeds"); do
    ran+=("verdicts-$seed")
    step "verdicts-$seed" "$compare" verdicts --seed "$seed"
  done
  # Seed 1 is the gate, in quick and full; deep explores beyond it with a
  # seed that changes each day, printed, so a failure found is replayed by
  # its seed.
  local today
  today=$(date -u +%Y%m%d)
  echo "deep: 20,000 random cases at today's seed, $today"
  ran+=("random-$today")
  step "random-$today" "$compare" run --seed "$today" --cases 20000
  echo "deep: esctest in a fux pane, in real xterm and on each engine; tmux and zellij; feel; MB/s (each alone)"
  ran+=(esctest-in-fux esctest-xterm multiplexers feel info)
  step esctest-in-fux "$compare" esctest --in-fux ${jobs:+--jobs "$jobs"} --json "$out/esctest-in-fux.json"
  step esctest-xterm "$compare" esctest --xterm ${jobs:+--jobs "$jobs"} --json "$out/esctest-xterm.json"
  # Each engine on the one denominator, scored, with no list of its own.
  local engine
  # tmux runs as its own process: esctest runs inside a real one.
  ran+=(esctest-tmux)
  step esctest-tmux "$compare" esctest --tmux ${jobs:+--jobs "$jobs"} --json "$out/esctest-tmux.json"
  for engine in alacritty libvterm avt wezterm vt100; do
    ran+=("esctest-$engine")
    step "esctest-$engine" "$compare" esctest --terminal "$engine" --unlisted ${jobs:+--jobs "$jobs"} \
      --json "$out/esctest-$engine.json"
  done
  # The named cases and the corpus with each engine the subject, for the
  # conformance ledger, then its scores over every result here.
  for engine in ghostty alacritty libvterm avt wezterm vt100; do
    ran+=("cases-subject-$engine")
    step "cases-subject-$engine" "$compare" cases --subject "$engine" --json "$out/cases-subject-$engine.json"
  done
  for engine in alacritty libvterm avt wezterm vt100; do
    ran+=("corpus-subject-$engine")
    step "corpus-subject-$engine" "$compare" corpus --subject "$engine" --json "$out/corpus-subject-$engine.json"
  done
  ran+=(ledger)
  step ledger "$compare" ledger --results "$out" --json "$out/ledger-scores.json" --table "$out/ledger.md"
  step multiplexers "$compare" transparency --multiplexers --json "$out/multiplexers.json"
  step feel "$bench" feel --json "$out/feel.json"
  step info "$bench" info --json "$out/info.json"
  fuzz "$fuzz_minutes"
}

# The fuzz directories, each built and run on its own, so that one that
# does not build is named and the others still run. Their targets are what
# `cargo fuzz list` says, so a new one is never left out.
fuzz_dirs=(fuzz fux-vt/fuzz diff/fuzz)

# What a target takes beyond its corpus: TARGET DICT [ASAN_OPTIONS].
fuzz_extras=(
  "config fuzz/config.dict"
  "session fuzz/session.dict quarantine_size_mb=16"
  "fuxth_tokens fuzz/fuxth.dict"
  "fuxth_run fuzz/fuxth.dict"
  "fuxth_pause fuzz/fuxth.dict"
  "terminal fux-vt/fuzz/terminal.dict"
  "deep fux-vt/fuzz/terminal.dict"
  "oracle fux-vt/fuzz/terminal.dict"
)

# fuzz MINUTES: each target for its share, from its stored corpus and what
# earlier runs here found (kept in $out/fuzz-corpus, so the stored corpus
# is left as it is). A crash is minimized; it and the minimized input are
# listed, to be made a test. A line per target goes to fuzz.jsonl.
#
# diff/fuzz's target is the oracle, which builds against base-vt's pin in
# diff/oracle/Cargo.toml: it is pinned first to the ref the oracle check
# uses (diff/oracle.sh --pin), and the pin and Cargo.lock are put back
# when the command ends.
fuzz() {
  local minutes=${1:-10} dir name target extra dict asan share built=() broken=() targets=()
  local pin=$out/oracle-pin
  mkdir -p "$pin"
  cp "$root/diff/oracle/Cargo.toml" "$root/diff/Cargo.lock" "$pin/"
  # shellcheck disable=SC2064 # the paths are fixed now
  trap "cp $(printf %q "$pin/Cargo.toml") $(printf %q "$root/diff/oracle/Cargo.toml"); \
    cp $(printf %q "$pin/Cargo.lock") $(printf %q "$root/diff/Cargo.lock")" EXIT
  ran+=(fuzz-oracle-pin)
  step fuzz-oracle-pin "$root/diff/oracle.sh" --pin ${against:+"$against"}
  for dir in "${fuzz_dirs[@]}"; do
    name=fuzz-build-${dir//\//-}
    ran+=("$name")
    (cd "$root" && step "$name" cargo +nightly fuzz build --fuzz-dir "$dir" -O -a)
    if [[ $(cut -d' ' -f1 "$out/status/$name") == 0 ]]; then
      built+=("$dir")
      for target in $(cd "$root" && cargo +nightly fuzz list --fuzz-dir "$dir"); do
        targets+=("$dir $target")
      done
    else
      broken+=("$dir")
    fi
  done
  echo "fuzz: ${#built[@]} of ${#fuzz_dirs[@]} directories build$( ((${#broken[@]})) && echo "; not: ${broken[*]} (logs/fuzz-build-*.log)")"
  ((${#targets[@]} > 0)) || return 0
  share=$((minutes * 60 / ${#targets[@]}))
  ((share > 0)) || share=1
  echo "fuzz: ${#targets[@]} targets, ${share}s each"
  # The oracle's target starts from the oracle's own random cases, written
  # as its stored corpus (ignored by git), which reach far more than bytes.
  if [[ " ${built[*]} " == *" diff/fuzz "* ]]; then
    ran+=(fuzz-oracle-seeds)
    step fuzz-oracle-seeds "$root/diff/oracle.sh" ${against:+"$against"} \
      --seeds "$root/diff/fuzz/corpus/oracle" --cases 1000 --streams 5 ${jobs:+--threads "$jobs"}
  fi
  for spec in "${targets[@]}"; do
    read -r dir target <<<"$spec"
    dict="" asan=""
    for extra in "${fuzz_extras[@]}"; do
      [[ $extra == "$target "* ]] && read -r _ dict asan <<<"$extra"
    done
    local found=$out/fuzz-corpus/$target stored=$root/$dir/corpus/$target
    local args=(-max_total_time="$share" -rss_limit_mb=2048 -timeout=25)
    [[ -n $dict ]] && args+=(-dict="$root/$dict")
    mkdir -p "$found"
    local corpora=("$found")
    [[ -d $stored ]] && corpora+=("$stored")
    ran+=("fuzz-$target")
    (cd "$root" && ASAN_OPTIONS=$asan step "fuzz-$target" \
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

# The manifest's checks, a line each: NAME SCHEDULE WIRED WHY.
manifest() {
  awk '
    function flush() { if (name != "") print name, schedule, wired, why; name = ""; wired = "true"; why = "" }
    function value(line) { sub(/^[^=]*= */, "", line); gsub(/^"|"$/, "", line); return line }
    /^\[\[check\]\]/ { flush() }
    /^name *=/ { name = value($0) }
    /^schedule *=/ { schedule = value($0) }
    /^wired *=/ { wired = value($0) }
    /^why *=/ { why = value($0) }
    END { flush() }
  ' "$here/manifest.toml"
}

# The schedules a command runs: full runs quick's, deep full's and fuzz's.
schedules() {
  case $1 in
    quick) echo quick ;;
    full) echo quick full ;;
    deep) echo quick full deep fuzz ;;
    fuzz) echo fuzz ;;
  esac
}

# Checks the run against the manifest: each check it ran is listed there,
# and each listed for its schedules ran. Prints a line for each that is not.
missing() {
  local command=$1 name schedule wired why pattern found
  local wanted
  wanted=" $(schedules "$command") "
  for name in "${ran[@]}"; do
    found=""
    while read -r pattern _; do
      # shellcheck disable=SC2053 # the pattern may end in *, on purpose
      [[ $name == $pattern ]] && found=1
    done < <(manifest)
    [[ -n $found ]] || echo "  UNLISTED $name: not in manifest.toml"
  done
  while read -r pattern schedule wired why; do
    [[ $wanted == *" $schedule "* ]] || continue
    found=""
    for name in "${ran[@]}"; do
      # shellcheck disable=SC2053
      [[ $name == $pattern ]] && found=1
    done
    if [[ -z $found ]]; then
      if [[ $wired == false ]]; then
        echo "  NOT RUN $pattern: not wired yet: $why"
      else
        echo "  NOT RUN $pattern: required for $command, and did not run"
      fi
    fi
  done < <(manifest)
}

# The summary: each check that ran, the manifest's that did not, and the
# command's time beside its budget.
summary() {
  local command=$1 budget=$2 started=$3 name rc seconds failed=0 absent
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
  local report
  report=$(missing "$command")
  absent=0
  if [[ -n $report ]]; then
    echo "$report"
    absent=$(grep -c . <<<"$report")
  fi
  failed=$((failed + absent))
  echo "  ${#ran[@]} checks, $failed failed ($absent of the manifest's not run or not listed); results in $out"
  if [[ -n $budget ]] && ((took > budget)); then
    echo "  over budget: a check that does not fit moves to a deeper command"
  fi
  ((failed == 0))
}

command=${1:-}
shift || true
jobs="" against="" minutes=10
while (($#)); do
  case $1 in
    --jobs)
      [[ ${2:-} =~ ^[1-9][0-9]*$ ]] || { echo "harness.sh: --jobs takes a positive number" >&2; exit 2; }
      jobs=$2
      shift 2
      ;;
    --against)
      [[ -n ${2:-} ]] || { echo "harness.sh: --against takes a ref" >&2; exit 2; }
      against=$(git -C "$root" rev-parse --verify "$2^{commit}")
      shift 2
      ;;
    *) minutes=$1; shift ;;
  esac
done
if [[ -n $jobs ]]; then
  export CARGO_BUILD_JOBS=$jobs
fi
started=$(date +%s)
case $command in
  quick) quick; summary quick 60 "$started" ;;
  full) full; summary full 600 "$started" ;;
  deep) deep; summary deep "" "$started" ;;
  fuzz) fuzz "$minutes"; summary fuzz "" "$started" ;;
  scoreboard) scoreboard ;;
  *) echo "harness.sh: quick, full, deep, fuzz [MINUTES] (each [--jobs N] [--against REF]) or scoreboard" >&2; exit 2 ;;
esac
