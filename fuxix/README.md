# fuxix

The system calls [fux](https://github.com/gold-silver-copper/fux) makes, on
Linux, Android and macOS, over `libc`, each behind a safe function. It holds
only what fux, its tests, its fuzz targets and its walk use.

fux itself forbids `unsafe_code`; this crate is where the `unsafe` it needs
lives, one call per `unsafe` block, each with its reasoning. No function
returns a value its caller has to check for validity: a process ID, group or
session is a `Pid`, which is positive; a failure is an `Errno` or `None`; a
struct the kernel fills is read only after the call says it filled it.

No function retries a call a signal interrupted: it fails with
`Errno::INTR`, as the call did, and the caller decides.

| Function | Linux and Android | macOS | Why not std |
| --- | --- | --- | --- |
| `io::read`, `io::write` | `read`, `write` | the same | on a borrowed descriptor, with `AGAIN` and `INTR` as values |
| `io::set_nonblocking` | `fcntl(F_SETFL, O_NONBLOCK)` | the same | not offered on a bare descriptor |
| `io::set_cloexec` | `fcntl(F_SETFD, FD_CLOEXEC)` | the same | not offered |
| `io::cloexec_from` | `close_range(CLOSE_RANGE_CLOEXEC)`, else `/proc/self/fd` (Android: always) | `/dev/fd` | not offered |
| `io::duplicate_inheritable` | `dup` | the same | std's copies are close-on-exec, by design |
| `poll::poll` | `poll` | the same | not offered |
| `process::kill`, `kill_group`, `exists` | `kill`, `killpg`, `kill(pid, 0)` | the same | std signals only its own children, and only with SIGKILL |
| `process::geteuid`, `setsid`, `session` | `geteuid`, `setsid`, `getsid` | the same | not offered |
| `process::ended` | `waitid(WEXITED, WNOHANG, WNOWAIT)` | the same, ignoring the stops macOS reports | std's `try_wait` reaps |
| `process::reap` | `waitpid(WNOHANG)` | the same | for a pid, not a `Child` |
| `process::processes` | `/proc` | `proc_listallpids` | not offered |
| `process::cwd` | `/proc/PID/cwd` | `proc_pidinfo(PROC_PIDVNODEPATHINFO)` | not offered |
| `pty::open` | `posix_openpt(O_CLOEXEC)`, `grantpt`, `unlockpt`, `ptsname_r` | `posix_openpt`, then close-on-exec, `TIOCPTYGNAME` | not offered |
| `terminal::attributes`, `set_attributes`, `Termios::make_raw` | `tcgetattr`, `tcsetattr`, `cfmakeraw` | the same | not offered |
| `terminal::window_size`, `set_window_size` | `TIOCGWINSZ`, `TIOCSWINSZ` | the same | not offered |
| `terminal::foreground_group`, `make_controlling` | `tcgetpgrp`, `TIOCSCTTY` | the same | not offered |
| `socket::stream`, `bind`, `listen`, `connect` | `socket(SOCK_CLOEXEC)`, `bind`, `listen`, `connect` | `socket`, then close-on-exec | `UnixListener::bind` binds and listens in one step; fux sets the socket's mode between them |
| `socket::peer_uid` | `SO_PEERCRED` | `getpeereid` | `peer_cred` is unstable |
| `file::pin` | `open(O_PATH \| O_NOFOLLOW)` | none | `O_PATH` has no name in std |

It replaces `fux-sys`, which had the macOS calls, and rustix, which fux
used for the rest. Where rustix did not serve:

- `tcgetpgrp` and `getsid` built a process ID from their result unchecked
  on macOS, where 0 is undefined behaviour;
- `getsid` panicked on Linux for kernel threads, whose session is 0;
- `posix_openpt` and `socket` could not be close-on-exec on macOS without a
  second call, which it left to the caller;
- nothing marked every inherited descriptor close-on-exec.

On any other platform the crate does not build, and neither does fux.
