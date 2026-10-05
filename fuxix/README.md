# fuxix

The system calls [fux](https://github.com/gold-silver-copper/fux) makes on
Linux, Android and macOS, over `libc`, each behind a safe function. It
holds only what fux and the tools in its repository use. On any other
platform it does not build.

## Rules

- fux forbids `unsafe_code`; the `unsafe` it needs lives here, one call per
  `unsafe` block, each with a `SAFETY` comment.
- No returned value has to be checked for validity: a process ID, group or
  session is a `Pid`, which is positive; a failure is an `Errno` or `None`;
  a struct the kernel fills is read only after the call says it filled it.
- No function retries a call a signal interrupted: it fails with
  `Errno::INTR`, and the caller decides.

## `pty::open` on macOS

The one function that retries, though not for signals. It works around
two macOS kernel bugs that strike when PTYs are allocated and freed
quickly, by any processes (see [the report for Apple](https://github.com/gold-silver-copper/fux/blob/main/docs/apple-feedback-ptmx-eredriveopen.md)):

- `posix_openpt` can fail with errno -6, the kernel-private
  `EREDRIVEOPEN`, or with `ENXIO` after losing a race with another
  process's close. fuxix opens one master at a time in the process and
  tries up to 8 times; then `EREDRIVEOPEN` becomes `AGAIN`, and
  `ENXIO` (which also means every PTY is in use) stays `ENXIO`.
- A master can open with no replica node in `/dev`, and `grantpt` on it
  then never returns. fuxix checks for the replica first and watches
  `grantpt`: after 1 s a watchdog thread replaces the master with
  `/dev/null`, so the call returns. Either way the PTY is dropped and
  another opened, up to 4 in all; then the error is `AGAIN`.

## Functions

| Function | Linux and Android | macOS | Why not std |
| --- | --- | --- | --- |
| `io::read`, `io::write` | `read`, `write` | the same | on a borrowed descriptor, with `AGAIN` and `INTR` as values |
| `io::set_nonblocking` | `fcntl(F_SETFL, O_NONBLOCK)` | the same | not offered on a bare descriptor |
| `io::set_cloexec` | `fcntl(F_SETFD, FD_CLOEXEC)` | the same | not offered |
| `io::cloexec_from` | `close_range(CLOSE_RANGE_CLOEXEC)`, else `/proc/self/fd` (Android: always) | `/dev/fd` | not offered |
| `io::duplicate_inheritable` | `dup` | the same | std's copies are close-on-exec |
| `poll::poll` | `ppoll`, with an exact timeout | `poll`, the timeout rounded up to a millisecond | not offered |
| `process::kill`, `kill_group`, `exists` | `kill`, `killpg`, `kill(pid, 0)` | the same | std signals only its own children, and only with SIGKILL |
| `process::geteuid`, `setsid`, `session` | `geteuid`, `setsid`, `getsid` | the same | not offered |
| `process::thread_cpu_time` | `clock_gettime(CLOCK_THREAD_CPUTIME_ID)` | the same | not offered |
| `process::ended` | `waitid(WEXITED, WNOHANG, WNOWAIT)` | the same, ignoring the stops macOS reports | std's `try_wait` reaps |
| `process::reap` | `waitpid(WNOHANG)` | the same | for a pid, not a `Child` |
| `process::processes` | `/proc` | `proc_listallpids` | not offered |
| `process::cwd` | `/proc/PID/cwd` | `proc_pidinfo(PROC_PIDVNODEPATHINFO)` | not offered |
| `pty::open` | `posix_openpt(O_CLOEXEC)`, `grantpt`, `unlockpt`, `ptsname_r` | `posix_openpt(O_CLOEXEC)` (retried as above; marked close-on-exec after if a release refuses the flag), `TIOCPTYGNAME` and a check of the replica, `grantpt` under a watchdog, `unlockpt` | not offered |
| `terminal::attributes`, `set_attributes`, `Termios::make_raw`, `echoes`, `line_mode` | `tcgetattr`, `tcsetattr`, `cfmakeraw`, `ECHO` and `ICANON` in `c_lflag` | the same | not offered |
| `terminal::window_size`, `set_window_size` | `TIOCGWINSZ`, `TIOCSWINSZ` | the same | not offered |
| `terminal::foreground_group`, `make_controlling` | `tcgetpgrp`, `TIOCSCTTY` | the same | not offered |
| `socket::stream`, `bind`, `listen`, `connect` | `socket(SOCK_CLOEXEC)`, `bind`, `listen`, `connect` | `socket`, then close-on-exec | `UnixListener::bind` binds and listens in one step; fux sets the socket's mode between them |
| `socket::peer_uid` | `SO_PEERCRED` | `getpeereid` | `peer_cred` is unstable |
| `file::pin` | `open(O_PATH \| O_NOFOLLOW)` | none | `O_PATH` has no name in std |

`file::NOFOLLOW` is `O_NOFOLLOW`, for `OpenOptions::custom_flags`.
