#!/usr/bin/env bash
# Mutation testing (threats.md section 5, HAR-31): cargo-mutants over the
# mutants a change makes, or over the frozen guard set, each mutant tried
# against fux-vt's tests and fux's unit tests (which kill some that
# fux-vt's miss, such as palette::answer's; FUX_MUTANTS_TESTS, a nextest
# filter, says otherwise), with the `checks` feature the bounds tests
# need, two at a time, in the dev profile (no LTO), pinned to one core type.
#
#   fux-vt/compare/mutants.sh diff [REF]  the mutants in the diff from REF
#                                         (default HEAD~1) to the working tree
#   fux-vt/compare/mutants.sh guards      the frozen guard set: each line of
#                                         fux-vt/compare/mutants-guards.txt a
#                                         mutant's name as `--list` prints it
#
# It prints each outcome apart (caught, missed, timeout, unviable,
# equivalent, unattempted) and fails unless every mutant that builds is
# caught or listed as equivalent, with its reason, in mutants-equivalent.txt
# (a name, a tab, the reason). It fails on an empty selection, and in
# `guards` on a guard that isn't generated. Timeout and unattempted are
# never caught. Results go to $FUX_MUTANTS_OUT (default target/mutants), and
# temporary trees under $TMPDIR.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${FUX_MUTANTS_OUT:-$root/target/mutants}
cpus=${FUX_MUTANTS_CPUS:-4-11}
equivalent=$here/mutants-equivalent.txt
mkdir -p "$out"

# fux-vt's tests, and fux's own unit tests (where pane.rs's kill
# palette::answer's mutants); fux's integration tests take minutes a mutant
# and test the server, not fux-vt. Run by nextest, two at a time each.
tests=${FUX_MUTANTS_TESTS:-'package(fux-vt) | (package(fux) & kind(lib))'}
args=(--workspace --test-workspace=true --features fux-vt/checks --jobs 2 --output "$out"
  --test-tool nextest)
after=(-- -E "$tests" --test-threads 2)
case ${1-} in
  diff)
    ref=${2:-HEAD~1}
    git -C "$root" diff --src-prefix=a/ --dst-prefix=b/ "$ref" -- . >"$out/change.diff"
    args+=(--in-diff "$out/change.diff")
    ;;
  guards)
    guards=$here/mutants-guards.txt
    names=$(grep -v '^#' "$guards" | sed '/^$/d' || true)
    [[ -n $names ]] || { echo "mutants: no guards in $guards" >&2; exit 1; }
    # One alternation of the names, each matched whole.
    re=$(printf '%s\n' "$names" | sed 's/[][\.*^$()+?{}|]/\\&/g; s/.*/^&$/' | paste -sd'|')
    args+=(--re "$re")
    ;;
  *)
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac

cd "$root"
cargo mutants --list "${args[@]}" "${after[@]}" >"$out/selected.txt"
selected=$(wc -l <"$out/selected.txt")
if ((selected == 0)); then
  echo "mutants: the selection is empty" >&2
  exit 1
fi
if [[ $1 == guards ]]; then
  missing=$(printf '%s\n' "$names" | grep -vxF -f "$out/selected.txt" || true)
  if [[ -n $missing ]]; then
    printf 'mutants: guards not generated:\n%s\n' "$missing" >&2
    exit 1
  fi
fi
echo "mutants: $selected selected"

status=0
taskset -c "$cpus" cargo mutants "${args[@]}" "${after[@]}" || status=$?
# cargo-mutants exits 2 when a mutant is missed, 3 on a timeout: judged below.
case $status in 0 | 2 | 3) ;; *) echo "mutants: cargo-mutants failed ($status)" >&2; exit 1 ;; esac

python3 - "$out/mutants.out/outcomes.json" "$out/selected.txt" "$equivalent" <<'EOF'
import json, sys
outcomes = json.load(open(sys.argv[1]))["outcomes"]
selected = [l.rstrip("\n") for l in open(sys.argv[2]) if l.strip()]
equivalent = {}
for line in open(sys.argv[3]):
    if line.strip() and not line.startswith("#"):
        name, _, why = line.rstrip("\n").partition("\t")
        if not why.strip():
            sys.exit(f"mutants: {name!r} is listed equivalent with no reason")
        equivalent[name] = why
kinds = {"CaughtMutant": "caught", "MissedMutant": "missed", "Timeout": "timeout", "Unviable": "unviable"}
tally = {k: [] for k in ["caught", "missed", "timeout", "unviable", "equivalent", "unattempted"]}
tried = set()
for o in outcomes:
    scenario = o.get("scenario")
    if not isinstance(scenario, dict):
        continue  # the baseline
    name = scenario["Mutant"]["name"]
    tried.add(name)
    kind = kinds.get(o["summary"], "unattempted")
    if kind != "caught" and name in equivalent:
        kind = "equivalent"
    tally[kind].append(name)
tally["unattempted"] += [n for n in selected if n not in tried]
for kind, names in tally.items():
    print(f"{kind}: {len(names)}")
    if kind != "caught":
        for n in names:
            print(f"  {n}" + (f"  ({equivalent[n]})" if n in equivalent else ""))
# Unviable mutants don't build, so no test could catch them: reported,
# not failed.
bad = sum(len(tally[k]) for k in ["missed", "timeout", "unattempted"])
sys.exit(1 if bad else 0)
EOF
