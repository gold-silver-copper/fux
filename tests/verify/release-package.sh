#!/bin/sh
set -eu

repository=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/fux-package-verify.XXXXXX")
cleanup() {
  rm -rf -- "$scratch"
}
trap cleanup EXIT HUP INT TERM

cd "$repository"
# Extra package flags (for example --allow-dirty for a local worktree) are explicit.
cargo package --locked "$@"
cargo metadata --no-deps --format-version 1 --locked > "$scratch/metadata.json"
version=$(cargo run --quiet --manifest-path tools/xtask/Cargo.toml --locked -- package-version < "$scratch/metadata.json")
fux_package="$repository/target/package/fux-$version"
test -f "$fux_package/Cargo.toml"

cargo install --path "$fux_package" --root "$scratch/install" --locked
"$scratch/install/bin/fux" --version
FUX_BIN="$scratch/install/bin/fux" \
cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked --test binary
