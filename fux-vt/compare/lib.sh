# shellcheck shell=bash
# The harness's own functions, which harness.sh sources: a step run with
# its log and status (step), and checks run side by side (spec, together).
# Apart, so a test can source them alone (compare's
# a_grouped_checks_path_with_a_space_stays_one_word): they read the
# caller's $out and $jobs.

# step NAME COMMAND...: runs COMMAND with its output in logs/NAME.log, and
# leaves its exit status and seconds in status/NAME. What an earlier run
# left (NAME.json) is removed first, so no result outlives the run that
# made it.
step() {
  local name=$1 start rc=0
  shift
  rm -f "$out/$name.json"
  start=$(date +%s)
  "$@" >"$out/logs/$name.log" 2>&1 || rc=$?
  local seconds=$(($(date +%s) - start))
  echo "$rc $seconds" >"$out/status/$name"
  printf '  %-14s %s %4ss\n' "$name" "$([[ $rc == 0 ]] && echo ok || echo FAIL)" "$seconds"
}

# spec NAME COMMAND...: a STEP for `together`, its words quoted (printf
# %q), so a path with a space stays one word.
spec() {
  printf '%q ' "$@"
}

# together STEP...: each STEP (`spec`'s "name command args") side by side.
# With --jobs N, at most N at a time, and a STEP marked "+" (one that runs
# N threads itself) alone.
together() {
  local spec pids=()
  for spec in "$@"; do
    if [[ -n $jobs ]] && { [[ $spec == +* ]] || ((${#pids[@]} >= jobs)); }; then
      # An empty array is unbound under set -u in bash 3.2 (macOS's).
      if ((${#pids[@]})); then
        wait "${pids[@]}"
      fi
      pids=()
    fi
    if [[ -n $jobs && $spec == +* ]]; then
      eval "step ${spec#+}"
      continue
    fi
    eval "step ${spec#+}" &
    pids+=($!)
  done
  if ((${#pids[@]})); then
    wait "${pids[@]}"
  fi
}
