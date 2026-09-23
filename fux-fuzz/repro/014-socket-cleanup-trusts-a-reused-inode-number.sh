#!/bin/sh
# Hunt 8, finding 014 (class 6: a silent invariant break), Linux on ext4.
#
# When a fux server shuts down it removes its socket, but only if the file at
# the path is still the one it bound: another program may have replaced it,
# and removing that would cut off whoever is listening there now. fux decided
# "still the one" by comparing `(device, inode)` with what it recorded at bind.
#
# That pair names a file only while its inode is allocated. Once fux's
# listener closes, the socket's inode is freed, and ext4 hands a freed inode
# number to the next file created in the filesystem: 200 times out of 200 for
# a socket bound, unlinked and rebound at one path. So a socket someone binds
# there after fux's listener closes carries fux's old pair, and fux removes it.
# APFS, tmpfs, btrfs and overlayfs never reused the number in the same test,
# which is why no earlier run saw this: the first run of fux's CI did, because
# GitHub's ubuntu runner has /tmp on ext4 and fux's own unit test
# `transport::tests::cleanup_leaves_a_socket_that_replaced_its_own` fails there.
#
# Through the binary this needs a race, a replacement landing between the
# listener closing and the endpoint being dropped during shutdown, so the
# deterministic demonstration is that unit test, run with TMPDIR on a
# filesystem that reuses inode numbers. The stale-socket path had the same
# weakness between its probe and its removal.
#
# Usage: 014-socket-cleanup-trusts-a-reused-inode-number.sh /path/to/fux
# Exit 0: reproduced (the test fails: fux removed the replacement).
# Exit 1: verified not reproduced (the replacement was left alone).
# Exit 2: setup failure, not Linux, or no inode-reusing filesystem here.
#
# It needs cargo and the fux source beside it (it runs a unit test), and a
# TMPDIR on ext4 or another filesystem that reuses inode numbers: GitHub's
# ubuntu runner, or `FUX_LINUX_TMP=ext4 fux-fuzz/linux/run.sh`. The binary
# argument is for the common interface.
#
# NEGATIVE_CONTROL=1 runs the same test with TMPDIR on /dev/shm, a tmpfs that
# does not reuse inode numbers, where it passes either way (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$(uname -s)" in
  Linux) ;;
  *) echo "this finding is Linux-only; APFS does not reuse inode numbers" >&2; exit 2 ;;
esac
command -v cargo >/dev/null 2>&1 || { echo "cargo is required" >&2; exit 2; }
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }
ROOT="$(CDPATH="" cd -- "$(dirname -- "$0")/../.." && pwd)"
[ -f "$ROOT/src/transport/tests.rs" ] || { echo "no fux source at $ROOT" >&2; exit 2; }

if [ "${NEGATIVE_CONTROL:-0}" = "1" ]; then
  SCRATCH=/dev/shm
  echo "negative control: TMPDIR on $SCRATCH, which does not reuse inode numbers"
else
  SCRATCH="${TMPDIR:-/tmp}"
fi
[ -d "$SCRATCH" ] && [ -w "$SCRATCH" ] || { echo "$SCRATCH is not a writable directory" >&2; exit 2; }

# Does this filesystem hand a freed inode number straight to the next socket?
REUSED="$(SCRATCH="$SCRATCH" python3 - <<'PY'
import os, socket, tempfile
d = tempfile.mkdtemp(dir=os.environ["SCRATCH"])
p = os.path.join(d, "probe.sock")
same = 0
for _ in range(20):
    a = socket.socket(socket.AF_UNIX); a.bind(p); before = os.lstat(p).st_ino; a.close(); os.unlink(p)
    b = socket.socket(socket.AF_UNIX); b.bind(p); after = os.lstat(p).st_ino; b.close(); os.unlink(p)
    same += before == after
os.rmdir(d)
print(same)
PY
)" || { echo "could not probe $SCRATCH" >&2; exit 2; }
FS="$(stat -f -c %T "$SCRATCH" 2>/dev/null)"
echo "TMPDIR $SCRATCH ($FS): a freed socket inode number was reused $REUSED times in 20"

if [ "${NEGATIVE_CONTROL:-0}" != "1" ] && [ "$REUSED" -eq 0 ]; then
  echo "this filesystem does not reuse inode numbers, so it cannot show the finding" >&2
  echo "run on ext4, e.g. FUX_LINUX_TMP=ext4 fux-fuzz/linux/run.sh" >&2
  exit 2
fi

OUT="$(mktemp)" || exit 2
trap 'rm -f "$OUT"' EXIT INT TERM
(cd "$ROOT" && TMPDIR="$SCRATCH" cargo test --locked --bin fux \
  transport::tests::cleanup_leaves_a_socket_that_replaced_its_own > "$OUT" 2>&1)
STATUS=$?
grep -E '^test |Error:|test result' "$OUT" | sed 's/^/   /'

if grep -q 'test result: ok. 1 passed' "$OUT"; then
  echo "NOT reproduced: the socket that replaced fux's was left in place"
  exit 1
fi
if grep -q 'cleanup_leaves_a_socket_that_replaced_its_own ... FAILED' "$OUT"; then
  echo "REPRODUCED: fux removed a socket it did not bind, because its inode number was reused"
  exit 0
fi
echo "the test did not run (cargo exit $STATUS)" >&2
tail -20 "$OUT" >&2
exit 2
