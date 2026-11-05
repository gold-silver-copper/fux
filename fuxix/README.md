# fuxix

The system calls [fux](https://github.com/gold-silver-copper/fux) makes on
Linux, Android and macOS, over `libc`, each behind a safe function. It
holds only what fux's own crates call, and the one clock fux-bench times
with. On Windows it has the types every OS shares (`Pid`, `Signal`'s
names, `Status`, `Errno`, `Allocation`), the event loop (`poll`,
`signal::Pipe`, `io`'s reads and writes) and `socket`'s named pipes (both
below), over `windows-sys`; the
other modules are Unix only for now. Each OS's code sits in its own file
(`poll/unix.rs`, `poll/windows.rs`, and so on). On any other platform it
does not build.

## Rules

- fux forbids `unsafe_code`; the `unsafe` it needs lives here, one call per
  `unsafe` block, each with a `SAFETY` comment.
- No returned value has to be checked for validity: a process ID, group or
  session is a `Pid`, which is positive; a failure is an `Errno` or `None`;
  a struct the kernel fills is read only after the call says it filled it.
- No function retries a call a signal interrupted: it fails with
  `Errno::INTR`, and the caller decides. Two retry: `io::write_what_fits`,
  which is asked to, and `terminal::Raw`'s drop, which has no caller to
  tell.
- `Errno::is_peer_gone` says `EPIPE` or `ECONNRESET`: the other end of a
  pipe or socket went.

## `pty::open` on macOS

The one function that retries, though not for signals. It works around
two macOS kernel bugs that strike when PTYs are allocated and freed
quickly, by any processes (see [the report for Apple](https://github.com/gold-silver-copper/fux/blob/752cffed/docs/apple-feedback-ptmx-eredriveopen.md)):

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

## The end of a program on a PTY

The same on both systems: once `process::ended` (or `reap`) says the
program has ended, read the master until it says `AGAIN`, or that it is
closed (0, or `IO`); that is everything it wrote. Waiting for the master
to close does not tell the end: on Linux it closes once every holder of
the slave has closed it, which a job the program left may never do, and
which a program whose stdin is elsewhere may do before it exits; on
macOS, where `Master` holds the slave, it may not close at all. A poll
loop learns of the end from a `signal::Pipe` of `Chld`.

## The event loop on Windows

The same `Poll`, `List`, `Entry`, `PollFd`, `Events`, `poll`,
`signal::Pipe` and `io::read`, `write` and `write_what_fits` as on Unix,
over one I/O completion port for the whole process, which no thread of
fuxix's own waits on. A Windows handle can't be asked whether it is ready,
so `PollFd::new` takes a `poll::Source`, a sealed trait only fuxix's
`io::Stream` and `signal::Pipe` have, and readiness is rebuilt from what
finished:

- `io::Stream`, one end of a pipe (`io::pipe` for two ends in this
  process, `io::pipe_for_child` for one whose other end is a plain handle
  for a child or a pseudo console), keeps one overlapped read in flight
  into a 64 KiB buffer of its own. `IN`: a read finished, with bytes, the
  end or an error. `OUT`: no write is in flight; `write` copies into a
  second buffer and starts one. `HUP`: the read came to the end with
  nothing left, or a write found no reader; `ERR`: another failure. A
  read or write that would wait fails with `Errno::AGAIN`
  (`WSAEWOULDBLOCK`, what std calls `WouldBlock`). Dropped, a stream
  cancels its read and closes at once, or once a write in flight lands.
- `signal::Pipe` is a queue any thread may add to (`signal::raise`), the
  console control handler (`Int` for Ctrl-C, `Quit` for Ctrl-Break, `Hup`
  for close, `Term` for logoff and shutdown) and a watched child's end
  (`process::watch`, a `RegisterWaitForSingleObject` wait, raising `Chld`).
  Each raise posts a packet; a signal waiting is not held twice.
- A timeout is a high-resolution waitable timer, with a thread-pool wait
  posting a packet when it fires: the port's own millisecond timeout ends
  a 1 ms wait 13 to 15 ms late.
- One thread polls at a time: another thread's poll meanwhile fails with
  `ERROR_BUSY`, rather than take the packet the first waits for.

## Named pipes on Windows

`socket` is where a server's clients connect, and who is at the other end.
On both OSes a connection's `socket::peer` is a `Peer`: its `User`, which
says whether it is this process's own (`User::is_current`), and its
process ID where the system says it. On Unix a `User` is an effective
user ID ("uid 1000"); on Windows it is a SID and whether the process's
token is elevated ("S-1-5-21-…-1001 (elevated)"), so an elevated and a
normal process of one account are two users, and refuse each other.

On Windows the transport is one named pipe instance per connection:

- `socket::name(base)` is `\\.\pipe\<base>-<SID>`, with `-elevated` after
  it for an elevated process: each user, elevated or not, has its own.
- `Listener::bind` makes the pipe's first instance with
  `FILE_FLAG_FIRST_PIPE_INSTANCE`, `PIPE_REJECT_REMOTE_CLIENTS` and an
  explicit owner-only descriptor, `D:P(A;;GA;;;<user SID>)` (the default
  one lets any user open a pipe to read). A name any process holds
  already fails with `Errno::ADDRINUSE`, never joining it.
- A `Listener` keeps one instance waiting, a `Spare`, a `poll::Source`
  ready to read once a client connected: an overlapped
  `ConnectNamedPipe` signals an event that a thread-pool wait turns into a
  packet. `accept` gives a `Connection` and `spare` gives the waiting
  `Spare`, each only once the next instance is made, so the name is never
  free while the listener lives.
- A `Spare` or a `Connection` is bound to no completion port until it is
  made a `Stream` (`Connection::stream`), so it can be handed to a child
  as a plain handle (`OwnedHandle::try_from`, `OwnedHandle::from`, then
  std's `Stdio::from`, which makes it inheritable only in the copy for
  that child's spawn). The child makes it a `Spare` or `Connection` again
  and binds it to its own port.
- `socket::peer` reads the pipe's client pid on a server's end and its
  server pid on a client's (`GetNamedPipeClientProcessId`,
  `GetNamedPipeServerProcessId`), then that process's token (`TokenUser`,
  `TokenElevation`). A process that can't be read is an error, a refusal.
  The server pid is the process that made the instance: a client of a
  child given a spare still reads the server's.
- `socket::connect` opens the pipe (identification only: the server can't
  act as the client), and refuses with `Errno::ACCES` a server that is not
  this process's user, before anything is sent. Every instance taken is
  `Errno::AGAIN`.

## Functions

| Function | Linux and Android | macOS | Why not std |
| --- | --- | --- | --- |
| `io::read`, `io::write` | `read`, `write` | the same | on a borrowed descriptor, with `AGAIN` and `INTR` as values |
| `io::write_what_fits` | `write` again till all is written or it says `AGAIN`, again after `INTR` | the same | not offered on a bare descriptor |
| `io::set_nonblocking` | `fcntl(F_SETFL, O_NONBLOCK)` | the same | not offered on a bare descriptor |
| `io::redirect_stdout` | `dup2(fd, 1)` | the same | not offered |
| `signal::Pipe` | `sigaction(SA_RESTART)`, a handler that writes the signal's number as a byte to each non-blocking pipe held for it (64 at most), saving `errno`: one pipe for several signals, told apart by `Pipe::next` | the same | not offered |
| `poll::poll`, `Poll::list` | `ppoll`, with an exact timeout | `poll`, the timeout rounded up to a millisecond | not offered |
| `process::kill`, `kill_group` | `kill`, `killpg` | the same | std signals only its own children, and only with SIGKILL |
| `process::geteuid`, `session` | `geteuid`, `getsid` | the same | not offered |
| `process::lead_session`, `pty::Slave::spawn` | before `fork`, the highest descriptor in `/dev/fd`; between `fork` and `exec` (`pre_exec`): every descriptor from 3 up marked close-on-exec (`close_range(CLOSE_RANGE_CLOEXEC)`, else `fcntl` on each up to past those listed; Android: always the latter), `sigaction` of every signal to `SIG_DFL` (`Dispositions::Default`) or of each ignored before a `signal::Pipe` caught it to `SIG_IGN` (`Inherited`), `sigprocmask` to block none, `setsid`, `TIOCSCTTY` | the same, by `fcntl` on each up to past those listed | std's spawn makes no session |
| `process::thread_cpu_time` (for fux-bench) | `clock_gettime(CLOCK_THREAD_CPUTIME_ID)` | the same | not offered |
| `process::ended` | `waitid(WEXITED, WNOHANG, WNOWAIT)`: an exit's code, or a kill's signal and whether it dumped core (`CLD_DUMPED`) | the same, ignoring the stops macOS reports | std's `try_wait` reaps |
| `process::reap` | `waitid(WEXITED, WNOHANG)` | the same, ignoring the stops macOS reports | for a pid, not a `Child` |
| `process::adopt_orphans`, `reap_orphans` | `prctl(PR_SET_CHILD_SUBREAPER)`; `waitid(WEXITED, WNOHANG)` on each child in `/proc/self/task/*/children` not kept, saying each reaped and how it ended | nothing: macOS has no subreaper, so orphans go to launchd | not offered |
| `process::tree` | this process's descendants, by `/proc/PID/task/*/children` | this process's descendants, by `proc_listchildpids`; an orphan leaves for launchd | not offered |
| `process::Job` (from `pty::Slave::spawn`): `members`, `signal`, `ended`, `reap` | a program leading a new session and group; its members are the processes in `tree` whose `getsid` is its; `signal` is `killpg` and, with `Reach::Session`, `kill` of each member found first; `ended` and `reap` as `process::ended` and `reap` | the members are the processes `proc_listallpids` lists whose `getsid` is its (macOS lists no session alone), wherever they are; the rest the same | not offered |
| `process::cwd_observed` | `/proc/PID/cwd` | `proc_pidinfo(PROC_PIDVNODEPATHINFO)` | not offered |
| `pty::open` | `posix_openpt(O_CLOEXEC)`, `grantpt`, `unlockpt`, `ptsname_r` | `posix_openpt(O_CLOEXEC)` (retried as above; marked close-on-exec after if a release refuses the flag), `TIOCPTYGNAME` and a check of the replica, `grantpt` under a watchdog, `unlockpt` | not offered |
| `terminal::attributes`, `set_attributes`, `Termios::make_raw`, `edits_lines`, `echoes`, `end_of_file`, `newline_as_crlf`, `set_newline_as_crlf` | `tcgetattr`, `tcsetattr(TCSANOW)`, `cfmakeraw`; `ICANON`, `ECHO`, `VEOF`, `OPOST` and `ONLCR` | the same | not offered |
| `terminal::Raw` | `tcsetattr` to raw mode, and back once dropped or, for every live one, from a panic hook it chains, before a panic's message: `TCSANOW` (`enter`) or `TCSADRAIN` (`enter_draining`) | the same | not offered |
| `terminal::window_size`, `set_window_size` | `TIOCGWINSZ`, `TIOCSWINSZ` | the same | not offered |
| `terminal::foreground_group` | `tcgetpgrp` | the same | not offered |
| `socket::stream`, `bind`, `listen`, `connect` | `socket(SOCK_CLOEXEC)`, `bind`, `listen`, `connect` | `socket`, then close-on-exec | `UnixListener::bind` binds and listens in one step; fux sets the socket's mode between them |
| `socket::peer` | `SO_PEERCRED` | `getpeereid` | `peer_cred` is unstable |
| `file::pin` | `open(O_PATH \| O_NOFOLLOW)` | none | `O_PATH` has no name in std |

`terminal::Size` is a size in cells, never no rows or no columns:
`Size::new(rows, cols)` gives one if neither is zero.
A `poll::List`'s `push` gives an `Entry`, which `events` asks what that
descriptor was found ready for, wherever it is in the list.
`file::NOFOLLOW` is `O_NOFOLLOW`, for `OpenOptions::custom_flags`.
`socket::max_path_len` is the longest path a socket address holds: its
`sun_path` less the NUL (107 bytes on Linux and Android, 103 on macOS).
`process::Pid::of` is the `Pid` of a `std::process::Child`.
`process::Signal` is the signals fuxix sends and those a program dies of,
each found by its number (`from_raw`), and giving it and its name back
(`raw`, `name`, `TERM`).
