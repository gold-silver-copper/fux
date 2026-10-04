#!/usr/bin/env bash
# diff/oracle.sh [REF] [ARGS...]: the working tree's fux-vt beside fux-vt
# at REF, by fux-vt-oracle (diff/oracle), with ARGS passed to it
# (`--cases N`, `--seed N`, `--replay FILE`; `--help` says them all).
# REF is any commit; by default, the merge base of HEAD and main.
#
# The commit is base-vt's rev in diff/oracle/Cargo.toml, and diff/Cargo.lock
# names the commit that was built. When REF is another commit, this pins
# it there, and says so: the two files change, to commit if the pin is
# to stay, or to `git checkout` to go back. A commit on GitHub is fetched
# from there; one that is only here, from this repository by a file:// URL,
# which is this machine's alone and not to be committed. Once fetched, a
# commit builds with no network.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
manifest=$here/oracle/Cargo.toml
url=https://github.com/gold-silver-copper/fux

ref=
if [[ $# -gt 0 && $1 != -* ]]; then
  ref=$1
  shift
fi
if [[ -z $ref ]]; then
  ref=$(git -C "$root" merge-base HEAD main)
fi
sha=$(git -C "$root" rev-parse --verify "$ref^{commit}")
pinned=$(sed -n -E 's/^base-vt = .*rev = "([0-9a-f]+)".*/\1/p' "$manifest")

locked=--locked
if [[ $sha != "$pinned" ]]; then
  if [[ -n $(git -C "$root" branch -r --contains "$sha" 2>/dev/null) ]]; then
    source=$url
  else
    source=file://$(cd "$(git -C "$root" rev-parse --git-common-dir)" && pwd)
    echo "oracle.sh: $sha is on no remote branch, so it is fetched from $source; do not commit this pin" >&2
  fi
  sed -i.bak -E "s|^base-vt = .*|base-vt = { package = \"fux-vt\", git = \"$source\", rev = \"$sha\" }|" "$manifest"
  rm -f "$manifest.bak"
  echo "oracle.sh: base-vt pinned to $sha (was $pinned): diff/oracle/Cargo.toml and diff/Cargo.lock change" >&2
  locked=
fi
# shellcheck disable=SC2086 # $locked is one flag or none
cargo build --release --quiet $locked --manifest-path "$manifest" --bin fux-vt-oracle
exec "$here/target/release/fux-vt-oracle" "$@"
