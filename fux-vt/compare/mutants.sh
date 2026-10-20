#!/usr/bin/env bash
# Mutation testing (fux-vt/THREATS.md section 5): cargo-mutants over the
# mutants a change makes, or over the guard set, each mutant tried against
# fux-vt's tests and fux's unit tests (which kill some that fux-vt's miss,
# such as palette::answer's; FUX_MUTANTS_TESTS, a nextest filter, says
# otherwise), with the `checks` feature the bounds tests need, two at a
# time, in the dev profile (no LTO), on the CPUs FUX_MUTANTS_CPUS names, if
# any (one core type, for steady timeouts).
#
#   fux-vt/compare/mutants.sh diff [REF]  the mutants in the diff from REF
#                                         (default HEAD~1) to the working tree
#   fux-vt/compare/mutants.sh guards      the guard set, mutants-guards.txt
#   fux-vt/compare/mutants.sh check       only that every guard and every
#                                         equivalent names a mutant (seconds)
#
# A guard is named as `cargo mutants --list` names a mutant, its line and
# column replaced by the line's code: `FILE: WHAT @ CODE`. So a guard
# follows its line wherever it moves, and only a change to the guarded
# line itself (or its function's name) makes `check` fail, naming the
# guard to look at again. Every mutant a guard's name matches is a guard;
# `FILE:COLUMN: WHAT @ CODE` names one of several alike on a line, as each
# equivalent must.
#
# The guards run as the nightly job does: built at opt-level 1, each mutant
# under a 150-second timeout (FUX_MUTANTS_TIMEOUT), each test binary
# limited to 2 GiB of data on Linux so that a guard mutant that lets memory
# go fails its test, not the machine, the guard tests first
# (mutants-nextest.toml) and ignored tests run but the ones that fetch,
# need a harness or write files.
#
# It prints each outcome apart (caught, missed, timeout, unviable,
# equivalent, unattempted) and fails unless every mutant that builds is
# caught or listed as equivalent, with its reason, in mutants-equivalent.txt
# (a name as a guard is named, a tab, the reason). It fails on an empty
# selection. Timeout and unattempted are never caught. Results go to
# $FUX_MUTANTS_OUT (default target/mutants), and temporary trees under
# $TMPDIR.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${FUX_MUTANTS_OUT:-$root/target/mutants}
# The CPUs it runs on, if FUX_MUTANTS_CPUS names them (taskset's list);
# else wherever the system puts it.
cpus=${FUX_MUTANTS_CPUS:-}
equivalent=$here/mutants-equivalent.txt
mkdir -p "$out"

# fux-vt's tests, and fux's own unit tests (where pane.rs's kill
# palette::answer's mutants); fux's integration tests take minutes a mutant
# and test the server, not fux-vt. Run by nextest, two at a time each.
tests=${FUX_MUTANTS_TESTS:-'package(fux-vt) | (package(fux) & kind(lib))'}
if [[ ${1-} == guards ]]; then
  # Not the `_10x` and `_100x` bounds rows (minutes a mutant), nor the
  # ignored tests that fetch, need a harness or write files, nor fux's
  # one that makes a 1 GiB buffer; fux's journal tests need no server.
  tests=${FUX_MUTANTS_TESTS:-'(package(fux-vt) & !test(/_10x$/) & !test(/_100x$/) & !test(=write_the_seeds) & !test(=moshs_fuzz_corpus_replays_with_no_fault) & !test(=libvterm_passes_its_own_tests)) | (package(fux) & (kind(lib) | binary(journal)) & !test(=session::tests::keys_for_several_panes_go_to_all_or_none))'}
  export CARGO_PROFILE_DEV_OPT_LEVEL=${CARGO_PROFILE_DEV_OPT_LEVEL:-1}
  if [[ $(uname) == Linux ]]; then
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER=${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER:-prlimit --data=2147483648 --}
  fi
fi
# FUX_MUTANTS_JOBS mutants at a time (2 if unset); FUX_MUTANTS_SHARD, as
# K/N, runs only the Kth of N shards of the selection (for CI's matrix).
args=(--colors never --workspace --test-workspace=true --features fux-vt/checks --jobs "${FUX_MUTANTS_JOBS:-2}"
  --output "$out" --test-tool nextest --timeout "${FUX_MUTANTS_TIMEOUT:-150}"
  --build-timeout "${FUX_MUTANTS_BUILD_TIMEOUT:-600}")
# The first failure ends a mutant's tests at once: a test the mutant made
# hang must not hold up the one that fails.
after=(-- --config-file "$here/mutants-nextest.toml" -E "$tests" --test-threads 2
  --max-fail=1:immediate)
if [[ ${1-} == guards ]]; then
  after+=(--run-ignored all)
fi
case ${1-} in
  diff)
    ref=${2:-HEAD~1}
    git -C "$root" diff --src-prefix=a/ --dst-prefix=b/ "$ref" -- . >"$out/change.diff"
    args+=(--in-diff "$out/change.diff")
    ;;
  guards | check)
    guards=$here/mutants-guards.txt
    files=$(grep -v '^#' "$guards" | sed -n 's/^\([^:]*\): .*/\1/p' | sort -u)
    [[ -n $files ]] || { echo "mutants: no guards in $guards" >&2; exit 1; }
    while read -r file; do
      args+=(-f "$file")
    done <<<"$files"
    # Every mutant of the guards' files, each named as a guard is: the
    # ones a guard names are the guards, by their names this run.
    (cd "$root" && cargo mutants --list "${args[@]}" "${after[@]}") >"$out/listed.txt"
    python3 - "$root" "$out/listed.txt" "$guards" "$equivalent" >"$out/guards.txt" <<'EOF'
