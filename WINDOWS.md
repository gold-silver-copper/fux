# fux on Windows 11: handover

The plan for porting fux, fuxix and fux-vt to Windows 11, from a study run on 2026-11-04. Nothing here
is built yet. This file says what was decided, what was measured, the order of work, and the rules the
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

## Order of work (one PR each)
Each PR keeps the Linux and macOS CI jobs green and adds its checks to a Windows job running on
`windows-11-arm` (Windows 11 25H2) and `windows-2025` (Server 2025, the Windows 11 24H2 code base). The 30
programs built on the libraries (below) are the acceptance test for the whole port.
1. **Windows CI and the 32-bit fux-vt fix.** A CI job that builds and tests fux-vt, fuxth and fuxth-core on
   both images; `fux-vt/src/link.rs:141`'s size assert holds only on 64-bit (gate it on pointer width); a
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
8. **The client and fuxth-run on Windows:** console VT input and output, resize events, restore. Done when:
   `fux` runs in Windows Terminal on a Windows 11 machine and the dogfood programs pass.

## Rules the spikes found (each cost a failed run)
- **ConPTY**
  - Load `conpty.dll` only from fux's own data folder, never through the search path.
  - Spawn with `CreateProcessW` and the pseudo console attribute (not by starting conhost), so the pane
    process keeps the program's handle and exit code.
  - Set `STARTF_USESTDHANDLES` with invalid handles, or the program writes into the parent's output.
  - Never set `PSEUDOCONSOLE_INHERIT_CURSOR` (it stalls the program waiting for a cursor answer).
  - End rule: call `ReleasePseudoConsole` right after spawning, read until the pipe breaks (1–5 ms after the
    program exits), then `GetExitCodeProcess`.
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
  - Listing a console's processes means attaching to it: re-register the Ctrl-Break handler right after
    attaching (or list from a helper), or a Ctrl-Break kills the pane process.
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
- A real Windows 11 desktop: Windows Terminal, Defender on, a standard user, UAC. Every number is from
  GitHub runners (admin, Defender off). Windows 11 22H2 and 23H2 aren't available as runners.
- Corporate lockdowns (AppLocker, WDAC) blocking the downloaded `OpenConsole.exe`.
- Memory per empty pane: each pane adds an `OpenConsole.exe`, likely over the 1 MiB Unix figure.
- `cargo install` while a run is alive: the kept executable must be a copy, not a hard link.
- `kill <server pid>` looks like a crash on Windows (takeover follows); `fux kill-server` stops cleanly.
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
