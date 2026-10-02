#!/usr/bin/env bash
# Builds fux-vt-compare and runs it with the given arguments; or, as
# `run.sh --cargo SUBCOMMAND ARGS...`, runs that cargo subcommand on it
# (`--cargo test`, `--cargo clippy --all-targets -- -D warnings`) in the
# same environment.
#
# libghostty-vt is built from Ghostty's source by Zig. This script fetches
# both, once, into a cache ($FUX_VT_COMPARE_CACHE, default
# ~/.cache/fux-vt-compare), and pins them:
#
# - Zig 0.16.0. The bindings' own pin of Ghostty needs Zig 0.15, which
#   cannot link on macOS 27.
# - Ghostty at 7aa95917, the first commit built by Zig 0.16. Its C API
#   differs from the one the bindings were generated from (a887df42) only
#   in the kitty-graphics temporary-file option, which is built out here,
#   and a new data key.
#
# - libvterm 0.3.3, from its release tarball (checked by SHA-256), compiled
#   by build.rs.
# - xterm.js: @xterm/headless, by `npm ci` in node/ (its package-lock.json
#   pins it), when Node is installed.
#
# tmux and xterm (with Xvfb) are used if installed; `fux-vt-compare
# engines` says which engines can run.
#
# On macOS 27, Zig's bundled libc++ does not compile against the 27.0 SDK,
# so the newest older SDK installed is used instead, through an `xcrun`
# shim (Zig asks `xcrun --sdk macosx --show-sdk-path`, which ignores
# SDKROOT).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
cache=${FUX_VT_COMPARE_CACHE:-$HOME/.cache/fux-vt-compare}
zig_version=0.16.0
ghostty_commit=7aa9591746ffa4d2eee458960c76554352832595
mkdir -p "$cache"

# Each Zig tarball's SHA-256, from ziglang.org/download/index.json.
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) zig_target=aarch64-macos zig_sha256=b23d70deaa879b5c2d486ed3316f7eaa53e84acf6fc9cc747de152450d401489 ;;
  Darwin-x86_64) zig_target=x86_64-macos zig_sha256=0387557ed1877bc6a2e1802c8391953baddba76081876301c522f52977b52ba7 ;;
  Linux-x86_64) zig_target=x86_64-linux zig_sha256=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00 ;;
  Linux-aarch64) zig_target=aarch64-linux zig_sha256=ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17 ;;
  *) echo "run.sh: no Zig build for $(uname -s) $(uname -m)" >&2; exit 2 ;;
esac

zig_dir=$cache/zig-$zig_target-$zig_version
if [[ ! -x $zig_dir/zig ]]; then
  echo "run.sh: fetching Zig $zig_version" >&2
  zig_tarball=$cache/zig-$zig_target-$zig_version.tar.xz
  curl -sSfL -o "$zig_tarball" "https://ziglang.org/download/$zig_version/zig-$zig_target-$zig_version.tar.xz"
  if [[ $(shasum -a 256 "$zig_tarball" | cut -d' ' -f1) != "$zig_sha256" ]]; then
    echo "run.sh: the Zig tarball does not match its SHA-256" >&2
    rm -f "$zig_tarball"
    exit 2
  fi
  tar -xJf "$zig_tarball" -C "$cache"
  rm "$zig_tarball"
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

libvterm_version=0.3.3
libvterm_sha256=09156f43dd2128bd347cbeebe50d9a571d32c64e0cf18d211197946aff7226e0
libvterm=$cache/libvterm-$libvterm_version
if [[ ! -f $libvterm/include/vterm.h ]]; then
  echo "run.sh: fetching libvterm $libvterm_version" >&2
  tarball=$cache/libvterm-$libvterm_version.tar.gz
  curl -sSfL -o "$tarball" "https://www.leonerd.org.uk/code/libvterm/libvterm-$libvterm_version.tar.gz"
  if [[ $(shasum -a 256 "$tarball" | cut -d' ' -f1) != "$libvterm_sha256" ]]; then
    echo "run.sh: libvterm tarball does not match its SHA-256" >&2
    exit 2
  fi
  tar -xzf "$tarball" -C "$cache"
  rm "$tarball"
fi
export LIBVTERM_SOURCE_DIR=$libvterm

if command -v npm >/dev/null && [[ -f $here/node/package.json && ! -d $here/node/node_modules ]]; then
  echo "run.sh: installing @xterm/headless" >&2
  npm ci --silent --prefix "$here/node"
fi

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
  if [ "$arg" = --show-sdk-path ]; then echo "$FUX_VT_COMPARE_SDK"; exit 0; fi
done
exec /usr/bin/xcrun "$@"
EOF
  chmod +x "$cache/bin/xcrun"
  export FUX_VT_COMPARE_SDK=$sdk PATH=$cache/bin:$PATH
fi

if [[ ${1:-} == --cargo ]]; then
  shift
  sub=$1
  shift
  exec cargo "$sub" --release --locked --manifest-path "$here/Cargo.toml" "$@"
fi
cargo build --release --locked --quiet --manifest-path "$here/Cargo.toml"
exec "$here/target/release/fux-vt-compare" "$@"
