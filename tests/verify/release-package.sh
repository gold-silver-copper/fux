#!/bin/sh
set -eu

repository=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/fux-package-verify.XXXXXX")
cleanup() {
  rm -rf -- "$scratch"
}
trap cleanup EXIT HUP INT TERM

cd "$repository"
cargo package --manifest-path zor/Cargo.toml --locked
cargo package --locked

zor_package="$repository/zor/target/package/zor-0.1.1"
fux_package="$repository/target/package/fux-0.2.0"
test -f "$zor_package/Cargo.toml"
test -f "$fux_package/Cargo.toml"

export CARGO_HOME="$scratch/cargo-home"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
cargo install --path "$zor_package" --root "$scratch/install" --locked
cargo install --path "$fux_package" --root "$scratch/install" --locked

"$scratch/install/bin/zor" --version
"$scratch/install/bin/fux" --version
FUX_BIN="$scratch/install/bin/fux" \
ZOR_BIN="$scratch/install/bin/zor" \
cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked --test binary
