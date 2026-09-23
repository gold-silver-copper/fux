#!/bin/sh
# Hunt 7, finding 010 (class 2 on Linux: a request fails for no reason the
# caller can act on, and the attachment ends with it).
#
# `UnixTransport::await_input` in `src/unix_http.rs` does one `read` on the
# socket and treats anything that is not `WouldBlock` or `TimedOut` as a
# transport error. A read on a socket with `SO_RCVTIMEO` set is not restarted
# after a signal handler runs: it fails with `EINTR` (signal(7), "Interruption
# of system calls and library functions by signal handlers"). fux sets a read
# timeout on every request, because `agent()` takes a whole-call budget.
#
# The frontend installs a `SIGWINCH` handler through `signal-hook` and sends
# every key, paste and resize through that transport on its main thread. So a
# terminal resize that lands while a request is in flight ends the request,
# and `viewer::run` returns the error:
#
#   fux: io: Interrupted system call (os error 4)
#
# The attachment is gone: the user is back at their shell, mid-session, with
# the panes still running on the server. Resizing a window is not a hostile
# act, and a signal handler firing during a read is not a failure.
#
# macOS restarts the read (SA_RESTART with no timeout semantics that defeat
# it), so this is Linux-only and neither platform's behaviour is stated in the
# README. The same transport is what fux's own integration tests use, which is
# why 25 to 27 of them fail on Linux x86_64, where the emulated CPU makes the
# window wide enough to hit reliably.
#
# This script drives a real `fux attach` in a pty and resizes that pty while
# typing. Nothing here is a hostile peer: it is one terminal being resized.
#
# Usage: 010-a-signal-ends-a-request-and-the-attachment.sh /path/to/fux
# Exit 0: reproduced (the frontend exited during the resizes).
# Exit 1: verified not reproduced (the frontend survived them).
# Exit 2: setup failure, or not this platform.
#
# This finding is Linux-only, so the script refuses to judge anywhere else
# rather than reporting a pass fux has not earned. Run on macOS by hand it
# survives 7278 resize rounds; that measurement is in BREAKS.md.
#
# NEGATIVE_CONTROL=1 types the same keys for the same time without resizing,
# which must leave the frontend attached (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

case "$(uname -s)" in
  Linux) ;;
  *)
    echo "this finding is Linux-only; $(uname -s) restarts the interrupted read" >&2
    exit 2 ;;
esac

DIR="$(mktemp -d /tmp/fux-repro-010.XXXXXX)" || exit 2

cleanup() {
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

cat > "$DIR/fux.json" <<'JSON'
{"shell":["/bin/sh","-c","printf 'PANE\n'; exec cat"]}
JSON

(cd "$DIR" && exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" TERM=xterm-256color \
  "$FUX" server --socket "$DIR/s/fux.sock" --config "$DIR/fux.json") \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.5
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

FUX="$FUX" DIR="$DIR" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import fcntl, json, os, pty, select, signal, socket, struct, sys, termios, time
FUX, DIR = os.environ["FUX"], os.environ["DIR"]
CONTROL = os.environ["CONTROL"] == "1"
SOCK = os.path.join(DIR, "s", "fux.sock")

def answers():
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "rpc.discover"}).encode()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(2)
    try:
        s.connect(SOCK)
        s.sendall(b"POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: %d\r\n"
                  b"Connection: close\r\n\r\n" % len(payload) + payload)
        return b"jsonrpc" in s.recv(4096)
    except Exception:
        return False
    finally:
        s.close()

for _ in range(250):
    if answers():
        break
    time.sleep(0.1)
else:
    print("setup: server never answered", file=sys.stderr)
    sys.exit(2)

pid, fd = pty.fork()
if pid == 0:
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    os.execve(FUX, [FUX, "attach"], {"PATH": "/usr/bin:/bin", "HOME": DIR,
                                     "TERM": "xterm-256color", "FUX_SOCKET": SOCK})

out = bytearray()
def drain(seconds):
    end = time.time() + seconds
    while True:
        r, _, _ = select.select([fd], [], [], max(0, end - time.time()))
        if not r:
            return
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            return
        if not chunk:
            return
        out.extend(chunk)

drain(1.5)
if b"\x1b[" not in bytes(out):
    print("setup: the frontend never painted", file=sys.stderr)
    os.kill(pid, signal.SIGKILL)
    sys.exit(2)

print("frontend attached; %s for up to 20s"
      % ("typing only (negative control)" if CONTROL else "typing while the terminal is resized"))
start = time.time()
rounds = 0
exited = None
while time.time() - start < 20:
    rounds += 1
    if not CONTROL:
        rows, cols = 20 + rounds % 20, 60 + rounds % 40
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        try:
            os.kill(pid, signal.SIGWINCH)
        except ProcessLookupError:
            pass
    try:
        os.write(fd, b"x")
    except OSError:
        pass
    drain(0.002)
    if os.waitpid(pid, os.WNOHANG)[0] != 0:
        exited = rounds
        break

drain(0.5)
tail = bytes(out[-300:]).decode("utf-8", "replace")
if exited is None:
    print("   %d rounds, frontend still attached" % rounds)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    print("NOT reproduced: the attachment survived")
    sys.exit(20)

print("   the frontend exited after %d rounds" % exited)
print("   its last output: %s" % repr(tail[-120:]))
if "Interrupted system call" in tail:
    print("REPRODUCED: a signal ended a request and the attachment with it")
    sys.exit(10)
print("REPRODUCED: the attachment ended during ordinary resizes")
sys.exit(10)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
