#!/bin/sh
# Run a command in the fux Linux container, on the repository checkout.
#
# Usage: fux-fuzz/linux/run.sh ARCH COMMAND...
#   ARCH is arm64 or amd64 (amd64 is emulated on an Apple silicon host).
#
#   fux-fuzz/linux/run.sh arm64 cargo test --locked
#   fux-fuzz/linux/run.sh amd64 sh -c 'cd fux-fuzz && cargo test --locked'
#
# The checkout is mounted at /src. Build output and the cargo registry live in
# named volumes per architecture (fux-linux-target-ARCH, fux-linux-cargo-ARCH),
# so repeated runs build incrementally; remove them with
#   docker volume rm fux-linux-target-ARCH fux-linux-cargo-ARCH
# The binary tests use is /target/debug/fux; fux-fuzz's is /target/debug/fux-fuzz.
#
# FUX_LINUX_TMP=ext4 runs the command with TMPDIR on a fresh ext4 filesystem,
# as GitHub's ubuntu runner has in /tmp. ext4 reuses a freed inode number at
# once, which the container's own overlayfs never does, and that difference
# hid hunt 8 finding 014. This needs a privileged container, for the loop
# mount, so it is opt-in; the command itself still runs as the ordinary user.
set -eu
ARCH="${1:?usage: $0 arm64|amd64 COMMAND...}"; shift
case "$ARCH" in arm64|amd64) ;; *) echo "unknown arch: $ARCH" >&2; exit 2 ;; esac
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
IMAGE="fux-linux:$ARCH"
# The image needs only the Dockerfile and the pinned toolchain file.
CONTEXT="$(mktemp -d)"
cp "$HERE/Dockerfile" "$ROOT/rust-toolchain.toml" "$CONTEXT/"
docker build -q --platform "linux/$ARCH" -t "$IMAGE" "$CONTEXT" >/dev/null
rm -rf "$CONTEXT"
# The volumes are created root-owned; hand them to the container user once.
docker run --rm --platform "linux/$ARCH" --user root \
  -v "fux-linux-target-$ARCH:/target" -v "fux-linux-cargo-$ARCH:/cargo-home/registry" \
  "$IMAGE" chown fux:fux /target /cargo-home/registry
if [ "${FUX_LINUX_TMP:-}" = ext4 ]; then
  # Mount as root, then drop to the ordinary user for the command itself.
  exec docker run --rm --init --privileged --user root --platform "linux/$ARCH" \
    -v "$ROOT:/src" \
    -v "fux-linux-target-$ARCH:/target" -v "fux-linux-cargo-$ARCH:/cargo-home/registry" \
    -e CARGO_TERM_COLOR=never \
    "$IMAGE" sh -c '
      set -e
      truncate -s 1G /ext4.img
      mkfs.ext4 -q /ext4.img
      mkdir -p /ext4
      mount -o loop /ext4.img /ext4
      chmod 1777 /ext4
      exec setpriv --reuid=fux --regid=fux --init-groups \
        env HOME=/home/fux TMPDIR=/ext4 PATH="$PATH" CARGO_HOME="$CARGO_HOME" \
        CARGO_TARGET_DIR="$CARGO_TARGET_DIR" CARGO_TERM_COLOR=never "$@"
    ' sh "$@"
fi
exec docker run --rm --init --platform "linux/$ARCH" \
  -v "$ROOT:/src" \
  -v "fux-linux-target-$ARCH:/target" -v "fux-linux-cargo-$ARCH:/cargo-home/registry" \
  -e CARGO_TERM_COLOR=never \
  "$IMAGE" "$@"
