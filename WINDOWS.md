# fux on Windows 11: handover

The plan for porting fux, fuxix and fux-vt to Windows 11, from a study run on 2026-11-04. Nothing here
was built then; the port has since landed, PR by PR. This file says what was decided, what was measured, the order of work, and the rules the
spikes found the hard way. Whoever starts the port starts here.

## The owner's decisions (binding)
- fux, not only fuxix and fux-vt, works on Windows. **Windows 11 only.**
- Pane processes and crash takeover stay on Windows.
- Windows may behave slightly differently; where something can't be supported, fux falls back to
  something else and says so. Traits are welcome where they earn their place.
- **Nothing Linux or macOS does today changes.**
- Microsoft's separately shipped ConPTY (`conpty.dll` + `OpenConsole.exe`) is **fetched by fux on first
  use on Windows**: the pinned NuGet package `Microsoft.Windows.Console.ConPTY` (MIT), its hash checked,
  the two files kept in fux's data folder.
- The download goes through Windows' own `curl.exe` and `tar.exe` (Windows 11 ships both; D-82, lean on
  the OS), with the SHA-256 checked in Rust; offline or on failure, fux uses Windows' built-in ConPTY and
  says once what that loses (confirmed by the owner, 2026-11-04).

All of this is recorded as D-130; the design below as D-131 to D-138.

## The settled design (DECISIONS.md D-131 to D-138)
Settled by a six-seat panel (3 Claude Opus, 3 Codex) over two rounds; the two splits left after round 2
were settled by an experiment both sides agreed on beforehand (spike S5).
1. **The seam is a module switch.** Each fuxix concept keeps one public type with the same name on both
   OSes, backed by a per-OS file; the Unix files don't change. One small sealed trait, `poll::Source`
   ("something you can wait on"), covers the Windows types. No trait per concept. (6/6)
2. **Event loop:** fuxix's current API (`Poll`, `Entry`, `PollFd`, `signal::Pipe` with `Chld`) over one
   I/O completion port per process; only documented Windows waits; a high-resolution timer for short
   timeouts; a pane's pseudo console is closed on a short-lived thread, after its output is drained. One
   polling thread per process, so D-2's single-threaded server holds. (6/6; 5/6 on documented waits)
3. **A `Job` type on both OSes** for a pane's process tree: start contained, list members, ask whether any
   are left, end them with a grace period, reap. Unix: session and process group plus orphan adoption;
   Windows: a job object. Not built on today's macOS `process::tree()`. (6/6)
4. **No kill-on-close on a pane's job.** The pane process gives the server a copy of the program's process
   handle, so after a pane-process crash the server hangs the program up, waits the 100 ms grace, kills it
   and reports its real exit code, as Unix does (`fux-server/src/server.rs:913-923`,
   `fux-server/src/session.rs:1066-1075`); what the program started lives on, as on Unix. (5/6)
5. **Breakaway from a launcher's job is used when allowed, never silently;** when refused, fux reports that
   the run ends with that terminal instead of promising survival. (5/6)
6. **Signals on Windows** (Unix unchanged): INT → the 0x03 key; QUIT → the Ctrl-Break sequence; TERM on a
   shell pane → Ctrl-Break; KILL on a shell pane → every process on the pane's console except the shell,
   `only_shell` when the shell is alone; TERM/KILL on a program pane → terminate the pane's job with fux's own
   exit code; HUP, STOP, CONT, TSTP → the `signal` error. fux's own close: close the pseudo console, 100 ms
   grace, end the job. Exit status is the program's native exit code; "killed" only when fux's own
   terminate ended it. (6/6 on mechanisms; HUP 5/6; shell-pane TERM/KILL settled by S5)
7. **Facts:** `foreground_group` is always null; a pane's directory for splits is read from the shell's
   process (rules below), and a split is refused when it can't be read, as Unix refuses an unknown cwd;
   pixel sizes come only from fux-vt's in-band answers; an elevated and a normal fux of the same user are
   kept apart and refuse each other. (6/6; cwd settled by S5)
8. **Transport:** one named pipe per connection, owner-only security descriptor, the "first instance" flag,
   remote clients rejected, the peer's user checked from its process; the run record (pid + creation time)
   and a file lock decide takeover. (from S3)

