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
package_target=${CARGO_TARGET_DIR:-"$repository/target"}
cargo package --locked --target-dir "$package_target" "$@"
version=$(cargo metadata --no-deps --format-version 1 --locked | cargo run --quiet --locked --manifest-path tools/xtask/Cargo.toml -- package-version)
fux_package="$package_target/package/fux-$version"
test -f "$fux_package/Cargo.toml"

cargo install --path "$fux_package" --root "$scratch/install" --locked
"$scratch/install/bin/fux" --version
FUX_BIN="$scratch/install/bin/fux" \
cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked --test binary
