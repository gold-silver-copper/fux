# Sourced by the release checks here: a private fux server, and real xterm
# 411 on a private Xvfb display, each stopped by the caller's EXIT trap
# (`trap stop EXIT`), whatever happens.
#
# FUX_BIN names the fux binary (default: the release build in
# CARGO_TARGET_DIR, else target/); not FUX, which every fux pane has. Its
# server is private: FUX_SOCKET, FUX_PANE, FUX_SERVER and TERM_PROGRAM
# unset, a fresh 0700 XDG_RUNTIME_DIR under $work, and an explicit
# FUX_SOCKET there; it is stopped with its own force-kill-server.
# Xvfb and every xterm are stopped by their PIDs alone.

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
# Never under /tmp, which may be memory (a RAM-backed tmpfs).
case ${TMPDIR:?set TMPDIR, off /tmp} in
  /tmp | /tmp/*) echo "release: TMPDIR is under /tmp; set it elsewhere" >&2; exit 2 ;;
esac
work=$(mktemp -d "$TMPDIR/fux-release.XXXXXX")
fux_bin=${FUX_BIN:-${CARGO_TARGET_DIR:-$repo/target}/release/fux}
xvfb_pid=
fux_started=

stop() {
  if [[ -n $fux_started ]]; then
    "$fux_bin" force-kill-server >/dev/null 2>&1 || true
  fi
  if [[ -n $xvfb_pid ]]; then
    kill "$xvfb_pid" 2>/dev/null || true
    wait "$xvfb_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}

# A private fux server, started.
fux_server() {
  [[ -x $fux_bin ]] || { echo "release: no fux at $fux_bin (set FUX_BIN)" >&2; return 2; }
  unset FUX_SOCKET FUX_PANE FUX_SERVER TERM_PROGRAM
  export XDG_RUNTIME_DIR=$work/run
  mkdir -m 700 "$XDG_RUNTIME_DIR"
  export FUX_SOCKET=$XDG_RUNTIME_DIR/fux.sock
  "$fux_bin" start
  fux_started=1
}

# Runs CMD, a line for sh -c, in a new pane of COLSxROWS and waits up to
# SECONDS for it to end, leaving the pane in $pane: fux_run COLS ROWS
# SECONDS CMD. It has ended once its version's last count, ENDED, is 1
# (`%2:R.S.B.D.1:RUN`).
fux_run() {
  pane=$("$fux_bin" opts "$1" "$2" size str "$work" cwd str sh program str -c arg str "$4" arg run 2>"$work/run.err" | head -n1 | cut -d: -f1) || true
  [[ $pane == %* ]] || { echo "release: fux did not start $4" >&2; cat "$work/run.err" >&2; return 1; }
  local end=$((SECONDS + $3))
  until [[ $("$fux_bin" "$pane" str version get 2>/dev/null) == *.1:* ]]; do
    (( SECONDS < end )) || { echo "release: $4 still runs after $3 s" >&2; return 1; }
    sleep 1
  done
}

# Xvfb on a display of its own, exported as DISPLAY; xterm 411 required.
xvfb_start() {
  [[ $(xterm -version 2>/dev/null) == "XTerm(411)" ]] || { echo "release: needs xterm 411" >&2; return 2; }
  command -v Xvfb >/dev/null || { echo "release: needs Xvfb" >&2; return 2; }
  Xvfb -displayfd 3 -nolisten tcp -noreset -screen 0 1600x1200x24 3>"$work/display" 2>"$work/xvfb.log" &
  xvfb_pid=$!
  local n
  for n in $(seq 100); do
    [[ -s $work/display ]] && break
    sleep 0.1
  done
  [[ -s $work/display ]] || { echo "release: Xvfb did not start" >&2; cat "$work/xvfb.log" >&2; return 2; }
  export DISPLAY=:$(head -n1 "$work/display")
}
