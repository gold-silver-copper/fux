#!/usr/bin/env bash
# worst-feed.sh OUT.json: the costliest and the longest single call fux-vt
# makes over every bounds reproducer (fux-vt/tests/bounds: every row at
# 1x, 10x and 100x, each at its chunkings) and every corpus recording read
# 4 KiB at a time as fux reads a pane (diff/oracle's `ceiling` part, which
# also resizes a full history both ways). Both run in a release build with
# fux-vt's work counters on (`checks`), which weighs each call's work in
# units, and times it: an upper bound on fux's own build, as counting only
# adds time. A bounds probe whose call took over 25 ms so leaves its input,
# and `fux-bench calls` feeds it again in fux's build, without the
# counters, to time it as fux would. The safety bar: no 4 KiB call past
# 50 ms. About 11 minutes on box2, 18 on desk (the bounds suite, one test
# at a time).
#
# OUT.json: worst_ms over the 4 KiB calls, each probe timed without the
# counters where it was kept, else with them (an upper bound), and its
# input; worst_counted_ms, the same all with the counters on; worst_units
# (twentieths, as Work::cost) and its input; chunk_bytes 4096; inputs, how
# many were measured at 4 KiB; timed_again, how many of them were kept and
# timed without the counters; bar_ms; head and harness (the last commit
# that changed what measures, -dirty if the tree differs); and whether the
# bounds suite passed and the oracle's part ran (it builds against HEAD,
# which a clone that got the commit by another ref may not fetch: then
# "corpus" says failed). Exit 1 if a 4 KiB call went past the bar.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:?usage: worst-feed.sh OUT.json}
lines=$here/target/worst-feed.txt again=$here/target/worst-feed-again.txt
kept=$here/target/worst-feed-inputs
rm -rf "$lines" "$again" "$kept"
mkdir -p "$kept"
export FUX_VT_WORST_FEED=$lines FUX_VT_WORST_FEED_INPUTS=$kept

suite=passed
cargo test --release --locked --features checks --manifest-path "$root/fux-vt/Cargo.toml" \
  --test bounds -- --include-ignored --test-threads=1 >"$here/target/worst-feed-bounds.log" 2>&1 ||
  suite=failed
corpus=passed
"$root/diff/oracle.sh" HEAD --cases 0 --streams 0 --cells 0 >"$here/target/worst-feed-oracle.log" 2>&1 ||
  corpus=failed
cargo run --release --locked --quiet --manifest-path "$root/bench/Cargo.toml" -- calls "$kept" >"$again"

dirty() { [[ -n $(git -C "$root" status --porcelain -- "$@") ]] && echo -dirty || true; }
head=$(git -C "$root" rev-parse HEAD)$(dirty .)
measures=(fux-vt/tests/bounds.rs fux-vt/tests/bounds diff/oracle bench/src/calls.rs fux-vt/compare/worst-feed.sh)
harness=$(git -C "$root" log -1 --format=%H -- "${measures[@]}")$(dirty "${measures[@]}")

# Each 4 KiB probe's time: timed again without the counters if it was
# kept (the first file), else its counted time.
awk -v head="$head" -v harness="$harness" -v suite="$suite" -v corpus="$corpus" '
  FILENAME == ARGV[1] { again[$1] = $4 + 0; timed++; next }
  $2 == 4096 {
    n++
    t = ($1 in again) ? again[$1] : $4 + 0
    if (t > ns) { ns = t; slow = $1 }
    if ($4 + 0 > counted) { counted = $4 + 0; slowest = $1 }
    if ($3 + 0 > units) { units = $3 + 0; costly = $1 }
  }
  END {
    printf "{\n  \"worst_ms\": %.3f,\n  \"worst_ms_input\": \"%s\",\n", ns / 1e6, slow
    printf "  \"worst_counted_ms\": %.3f,\n  \"worst_counted_ms_input\": \"%s\",\n", counted / 1e6, slowest
    printf "  \"worst_units\": %d,\n  \"worst_units_input\": \"%s\",\n", units, costly
    printf "  \"ceiling_units\": %d,\n  \"chunk_bytes\": 4096,\n  \"inputs\": %d,\n", 50000000 * 20, n
    printf "  \"timed_again\": %d,\n  \"bar_ms\": 50,\n", timed
    printf "  \"bounds_suite\": \"%s\",\n  \"corpus\": \"%s\",\n", suite, corpus
    printf "  \"head\": \"%s\",\n  \"harness\": \"%s\"\n}\n", head, harness
    exit (ns > 50e6)
  }' "$again" "$lines" >"$out" || past=1
cat "$out"
exit "${past:-0}"
