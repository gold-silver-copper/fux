#!/usr/bin/env bash
# diff/oracle.sh [REF] [ARGS...]: the working tree's fux-vt beside fux-vt
# at REF, by fux-vt-oracle (diff/oracle), with ARGS passed to it
# (`--cases N`, `--seed N`, `--replay FILE`; `--help` says them all).
# REF is any commit; by default the latest main, fetched from origin first.
#
# diff/oracle.sh --pin [REF]: only pin REF, and leave the pin in place
# (CI pins HEAD this way before it builds the diff/ workspace).
#
# The commit is base-vt's rev in diff/oracle/Cargo.toml, fetched from this
# repository by a file:// URL. A run pins it there (Cargo.lock follows) and
# puts both files back when it exits, so it leaves no change behind.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
manifest=$here/oracle/Cargo.toml
lock=$here/Cargo.lock
log=$here/target/oracle-build.log

keep=
if [[ ${1-} == --pin ]]; then
  keep=1
  shift
fi
ref=
if [[ $# -gt 0 && $1 != -* ]]; then
  ref=$1
  shift
fi
if [[ -z $ref ]]; then
  git -C "$root" fetch --quiet origin main
  ref=origin/main
fi
sha=$(git -C "$root" rev-parse --verify "$ref^{commit}")
source=file://$(cd "$(git -C "$root" rev-parse --git-common-dir)" && pwd)

mkdir -p "$here/target"
if [[ -z $keep ]]; then
  saved=$(mktemp -d "$here/target/oracle-pin.XXXXXX")
  cp "$manifest" "$lock" "$saved/"
  trap 'cp "$saved/Cargo.toml" "$manifest"; cp "$saved/Cargo.lock" "$lock"; rm -rf "$saved"' EXIT
fi
sed -i.bak -E "s|^base-vt = .*|base-vt = { package = \"fux-vt\", git = \"$source\", rev = \"$sha\" }|" "$manifest"
rm -f "$manifest.bak"
# A local commit (a shallow CI checkout's too) is fetched by git itself.
export CARGO_NET_GIT_FETCH_WITH_CLI=true
cargo fetch --quiet --manifest-path "$manifest"
[[ -n $keep ]] && exit 0

echo "oracle.sh: the working tree beside $ref ($sha)" >&2
if ! cargo build --release --quiet --locked --manifest-path "$manifest" --bin fux-vt-oracle 2>"$log"; then
  cat >&2 <<EOF
oracle.sh: fux-vt-oracle does not build against $ref ($sha).
If this tree changed fux-vt's public API, the one adapter in
diff/oracle/src/side.rs cannot read both sides: to compare such a branch,
add a temporary shim for the base side there, and never commit it.
rustc's errors: $log
EOF
  exit 1
fi
"$here/target/release/fux-vt-oracle" "$@"
