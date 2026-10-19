#!/usr/bin/env bash
# Which `Feature`s and host calls the `terminal` and `deep` targets reach on
# their corpus and frozen seeds. Builds them with the `reach` feature,
# replays every input once (each in a process of its own, so one that fails
# doesn't hide the rest), prints what each target reached, and fails naming
# every input that fails its target's checks and everything a target didn't
# reach. From the repository root: fux-vt/fuzz/reach.sh
set -euo pipefail
dir=fux-vt/fuzz
cargo +nightly fuzz build --fuzz-dir "$dir" -O --sanitizer none --features reach
host=$(rustc +nightly -vV | sed -n 's/^host: //p')
bin="${CARGO_TARGET_DIR:-$dir/target}/$host/release"
calls="process_until_stop FrameScan set_history_bytes record_prints resize full_refresh_since Window::text take_prints"
declare -A need=(
  [terminal]="$calls"
  [deep]="$calls set_host_graphics clear_history tick set_host_palette require_mark_nonce note_submit set_picture_bytes host_reads"
)
log=$(mktemp)
trap 'rm -f "$log"' EXIT
failed=0
for target in terminal deep; do
  : >"$log"
  inputs=0
  for input in "$dir/corpus/$target"/* "$dir/seeds/$target"/*; do
    [[ -f "$input" ]] || continue
    inputs=$((inputs + 1))
    if ! "$bin/$target" -rss_limit_mb=2048 -timeout=60 "$input" 2>>"$log" >/dev/null; then
      echo "$target: ${input#"$dir"/} fails its checks"
      failed=1
    fi
  done
  reached=$(sed -n 's/^reached: //p' "$log" | sort -u)
  echo "$target: $inputs inputs"
  # The features are all `Feature::ALL` names, which each run lists.
  features=$(sed -n 's/^of Feature //p' <<<"$reached")
  for name in $features; do
    if grep -qx "feature $name" <<<"$reached"; then
      echo "  reached  feature $name"
    else
      echo "  MISSING  feature $name"
      failed=1
    fi
  done
  for name in ${need[$target]}; do
    if grep -qx "call $name" <<<"$reached"; then
      echo "  reached  $name"
    else
      echo "  MISSING  $name"
      failed=1
    fi
  done
done
exit "$failed"