## How fux behaves on Windows
| Unix today | Windows 11 |
|---|---|
| ^C, `signal INT` | the 0x03 key: every process on the pane's console gets Ctrl-C (Windows has no foreground group) |
| `signal QUIT` | the Ctrl-Break sequence |
| `signal TERM`, shell pane | Ctrl-Break: foreground and background jobs end, detached programs are spared, the shell lives |
| `signal KILL`, shell pane | every process on the pane's console except the shell; `only_shell` when it's alone |
| TERM/KILL, program pane | the pane's job is terminated with fux's own exit code |
| `signal HUP`, STOP, CONT, TSTP | refused with the `signal` error |
| closing a pane | close the pseudo console (programs get a close event), 100 ms grace, end the job |
| exit status | the native exit code; "killed" only when fux ended it |
| `foreground_group` | null |
| split directory | the shell's process directory; pwsh needs `[Environment]::CurrentDirectory = $PWD.ProviderPath` in its prompt (document it) |
| built-in ConPTY (offline) | works, with a one-time notice: no sixel or synchronized output, titles and links rewritten |
| `fux '(events (all) "")' \| head -1` | ends within 100 ms of the reader going, the pipe being asked; where it won't say, at the next event's write |
| a client killed outright (`taskkill /F`) | nothing can put the console back from inside: the shell after it finds raw modes (Unix's `kill -9` of a client leaves a raw terminal too) |

## Order of work (one PR each)
Each PR keeps the Linux and macOS CI jobs green and adds its checks to a Windows job running on
`windows-11-arm` (Windows 11 25H2) and `windows-2025` (Server 2025, the Windows 11 24H2 code base). The 30
programs built on the libraries (below) are the acceptance test for the whole port.
1. **Windows CI and the 32-bit fux-vt fix.** A CI job that builds and tests fux-vt and the language crates on
   both images (then fuxth; fux's language is now grift, and fuxth is in `archived/`); `fux-vt/src/link.rs:141`'s size assert holds only on 64-bit (gate it on pointer width); a
   wasm32 build check for fux-vt; `.gitattributes` so CRLF checkouts don't break byte-exact test data.
   Done when: the Windows job is green and fux-vt builds for wasm32.
2. **fuxix per module.** The "Linux, Android and macOS only" `compile_error!` (`fuxix/src/lib.rs:21-22`)
   moves to each module; portable `Pid`, `Signal`, `Status`, `Errno`, `Allocation`; fux-wire's five
   Windows errors fixed. Done when: fux-wire builds for Windows, and the Unix diff is types only.
3. **`Job` on Unix**, and macOS `process::tree()` stops listing every process on the system. Done when: the
   pane code uses `Job` with identical behaviour, and a macOS test shows `tree()` returns descendants only.
4. **The Windows event loop** (`Poll`, `signal::Pipe`, timers). Done when: the S2a/S2b probes pass as tests
   on both images (flood exact, no lost wake-up, idle at 0% CPU).
5. **Named-pipe IPC** with owner-only access and handoff to child processes. Done when: S3's access,
   handoff and squatting probes pass as tests, including a second, standard user refused.
6. **ConPTY and `Job` on Windows**, with the first-use download and the built-in fallback. Done when:
   S1's byte-exactness check passes through the downloaded ConPTY, and S4's signal probes pass.
7. **fux-server on Windows:** pane and connection processes, takeover, the signal mapping. Done when: S3's
   takeover probe passes against real fux (kill the server; a new one drives the live panes).
8. **The client on Windows** (and the fuxth runner, since archived): console VT input and output, resize events, restore. Done when:
   `fux` runs in Windows Terminal on a Windows 11 machine and the dogfood programs pass.

## The finish line, revised (PR 8)
- **Original target:** "`fux` runs in Windows Terminal on a Windows 11 machine and the dogfood programs
  pass."
- **Evidence:** the owner has no Windows machine, and no one has run fux on a person's desktop.
- **New target:** CI is the finish line, as real as a runner gets: the real client attached in a hosted
  ConPTY through `cmd.exe` on both images, and real `fux` in real Windows Terminal on the runner's
  desktop, typed at, with screenshots (see "What PR 8 found").
- **Cost:** what only a person can see stays unseen; the list is under "Not verified yet".

The dogfood half, measured: **29 of the 30 programs' own tests pass on both images, 1 doesn't build**
(run 42150709484, branch `dogfood/windows`, never merged). Original target: all pass. "Pass" means each
program's `cargo test` on Windows, and most needed changes first:

| Programs | What it took | What each gave up |
|---|---|---|
| 15 on fux-vt | No Windows change; 4 fixed for API drift since they were written, and `vt-graphics-preview` needs `Picture::pixel` public (on that branch only) | nothing |
| recorder, expect, PTY driver, line editor, two-pane mux, nested term | `Job`, `stdin_size`, tests rewritten for `cmd` | line editor: Ctrl-Z redraws; byte-exact output checks, as ConPTY repaints |
| dtach, shared shell, agent socket | Unix sockets to `fuxix::socket`, servers on `process::start` | the TERM cleanup test; the agent's `forbid_dumps` |
| PTY bridge | as above, on a pipe | **TCP** (fuxix can't wait on a socket) and a connection at a time (one poller per process) |
| unbuffer | runs whole on a console | **stdin staying ours**, and `\n` as written: ConPTY has no output-only terminal |
| supervisor, group timeout, task runner | `process::start`, `Job` on a console | **TERM and its grace**: Windows ends a process outright; Ctrl-C stops the supervisor; a runner's pipe task runs on a console |
| job top (`ptop`) | **doesn't build** (18 errors) | it reads each process's state, CPU, memory and directory from `/proc`, and fuxix has nothing for them on Windows |

Cost: `ptop`'s kind of program has no way in, and each bold line is a feature that isn't there. Gaps the
port found in fuxix's shared API, left as they are: `Slave::spawn` returns `(Child, Job)` on Unix and
`Job` on Windows; a `Job` exists only with a console; `terminal::attributes` has no Windows form.

## Open questions the later PRs settle (found in PR 2)
- **"Killed by fux" on Windows** (D-136): `Status` there is only `Exited(i32)`, the native code as std,
  cmd and PowerShell show it (`0xC000013A` is -1073741510). The PR that adds terminate decides how the
  server says fux ended a program: likely from the `Signal` it sent, which it knows, not from a number.
- **Errno names on Windows** (settled in PR 4): `Errno::AGAIN` is `WSAEWOULDBLOCK` (10035), the code
  std reads as `WouldBlock`; `is_peer_gone` is `ERROR_BROKEN_PIPE`, `ERROR_NO_DATA` or
  `ERROR_PIPE_NOT_CONNECTED`; a read's end is `ERROR_BROKEN_PIPE` or `ERROR_HANDLE_EOF`.
- **A Windows path that isn't text:** the wire carries Windows strings as their UTF-16 units.
  `docs/records.md`'s `((encoding "hex") (text ...))` is a Unix path's bytes, and on Windows a path's
  WTF-8 bytes (UTF-8, with a lone surrogate as its three bytes).

## What PR 6 builds on (found in PR 3)
- **`process::Job`** is mechanism only: `leader`, `members` (a `Tree`), `signal(Signal, Reach)` (a
  `Sent`: the members' scan and each failure), `ended`, `reap(self)` (the job back while it runs). When to
  hang up, how far (`Reach::Group` or `Reach::Session`) and what is logged is fux-server's (`Leader`).
  Windows adds its `Job` beside the Unix one in `process/windows.rs`, holding the job object and the
  program's process handle; `Tree`, `Scan`, `Sent` and `Reach` move to `process.rs` for both.
- A job is made only by `pty::Slave::spawn` (`Job::start` is crate-private), so Windows' ConPTY spawn
  makes its own. The server keeps a killed program as its `Job`, not a pid, till it is reaped.
- macOS has no list of one session's processes (`sysctl` `KERN_PROC_SESSION` is `ENOENT` on macOS 15), so
  `Job::members` there asks every listed process its session, privately; `tree()` lists descendants only.
- Orphan adoption stays process-wide (`adopt_orphans`, `reap_orphans`), outside `Job`.
- Two shapes PR 6 must settle (the PR 3 judges named them): `Job::signal` takes a Unix `Signal`, so on
  Windows only KILL (terminate the job with fux's exit code, D-136) is a job operation and the hangup is
  closing the pseudo console, not a signal; and `Slave::spawn` returns std's `Child`, which a
  `CreateProcessW` spawn doesn't have.

## What PR 5 and PR 6 build on (found in PR 4)
- The loop is `fuxix::poll` over one completion port per process (`poll/windows.rs`). Every source
  is a `poll::Source`, sealed: today `io::Stream` and `signal::Pipe`. A new kind of source (PR 5's
  listener) implements the crate-private `poll::sealed::Sealed`: `arm` starts what must be in flight
  (an overlapped `ConnectNamedPipe`), `ready` says what finished. A packet with `poll::KEY_IO` must
  carry an `OVERLAPPED` that is the first field of an `io::Op`; a listener needs its own key.
- A named-pipe connection is an `io::Stream`: `Stream::new` (crate-private) binds any overlapped
  handle to the port with `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`.
- `io::pipe` and `io::pipe_for_child` make an unnamed-style pair as std does (one instance, local
  clients only, the first-instance flag, default security). PR 5's IPC pipe needs the owner-only
  descriptor; these pairs don't, as their name is opened at once and a stranger who opens it first
  makes the pair fail.
- ConPTY's two pipes are two `io::pipe_for_child` (the handle goes to `CreatePseudoConsole`), and the
  program's end is `process::watch` on its process handle (it takes a `std::process::Child` today;
  PR 6's `CreateProcessW` spawn needs it to take a handle).
- **One thread polls at a time**, enforced: a second thread's poll fails with `ERROR_BUSY`. Tests that
  poll in one test binary take turns (`fuxix/tests/windows.rs`, `turn`), and so must fux-server's.
- Console close, logoff and shutdown are `Hup` and `Term` on the pipe, but Windows ends the process
  once the handler returns (or after 5 s): PR 7/8 decide whether the handler must wait for the loop.
- Measured on the runners (PR 4's CI, `fuxix/tests/windows.rs`, the same on 26200 arm64 and 26100
  x64): a 1 ms timeout never early, late by p50 0.52-0.55 ms, p99 0.59-0.60 ms (later runs on
  windows-11-arm now and then saw p99 3.7-3.9 ms, the runner's, so the test holds p50 < 2 ms and
  p99 < 10 ms); an idle loop 0 CPU ticks in 2 s (`GetProcessTimes` counts in ~15.6 ms ticks);
  4 MiB + 1 MiB both ways through one pipe with 500 signals, exact; 2000 raises from 4 threads, each
  seen; a child's 1 MiB and its `Chld`.

## What PR 8 found
- **A runner has a real desktop.** Both images log `runneradmin` on at the console at boot
  (`AutoAdminLogon`): session 2, `WinSta0\Default`, `explorer` and `dwm` running, 1024x768, and a CI step
  runs in that session. Windows Terminal starts there, takes real keystrokes and shows in a screenshot
  (runs 42142106849, 42142509065). On the arm image Windows' first-logon privacy screen (`WWAHost`) holds
  the foreground till it is ended.
- **Defender can be on:** `Set-MpPreference -DisableRealtimeMonitoring $false` turns real-time protection
  on (it is off on windows-2025, on on the arm image, whose tamper protection refuses
  service changes but not that), and an EICAR file written then is quarantined by "Real-Time Protection",
  so the scan is real. The ConPTY download and `OpenConsole.exe` pass with it on (run 42142106849).
- **`RegisterWaitForSingleObject` on a console input handle works**, on both images: the console needs no
  thread of ours.
- **Records under `ENABLE_VIRTUAL_TERMINAL_INPUT`:** a terminal's bytes come as key-downs of `vk=0` and
  the byte as the character; a terminal's NUL comes as Ctrl+Shift+2 with no character (`vk=0x32`,
  state `0x18`), not as Ctrl+Space; in-box conhost's emoji on `VK_MENU` key-ups (as S4 said).
- **A pane's program has invalid standard handles**, and `std` then reads nothing and writes nowhere,
  silently: `terminal::open_stdio` opens the console for each one missing, so `fux` in a pane works.
- **A Ctrl-Break reaches every process on the console:** a shell with no handler ends with it, and
  takes the client's shell down, as on any console.
- **What runs on Windows and what stays Unix's:** fux-server's and fux's unit tests run with
  `--test-threads 1` (one poller). Those that stay Unix's are `#[cfg(unix)]` with why: they run
  `/bin/sh` or `/bin/cat`, name `/` as an absolute directory, or stand a Unix socket or a FIFO in for a
  peer. The root `tests/*.rs` stay Unix's whole: `tests/support` drives a client over a nonblocking
  `UnixStream`, holds a `Slave::spawn`'s `Child`, asks `ps`, and every test scripts `/bin/sh`.
  `tests/windows.rs` and `tests/console.rs` stand in for them on Windows.
- **`\` is grift's escape in a string,** on Windows as on Unix: `"C:\work"` in a `.grift` file or a
  program on a command line doesn't read (`parse-error`, `bad-escape`). Double it (`"C:\\work"`), write it
  with `/` (`"C:/work"`), or pass it after the program as a shell argument, which stays a string as it
  is: `fux '(directory ...)' 'C:\work'` with `(first (arguments))`.
- **`fux` must not link `user32`:** Windows' documentation says such a process isn't sent logoff or
  shutdown events (from the docs, not tested). Nothing here does.
- **Restoring on a panic can't come first:** `LEAVE` needs VT processing still on, or a console without
  it prints it as text. So the panic hook writes `LEAVE` on a thread waited for 250 ms at most, then
  restores: a console that stopped reading delays restoring by that, and no more.

## What PR 8 builds on (found in PR 7)
- **fux-server's OS seam is one module switch**, `socket.rs` with `socket/unix.rs` (today's code,
  moved) and `socket/windows.rs`: where a server is found and kept private, and how fux's own
  processes are started and linked (`connect`, `accept`, `peer`, `launch_conn`, `start_pane`,
  `pane_listener`, `pane_first`, `reach_named`, `taken_stdio`, `run_pinned`, `keep_executable`,
  `prepare_terminals`). The rest of fux-server is shared; its unit tests are `cfg(all(test, unix))`
  because they drive sessions through fux's language (`fux::script`), which is Unix-only until PR 8.
- **Who a pipe's server is:** `GetNamedPipeServerProcessId` names the process that made the pipe's
  *first* instance, even on a later instance another process made (CI run 42117571201, both
  images), not the instance's maker as PR 5's notes say. So a pipe that must name a process after
  its maker dies is first made by that process: a pane binds its own pipe and says so on its stdout
  (`socket::start_pane`), never one its server made and handed over.
- **The client is not built on Windows**: `src/client.rs`, `script`, `screens`, `keymap`,
  `client_state` and `help` are `cfg(unix)`; the binary runs `__serve`, `__pane PATH` and
  `__conn taken`, and refuses anything else once. PR 8 removes those gates. The root `tests/*.rs`
  (they drive the client) are `#![cfg(unix)]` but `tests/windows.rs`, a small wire client.
- **Names on Windows:** the socket path names a pipe, `\\.\pipe\fux-<path, \ as />-<SID>[-elevated]`
  (`socket::pipe_name`); no file is at the path. Beside it: `.starting` and `.lock` (opened with no
  delete sharing), `.server` (the server's pid and creation time, D-138), and `PATH.panes\`, where
  each pane has an empty marker file `N` naming its pipe. The default socket is
  `%LOCALAPPDATA%\fux\server.sock`, and `%LOCALAPPDATA%\fux-elevated\server.sock` for an elevated
  process (D-137): one shared folder can't work, as each mode needs the folder owned by its own
  default owner, which for a split-token administrator are the user and the Administrators group. The
  pinned ConPTY is `conpty` in the same folder.
- **Private folders:** `fuxix::file::private_dir` grants only the token's default owner, so an
  elevated folder is closed to the same user's normal processes. `fuxix::file::is_private_dir` is
  Unix's 0700 check: real (no reparse point), owned by the default owner, and no allowing entry but
  that owner's, SYSTEM's and Administrators' (who can't be kept out) and an inherit-only
  `CREATOR OWNER`. The socket's folder and `PATH.panes\` pass it before a record or `exe` there is
  read or run; fux's own folder passes it before a ConPTY is loaded from it.
- **A client's checks were PR 8's** (written since; `listened` stays Unix's): before it, `check_client_socket`,
  `listened` and `owned` existed only on Unix. On Windows `fuxix::socket::connect` already refuses a pipe of another user or owner; a
  client must not read `Errno::AGAIN` (every instance busy) as "a server runs": a squatter with
  busy instances gives it too. The server's `.lock` and `.server` record are what says one runs.
- **Starting a server:** a client starts `fux __serve SOCKET ENTRIES` and reads `READY` on its
  stdout. A server whose run is recorded starts the run's pinned copy (`run_pinned`: started with
  the same three handles, then this process exits 0, before taking any lock) and the copy writes
  `READY`. Notices before `READY` go to the server's log (`notice: ...`): the built-in ConPTY's
  losses, and a launcher job that binds the run (`process::job_fate`, D-135). PR 8's client should
  print them.
- **No spare connection process on Windows** (`keep_spare` is Linux's): the server takes every
  client and starts `fux __conn taken` with the client's instance on stdout and its link on stdin
  (`io::pipe_for_fuxix_child`, both overlapped), handed through a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
  of exactly those handles (`process::start`).
- **D-134:** a pane process sends `FromPane::Offer` (its program's pid, creation time and handle
  value) before each new program's facts and again to a server taking it back; the server copies
  the handle from the pane process (the pid it started, or the greeting's, checked against the
  pipe's maker) and checks pid and creation time (`process::adopt`). The program is not held
  suspended until then: a pane process that dies in the milliseconds before its server copies the
  handle leaves that program unreaped by the server (it gets the console's close event).
- **Signals:** `process::delivery(signal, shell)` is the D-136 table (Unix: always `Group`), used in
  `host.rs`. "Killed" is not on the wire: an end by fux's kill is `Exited(KILLED)`.
- **Reads stay armed:** the server's held links, the server's link in a client whose terminal has
  no room, and `__conn`'s client while an answer is on its way (D-158) are not read, and are polled on Windows for `HUP | ERR`, which keeps a read in flight and
  sets what it reads aside (256 KiB at most, the read buffer included), so the peer's end is told past
  bytes not yet taken. A peer that filled the bound and its pipe before it ended is seen only once it
  is read. On Unix they are polled for nothing (`HUP` would not do, as macOS's `poll` watches a
  descriptor asked for it and not one asked for nothing): Linux tells a hang-up unasked, macOS
  nothing till they are read again. Each use says so itself; there is no `Events::WATCH`, which was
  empty on Unix, so a test against it was never true. On Windows `Events::NONE` tells
  nothing at all, so a loop done with a stream doesn't spin on its end.
- **The end rule:** after `Chld` a pane keeps polling its master (`fuxix::pty::SETTLE`, 100 ms; none
  on Unix) until the pipe breaks; with the built-in ConPTY, which doesn't break it, the time running
  out ends the drain and dropping the master closes the console.
- **Not done here:** console close/logoff handlers that wait for the loop (a detached server has no
  console; a hand-typed one stops as a crash would, and is taken back); the suspended start above.

## What PR 7 builds on (found in PR 5)
- `fuxix::socket` is one module on both OSes. Its `Peer { user, pid }` and `User` are shared names:
  a uid on Unix, a SID plus "token elevated" on Windows (D-137: elevation, not the integrity level,
  as that is the fact D-137 names; a low-integrity process of the user should already be kept out by
  the default no-write-up label, not tested). "Is this my user?" is `peer.user.is_current()`, and fux-server's
  refusal is `socket::stranger(…)` on both, written once. On Windows `peer` takes a second argument, a
  moment the end is known to have been connected at: `socket::peer(&connection, connection.connected()?)`.
- The pipe name is `socket::name(base)`: `\\.\pipe\<base>-<SID>[-elevated]`. PR 7 picks `base`
  (from the socket path's name, say) and keeps the run record and lock beside it; takeover is PR 7's.
- `socket::Listener::bind` fails with `Errno::ADDRINUSE` when any process holds the name (another
  server or a squatter): the server then reads the run record, as Unix reads its socket.
- `Listener::accept` gives a `Connection` (to serve: `Connection::stream`; for `fux __conn taken`:
  `OwnedHandle::from` then `Stdio::from`); `Listener::spare` gives the waiting `Spare` for
  `fux __conn` (`OwnedHandle::try_from`). In the child, `Spare::try_from(handle)` or
  `Connection::from(handle)`; both bind to the child's own port only when made a stream. Pass the
  handle as a stdio the child's runtime doesn't write (Rust's test harness prints to stdout; fux's
  `__conn` already moves its stdout aside, `conn::wait`).
- Every instance takes a client: while a spare waits, the listener's own next instance can take one
  too. On Unix the server stops accepting while a spare waits (`FromConn::Waiting`); on Windows it
  must keep polling its listener and hand such a client to a `__conn taken`.
- `socket::connect` checks the server's user before returning, and the pipe's owner against this
  process's default owner (`TokenOwner`; Administrators for an elevated admin), which a squatter of
  another user can't forge even should the server's pid be reused. It fails with `Errno::ACCES` (the
  descriptor refused it, or the server is another user or elevation, or the owner differs),
  `Errno::INVAL` (a NUL in the name) or `Errno::AGAIN` (every instance still busy after a second of
  `WaitNamedPipeW`). `peer` refuses a peer process made after that moment, as the run record binds a
  pid to its creation time: such a process took the pid of a peer that is gone. The moment is when
  the server first saw the connect settle, when `connect` opened the client's end, and, in a child
  handed a `Connection`, the child's own creation time (its client connected before the handoff). A
  pid reused before that moment still passes: the window is the time from the peer's exit to the
  moment, and the clock's granularity. A peer gone with nothing left to read is refused as well.
- A failed connect makes the listener ready to read as well as `ERR`, so a server polling for `IN`
  calls `accept`, which tells the error and makes the next instance. A client that came and went
  before the connect saw it is no client: the instance is disconnected and waits again. A connection's server pid is the
  instance's maker, so a client still reads the server's pid after a handoff, as on Unix.
- Not tested on CI: an elevated and a normal process of the same user (the runner's account has no
  split token); the check is the same comparison the second-user test exercises.
- A handed instance goes through std's `Stdio::from`: std (1.95, `sys/process/windows.rs`) makes the
  inheritable copy only under its spawn lock, so no other std spawn inherits it. It does not pass a
  handle list (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), so the child also inherits any other handle
  made inheritable; fuxix makes none. PR 6's own spawn should add that list (below).
  PR 7's handoff should not rely on std there: it should spawn with a
  `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` of exactly the handed instance (portable-pty's
  `src/win/procthreadattr.rs`), so nothing else made inheritable meanwhile leaks into the child.
- A source polled with `Events::NONE` never arms a read, so a stream's `HUP` and `ERR` (learned only
  from a read or write that ends) are never told for it. What is not read is polled for
  `HUP | ERR`.
- A stream dropped with its read in flight cancels that read, but the read's 64 KiB buffer is freed
  only when some later poll takes the cancelled read's packet. A server that drops many streams and
  stops polling holds that memory until it polls again.
- For PR 6, from portable-pty 0.9 (`win/psuedocon.rs`, `win/procthreadattr.rs`): spawn with
  `CreateProcessW(…, bInheritHandles = FALSE, EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT)`,
  an attribute list holding `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` (the `HPCON` value itself as
  `lpValue`), and `STARTF_USESTDHANDLES` with all three stdio `INVALID_HANDLE_VALUE`, so the program
  never inherits the server's own redirected stdio; close `hThread` at once. Not to copy: its
  `Vec::with_capacity` + `set_len` for the list (uninitialised bytes; use `vec![0; n]`), and its
  first `InitializeProcThreadAttributeList` call's result going unchecked (it must fail with
  `ERROR_INSUFFICIENT_BUFFER`). If a ConPTY child must also inherit a handle (a `__conn` instance),
  use `bInheritHandles = TRUE` with a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` of exactly that handle.

## Rules the spikes found (each cost a failed run)
- **ConPTY**
  - Load `conpty.dll` only from fux's own data folder, never through the search path.
  - Spawn with `CreateProcessW` and the pseudo console attribute (not by starting conhost), so the pane
    process keeps the program's handle and exit code.
  - Set `STARTF_USESTDHANDLES` with invalid handles, or the program writes into the parent's output.
  - Never set `PSEUDOCONSOLE_INHERIT_CURSOR` (it stalls the program waiting for a cursor answer).
  - End rule: call `ReleasePseudoConsole` right after spawning, read until the pipe breaks (1–5 ms after the
    program exits), then `GetExitCodeProcess`. The built-in ConPTY of older Windows 11 has no
    `ReleasePseudoConsole`, and its pipe breaks only when the console is closed: the pane process (PR 7)
    also watches the program's process, and on its end drains for a bounded time (`pty::SETTLE`, written
    to or not: a program left writing doesn't hold the close off) and closes the console.
  - The bundled ConPTY sends `ESC[1t ESC[c ESC[?1004h ESC[?9001h` first and `ESC[?1004l ESC[?9001l` last:
    don't report those modes as the program's. fux-vt must answer DA1 and cursor-position queries quickly:
    an unanswered `ESC[6n` after a resize blocks the program for about 500 ms.
- **Event loop**
  - Close a dropped stream's handle at once (waiting for its last operation hung the peer).
  - A zero-timeout poll must still collect finished I/O.
  - Create a listener's next pipe instance before handing out a connected one.
  - The completion port's own timeout overshoots by about 15 ms: use a high-resolution waitable timer.
  - `ClosePseudoConsole` with unread output blocks for over 5 s on older builds and drops output on newer
    ones: drain first, close on a short-lived thread.
- **Signals and sessions**
  - A pane process started detached passes "ignore Ctrl-C" to every program: call
    `SetConsoleCtrlHandler(NULL, FALSE)` before spawning.
  - Closing the last handle of a kill-on-close job kills everything with exit code 0: never kill that way.
  - Console kills all exit `0xC000013A`; there are no signal numbers, so fux records what it sent.
  - Listing a console's processes means attaching to it: register the Ctrl-Break handler before attaching
    and again right after (or list from a helper), or a Ctrl-Break kills the pane process.
- **Pipes and access**
  - Default pipe security lets another user read: always pass an explicit owner-only descriptor.
  - Always create with `FILE_FLAG_FIRST_PIPE_INSTANCE`, or the server can silently join a squatter's pipe.
  - A mutex reporting "abandoned" is not a reliable crash signal; the run record's creation time is.
- **Reading a shell's directory**
  - Read it twice and treat a mismatch as failure (single reads tear under rapid changes).
  - Read the full stored length (a long path's length exceeds its recorded buffer size).
  - Use the 32-bit structure for 32-bit programs.
  - An elevated target is denied to a normal reader: refuse, don't guess.
- **Client console**
  - Use `ENABLE_VIRTUAL_TERMINAL_INPUT` with `ReadConsoleInputW`: it gives the exact key bytes and resize
    events in one stream. `ReadFile` breaks emoji and gives no resizes.
  - Emoji arrive on the Alt key-up record.
  - Save both console modes and restore them on every exit path, including a close handler.

## Not verified yet (each becomes a check in the PR that needs it)
- A person's Windows 11 desktop, as opposed to a runner's: a standard user with UAC, Windows Terminal's
  real key encoding (win32-input-mode) from a real keyboard, IME, AltGr, Win+`.` emoji, a font's width for
  wide emoji, mouse, drag-resize, closing a tab or window, logging off. Windows 11 22H2 and 23H2 aren't
  available as runners.
- `taskkill /F` of a client: `TerminateProcess` can't be restored from inside; what cmd and pwsh do with
  a console left in VT input is unseen.
- An elevated and a normal `fux` refusing each other (D-137): runners have no split token.
- Git Bash's mintty getting the "not a terminal" answer.
- Corporate lockdowns (AppLocker, WDAC) blocking the downloaded `OpenConsole.exe`.
- Memory per empty pane: each pane adds an `OpenConsole.exe`, likely over the 1 MiB Unix figure.
- `cargo install` while a run is alive: the kept executable must be a copy, not a hard link.
- `kill <server pid>` looks like a crash on Windows (takeover follows); `fux '(kill-server)'` stops cleanly.
  Windows' `timeout.exe` differs from GNU's, so agent recipes need a note.
- Helper processes (the download, console listing) must start without a console window.

## Evidence
The study's notes are in `~/code/notes/projects/fux/windows-2026-11-04/` on the owner's machine:
`recommendation.md` (the summary), `spikes/s1.md`–`s5.md` (each with run ids, quoted logs and rerun
commands), `panel/` (ballots, every answer, both tallies), `inventory.md` (what blocks the port, and every
Unix-dependent contract with its Windows counterpart), `prior-art/` (wezterm, alacritty, Microsoft's ConPTY,
crossterm and node-pty, Windows multiplexers), `premortem.md`, and `wave3/` (10 more programs on the
libraries and what they earned). The spike code is kept locally in `~/.cache/claude-tmp/win-port/s*/fux/spikes/`;
the GitHub Actions runs and their artifacts remain under the repository's Actions tab.

The 30 programs (`~/.cache/claude-tmp/vt-dogfood/`): 10 on fuxix (a recorder, expect, dtach, a supervisor,
unbuffer, a group timeout, a PTY driver, a line editor, a socket bridge, a shared shell), 10 on fux-vt (a
cast player, HTML screenshots, a log cleaner, a TUI test oracle, a screen diff, an ANSI art converter, a web
backend, recording search, a capture-pane dumper, an escape explainer), and wave 3's 10 (fux-vt in the
browser, an ssh-agent-like daemon, a two-pane mux, an accessibility reader, a sixel test-bed, a job top, a
cast compressor, a portable task runner, a nested terminal, an incremental renderer).
