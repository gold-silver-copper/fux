# shellcheck shell=bash
# The harness's own functions, which harness.sh sources: a step run with
# its log, status and stamp (step), and checks run side by side (spec,
# together). Apart, so a test can source them alone (compare's
# a_grouped_checks_path_with_a_space_stays_one_word): they read the
# caller's $out, $product, $harness and $jobs.

# A file's SHA-256: by sha256sum, else by shasum -a 256 (some systems have
# only one of them).
sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

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
  # A hash that can't be made leaves the stamp's empty, and the step runs.
  if [[ -f $binary ]]; then
    sha=$(sha256 "$binary") || sha=""
  fi
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
