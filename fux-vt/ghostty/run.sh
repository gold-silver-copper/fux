#!/usr/bin/env bash
# Builds fux-vt-ghostty and runs it with the given arguments; or, as
# `run.sh --cargo SUBCOMMAND ARGS...`, runs that cargo subcommand on it
# (`--cargo test`, `--cargo clippy --all-targets -- -D warnings`) in the
# same environment.
#
# libghostty-vt is built from Ghostty's source by Zig. This script fetches
# both, once, into a cache ($FUX_VT_GHOSTTY_CACHE, default
# ~/.cache/fux-vt-ghostty), and pins them:
#
# - Zig 0.16.0. The bindings' own pin of Ghostty needs Zig 0.15, which
#   cannot link on macOS 27.
# - Ghostty at 7aa95917, the first commit built by Zig 0.16. Its C API
#   differs from the one the bindings were generated from (a887df42) only
#   in the kitty-graphics temporary-file option, which is built out here,
#   and a new data key.
#
# On macOS 27, Zig's bundled libc++ does not compile against the 27.0 SDK,
# so the newest older SDK installed is used instead, through an `xcrun`
# shim (Zig asks `xcrun --sdk macosx --show-sdk-path`, which ignores
# SDKROOT).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
cache=${FUX_VT_GHOSTTY_CACHE:-$HOME/.cache/fux-vt-ghostty}
zig_version=0.16.0
ghostty_commit=7aa9591746ffa4d2eee458960c76554352832595
mkdir -p "$cache"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) zig_target=aarch64-macos ;;
  Darwin-x86_64) zig_target=x86_64-macos ;;
  Linux-x86_64) zig_target=x86_64-linux ;;
  Linux-aarch64) zig_target=aarch64-linux ;;
  *) echo "run.sh: no Zig build for $(uname -s) $(uname -m)" >&2; exit 2 ;;
esac

zig_dir=$cache/zig-$zig_target-$zig_version
if [[ ! -x $zig_dir/zig ]]; then
  echo "run.sh: fetching Zig $zig_version" >&2
  curl -sSfL "https://ziglang.org/download/$zig_version/zig-$zig_target-$zig_version.tar.xz" \
    | tar -xJ -C "$cache"
fi
export PATH=$zig_dir:$PATH

ghostty=$cache/ghostty
if [[ ! -d $ghostty/.git ]]; then
  echo "run.sh: fetching Ghostty" >&2
  git clone -q --filter=blob:none https://github.com/ghostty-org/ghostty.git "$ghostty"
fi
if [[ $(git -C "$ghostty" rev-parse HEAD) != "$ghostty_commit" ]]; then
  git -C "$ghostty" fetch -q origin "$ghostty_commit" 2>/dev/null || true
  git -C "$ghostty" -c advice.detachedHead=false checkout -q "$ghostty_commit"
fi
export GHOSTTY_SOURCE_DIR=$ghostty

if [[ $(uname -s) == Darwin ]] && (( $(sw_vers -productVersion | cut -d. -f1) >= 27 )); then
  sdks=/Library/Developer/CommandLineTools/SDKs
  sdk=$(ls -d "$sdks"/MacOSX2[0-6].[0-9]*.sdk 2>/dev/null | sort -V | tail -1 || true)
  if [[ -z $sdk ]]; then
    echo "run.sh: macOS 27 needs an older SDK in $sdks for Zig's libc++" >&2
    exit 2
  fi
  mkdir -p "$cache/bin"
  cat > "$cache/bin/xcrun" <<'EOF'
#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = --show-sdk-path ]; then echo "$FUX_VT_GHOSTTY_SDK"; exit 0; fi
done
exec /usr/bin/xcrun "$@"
EOF
  chmod +x "$cache/bin/xcrun"
  export FUX_VT_GHOSTTY_SDK=$sdk PATH=$cache/bin:$PATH
fi

if [[ ${1:-} == --cargo ]]; then
  shift
  sub=$1
  shift
  exec cargo "$sub" --release --locked --manifest-path "$here/Cargo.toml" "$@"
fi
cargo build --release --locked --quiet --manifest-path "$here/Cargo.toml"
exec "$here/target/release/fux-vt-ghostty" "$@"
