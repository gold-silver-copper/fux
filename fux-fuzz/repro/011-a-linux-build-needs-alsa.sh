#!/bin/sh
# Hunt 7, finding 011 (class: platform).
#
# `cargo install fux` on a clean Linux fails in a build script:
#
#   error: failed to run custom build command for `alsa-sys v0.4.0`
#   Could not run `pkg-config --libs --cflags alsa`
#
# The chain is entirely transitive, and fux asks for none of it:
#
#   fux -> bevy_remote -> bevy_dev_tools -> bevy_audio -> rodio -> cpal -> alsa-sys
#
# `bevy_remote` takes `bevy_dev_tools` for `schedule_data`, which is what
# answers `schedule.list` and `schedule.graph`, and `bevy_dev_tools` pulls
# `bevy_audio` unconditionally. fux already sets `default-features = false`
# and takes only `bevy_asset`; there is no feature that removes this from
# here. The built binary links `libasound.so.2`, a sound library a terminal
# multiplexer has no use for.
#
# This script asks the resolved dependency graph rather than building, so it
# is quick and runs anywhere: the chain is the defect, and a build failure on
# a machine without the headers is its consequence. It resolves for a Linux
# target explicitly, so a macOS host reports what a Linux user would get.
#
# Usage: 011-a-linux-build-needs-alsa.sh /path/to/fux
# Exit 0: reproduced (a Linux build of fux pulls alsa-sys).
# Exit 1: verified not reproduced (it does not).
# Exit 2: setup or infrastructure failure.
#
# The binary argument is for the common interface; what is under test is the
# manifest beside it, so the script locates the repository from its own path.
#
# NEGATIVE_CONTROL=1 resolves the same graph for a macOS target, where the
# chain does not appear, which must not report the finding (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
command -v cargo >/dev/null 2>&1 || { echo "cargo is required" >&2; exit 2; }

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
[ -f "$ROOT/Cargo.toml" ] || { echo "no Cargo.toml at $ROOT" >&2; exit 2; }

if [ "${NEGATIVE_CONTROL:-0}" = "1" ]; then
  TARGET=aarch64-apple-darwin
  echo "negative control: resolving for $TARGET, where the chain is absent"
else
  TARGET=x86_64-unknown-linux-gnu
  echo "resolving the dependency graph for $TARGET"
fi

TREE="$(cd "$ROOT" && cargo tree --locked --offline --target "$TARGET" -e normal 2>/dev/null)"
if [ -z "$TREE" ]; then
  TREE="$(cd "$ROOT" && cargo tree --locked --target "$TARGET" -e normal 2>/dev/null)"
fi
[ -n "$TREE" ] || { echo "cargo tree produced nothing for $TARGET" >&2; exit 2; }

# The tree must be the real one: fux itself has to be in it.
echo "$TREE" | grep -q '^fux v' || { echo "cargo tree did not resolve fux" >&2; exit 2; }

if echo "$TREE" | grep -q 'alsa-sys'; then
  echo "   the chain, as resolved:"
  (cd "$ROOT" && cargo tree --locked --offline --target "$TARGET" -e normal -i alsa-sys 2>/dev/null \
    || cargo tree --locked --target "$TARGET" -e normal -i alsa-sys 2>/dev/null) \
    | sed 's/^/     /' | head -12
  echo "REPRODUCED: a $TARGET build of fux pulls alsa-sys, so it needs ALSA headers"
  exit 0
fi

echo "NOT reproduced: no alsa-sys in the $TARGET graph"
exit 1
