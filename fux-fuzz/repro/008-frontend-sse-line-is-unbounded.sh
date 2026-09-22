#!/bin/sh
# Hunt 6, finding 008 (class 5: unbounded growth under a bounded input).
#
# `fux attach` reads the `fux.frame+watch` stream with `BufReader::read_line`
# into a `String` (src/viewer.rs ~150-165). Server-sent events are newline
# framed, so a peer that never sends a newline makes the frontend buffer the
# whole stream in memory. There is no cap on the line, on the frame, or on the
# decoded `Frame.paint`.
#
# Measured: resident memory climbs at roughly 95 MB/s for as long as the peer
# keeps writing, reaching 5.3 GB in sixty seconds before the peer stopped. The
# frontend is not painting anything during this; it is accumulating one line.
#
# The peer here is a small socket server in this script -- it is never fux. It
# is reached the way any peer is: `FUX_SOCKET` names a socket, and the client's
# own check (`transport::check_client_socket`) is satisfied by any socket the
# user owns in a private directory, which any same-user process can create. The
# same shape would arrive from a fux server that ever emitted a frame larger
# than the frontend can hold, so the missing bound is the finding, not the peer.
#
# Fixed: the frontend reads each event through a bound, and passing it ends
# the attachment with a message naming the bound and the terminal restored.
# The bound is 64 MiB, about five times the densest legitimate frame measured
# (a 4096x4096 viewer changing colour every cell serializes to 11.6 MB).
#
# This script stops at 1500 MB or 25 seconds, whichever comes first, so it
# cannot pressure the machine.
#
# Usage: 008-frontend-sse-line-is-unbounded.sh /path/to/fux
# Exit 0: reproduced (the frontend grew past 300 MB on one unterminated line).
# Exit 1: verified not reproduced (the frontend stayed bounded).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 makes the peer send the same bytes as ordinary newline
# terminated frames, which must leave the frontend bounded (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

FUX="$FUX" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import fcntl, json, os, pty, select, shutil, socket, struct, subprocess, sys
import tempfile, termios, threading, time

FUX = os.environ["FUX"]
CONTROL = os.environ["CONTROL"] == "1"
CEILING_MB = 1500
SECONDS = 25
THRESHOLD_MB = 300

d = tempfile.mkdtemp(prefix="fux-repro-008.", dir="/tmp")
sd = os.path.join(d, "s")
os.mkdir(sd, 0o700)
path = os.path.join(sd, "fux.sock")
stop = threading.Event()
sent_mb = [0]

def peer():
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(path)
    os.chmod(path, 0o600)
    srv.listen(8)
    srv.settimeout(0.5)
    while not stop.is_set():
        try:
            c, _ = srv.accept()
        except socket.timeout:
            continue
        except OSError:
            break
        threading.Thread(target=serve, args=(c,), daemon=True).start()
    try:
        srv.close()
    except Exception:
        pass

def serve(c):
    try:
        c.settimeout(10)
        req = b""
        while b"\r\n\r\n" not in req:
            chunk = c.recv(65536)
            if not chunk:
                return
            req += chunk
        if b"fux.frame+watch" not in req:
            body = b'{"jsonrpc":"2.0","id":1,"result":{"viewer":4294967295}}'
            c.sendall(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n"
                      b"content-length: %d\r\n\r\n" % len(body) + body)
            return
        c.sendall(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n"
                  b"transfer-encoding: chunked\r\n\r\n")
        blob = b"a" * (1 << 20)
        while not stop.is_set():
            if CONTROL:
                # the same volume, but as complete newline-terminated frames
                frame = (b'data: {"jsonrpc":"2.0","id":2,"result":'
                         b'{"paint":"' + blob[:4096] + b'","detach":false}}\n\n')
            else:
                frame = blob              # one line that never ends
            try:
                c.sendall(b"%x\r\n" % len(frame) + frame + b"\r\n")
            except OSError:
                return
            sent_mb[0] += len(frame) / (1 << 20)
    except Exception:
        pass
    finally:
        try:
            c.close()
        except Exception:
            pass

threading.Thread(target=peer, daemon=True).start()
time.sleep(0.4)
if not os.path.exists(path):
    print("setup: the peer never created its socket", file=sys.stderr)
    stop.set()
    shutil.rmtree(d, ignore_errors=True)
    sys.exit(2)

pid, fd = pty.fork()
if pid == 0:
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    os.environ.clear()
    os.environ.update({"PATH": "/usr/bin:/bin", "HOME": d, "TERM": "xterm-256color",
                       "FUX_SOCKET": path})
    os.execv(FUX, [FUX, "attach"])

def rss_mb(p):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(p)],
                         capture_output=True, text=True).stdout.strip()
    return (int(out) // 1024) if out else 0

base = None
peak = 0
exited = False
t0 = time.time()
while time.time() - t0 < SECONDS:
    r, _, _ = select.select([fd], [], [], 0.2)
    if r:
        try:
            if not os.read(fd, 1 << 16):
                break
        except OSError:
            break
    cur = rss_mb(pid)
    if cur:
        if base is None:
            base = cur
        peak = max(peak, cur)
    if peak > CEILING_MB:
        print("   stopping: frontend RSS reached the %d MB ceiling" % CEILING_MB)
        break
    done, _ = os.waitpid(pid, os.WNOHANG)
    if done:
        exited = True
        break

stop.set()
try:
    os.kill(pid, 9)
    os.waitpid(pid, 0)
except Exception:
    pass
try:
    os.close(fd)
except Exception:
    pass
shutil.rmtree(d, ignore_errors=True)

base = base or 0
growth = peak - base
print("peer sent %.0f MB; frontend RSS baseline %d MB, peak %d MB, growth %d MB%s"
      % (sent_mb[0], base, peak, growth, " (frontend exited)" if exited else ""))
if growth >= THRESHOLD_MB:
    print("REPRODUCED: one unterminated SSE line grew `fux attach` by %d MB" % growth)
    sys.exit(10)
print("frontend growth stayed under %d MB" % THRESHOLD_MB)
sys.exit(20)
PY
RESULT=$?
case "$RESULT" in
  10) exit 0 ;;
  20) echo "NOT reproduced: the frontend bounded the stream"; exit 1 ;;
  2)  echo "setup failure"; exit 2 ;;
  *)  echo "setup failure (client exit $RESULT)"; exit 2 ;;
esac