import re, sys
root, listed, guards, equivalent = sys.argv[1:]
sources = {}
def named(mutant):
    """The mutant's names: without its column, and with it."""
    path, line, column, what = re.match(r"([^:]+):(\d+):(\d+): (.*)", mutant).groups()
    if path not in sources:
        sources[path] = open(f"{root}/{path}").read().split("\n")
    code = sources[path][int(line) - 1].strip()
    return f"{path}: {what} @ {code}", f"{path}:{column}: {what} @ {code}"
names = {}
for mutant in open(listed).read().split("\n"):
    if mutant:
        for name in named(mutant):
            names.setdefault(name, []).append(mutant)
wanted = [l for l in open(guards).read().split("\n") if l and not l.startswith("#")]
lost = [g for g in wanted if g not in names]
listed = [l.split("\t")[0] for l in open(equivalent).read().split("\n") if l and not l.startswith("#")]
guarded = {m for g in wanted for m in names.get(g, [])}
stale = [e for e in listed if not set(names.get(e, [])) <= guarded or not names.get(e)]
vague = [e for e in listed if len(names.get(e, [])) > 1]
if lost:
    sys.exit("mutants: guards that name no mutant (their line changed: look at it again):\n" + "\n".join(lost))
if stale:
    sys.exit("mutants: listed equivalent but not guards:\n" + "\n".join(stale))
if vague:
    sys.exit("mutants: equivalents that name several mutants (name one by its column, FILE:COLUMN: ...):\n" + "\n".join(vague))
for g in wanted:
    print("\n".join(names[g]))
EOF
    # One alternation of the names, each matched whole.
    re=$(sed 's/[][\.*^$()+?{}|]/\\&/g; s/.*/^&$/' "$out/guards.txt" | paste -sd'|')
    args+=(--re "$re")
    ;;
  *)
    sed -n '2,/^set -euo/{/^set -euo/!p}' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac

cd "$root"
cargo mutants --list "${args[@]}" "${after[@]}" >"$out/selected.txt"
if [[ $1 == guards || $1 == check ]]; then
  missing=$(grep -vxF -f "$out/selected.txt" "$out/guards.txt" || true)
  if [[ -n $missing ]]; then
    printf 'mutants: guards not generated:\n%s\n' "$missing" >&2
    exit 1
  fi
fi
if [[ $1 == check ]]; then
  echo "mutants: all $(grep -c '^[^#]' "$here/mutants-guards.txt") guards name a mutant ($(wc -l <"$out/guards.txt") mutants)"
  exit 0
fi
if [[ -n ${FUX_MUTANTS_SHARD-} ]]; then
  args+=(--shard "$FUX_MUTANTS_SHARD")
  cargo mutants --list "${args[@]}" "${after[@]}" >"$out/selected.txt"
fi
if [[ $1 == guards ]]; then
  grep -xF -f "$out/guards.txt" "$out/selected.txt" >"$out/judged.txt" || true
else
  cp "$out/selected.txt" "$out/judged.txt"
fi
selected=$(wc -l <"$out/selected.txt")
if ((selected == 0)); then
  echo "mutants: the selection is empty" >&2
  exit 1
fi
echo "mutants: $selected selected"

status=0
if [[ -n $cpus ]]; then
  taskset -c "$cpus" cargo mutants "${args[@]}" "${after[@]}" || status=$?
else
  cargo mutants "${args[@]}" "${after[@]}" || status=$?
fi
# cargo-mutants exits 2 when a mutant is missed, 3 on a timeout: judged below.
case $status in 0 | 2 | 3) ;; *) echo "mutants: cargo-mutants failed ($status)" >&2; exit 1 ;; esac

python3 - "$root" "$out/mutants.out/outcomes.json" "$out/judged.txt" "$equivalent" <<'EOF'
import json, re, sys
root = sys.argv[1]
sources = {}
def named(mutant):
    """The mutant's names: without its column, and with it."""
    path, line, column, what = re.match(r"([^:]+):(\d+):(\d+): (.*)", mutant).groups()
    if path not in sources:
        sources[path] = open(f"{root}/{path}").read().split("\n")
    code = sources[path][int(line) - 1].strip()
    return f"{path}: {what} @ {code}", f"{path}:{column}: {what} @ {code}"
outcomes = json.load(open(sys.argv[2]))["outcomes"]
selected = [l.rstrip("\n") for l in open(sys.argv[3]) if l.strip()]
judged = set(selected)
equivalent = {}
for line in open(sys.argv[4]):
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
    if name not in judged:
        continue  # a mutant cargo-mutants adds beyond the guards
    tried.add(name)
    kind = kinds.get(o["summary"], "unattempted")
    # Only a mutant no test caught can be equivalent: one that timed out
    # changed behaviour (it hung), whatever the list says.
    if kind == "missed" and any(n in equivalent for n in named(name)):
        kind = "equivalent"
    tally[kind].append(name)
tally["unattempted"] += [n for n in selected if n not in tried]
for kind, names in tally.items():
    print(f"{kind}: {len(names)}")
    if kind != "caught":
        for n in names:
            why = next((equivalent[k] for k in named(n) if k in equivalent), None)
            print(f"  {n}" + (f"  ({why})" if why else ""))
# Unviable mutants don't build, so no test could catch them: reported,
# not failed.
bad = sum(len(tally[k]) for k in ["missed", "timeout", "unattempted"])
sys.exit(1 if bad else 0)
EOF
