# The Shape of zor

A small dedicated program that runs a shell or an agent in a pty, watches what the agent draws,
and announces the agent's state, **working**, **blocked**, **idle**, or **none**, in-band as an
escape sequence and out-of-band as event lines. It knows nothing about multiplexers. fux consumes
it; so does tmux, kitty, and plain koh on a phone.

- **Status:** proposal, audited against source (herdr 0.8.2, koh 0.11.0, vt100 0.16.2).
- **Date:** 3 Sep 2026
- **Name:** `zor`, from the Slavic root for sight (*vzor*, *zorkij*). Free on crates.io, 3 Sep 2026.
- **Relation to fux:** replaces the *Detection* section of `design.md`. fux spawns every pane
  through `zor` and reads its state OSC via koh's `take_unhandled_oscs()`. Detection code,
  rules, hysteresis, and fixtures leave fux entirely.

---

## Why a separate program

Detection is a pure function of one pane's byte stream plus its child process tree. It shares
nothing with layout, transport, or control. Kept inside fux it is useful only to fux users; as its
own binary it is useful the day it compiles:

- `zor -- claude` under tmux shows state in the window title through tmux's title passthrough.
- `koh connect` on Termux with `--on-bell` already notifies on the bell; with the wrapper the
  title carries the state glyph too, with no change to koh.
- A shell script can read the event line stream and do anything.

It also isolates the part of the system that changes most. Agents update their UIs every few
weeks; the rule set will churn. Releasing that churn on its own cadence keeps fux releases about
fux.

The cost is one extra process and one extra `vt100::Screen` per pane. vt100 is a few hundred
kilobytes of state at 200×50 and parses at memory speed; the process is a pty passthrough that
sleeps on two fds. Both are cheap enough that fux wraps every pane, not only agent panes, so
detection is a property of the pane and not of how the user typed the command.

---

## Surface

```sh
zor [options] [--] <command> [args…]    # run <command> in a pty; default: $SHELL -l
zor --events <path> …                   # also write event lines to a unix socket or fifo
zor --events - …                        # …or to fd 3 (stdout is the pty's)
zor --title never|prefix|replace …      # how to touch OSC 0/2 (default: prefix)
zor --no-osc …                          # never emit the state OSC (title only)
zor --rules <dir> …                     # extra rule files; later files win on the same agent
zor --agent <id> …                      # skip identification, force one rule set
zor --debug …                           # dump matched rules to stderr on each change
zor check <fixture.txt> [--agent id]    # evaluate one captured screen, print the verdict
zor agents                              # list the bundled rule sets and their versions
```

Everything not listed passes through untouched. The wrapper is transparent to the program inside:
same window size, same signals, same exit code, resize forwarded from `SIGWINCH`. Bytes from the
child reach stdout unchanged except for the OSC sequences the wrapper appends; bytes from stdin
reach the child unchanged.

---

## Architecture

```
  stdin ──▶ [pty master] ──▶ child (shell or agent)
                │
                └── child output ──┬──▶ stdout (byte-identical, plus state OSCs)
                                   │
                                   └──▶ vt100::Screen ──▶ regions ──▶ rules ──▶ raw verdict
                                                                                   │
                            /proc, sysctl ──▶ process tree ──▶ agent id ──▶ rule set
                                                                                   │
                                                                              hysteresis
                                                                                   │
                                                                  ┌────────────────┴──────────┐
                                                             state OSC + title           event lines
```

Four modules, each with one job and a test suite that does not need the others:

| Module | Job | Depends on |
|---|---|---|
| `pty` | spawn, passthrough, resize, exit status | portable-pty, libc |
| `screen` | one `vt100::Screen` plus title, bell count, OSC 9;4, per-drain change flag | vt100 |
| `rules` | region extraction over a `ScreenView`, rule evaluation, agent identification | regex, serde, toml |
| `state` | hysteresis machine, emitters | nothing |

koh's `terminal::ServerTerminal` already does what `screen` needs (callbacks for title, bell,
progress, unhandled OSC), but it is not on koh's stable surface and pulling koh pulls iroh. The
wrapper depends on vt100 directly and reimplements the eighty lines of callbacks. The
`ScreenView` trait is the same shape as koh's `predict::ScreenView` so a future shared crate is a
move, not a rewrite.

### Passthrough is the invariant

The wrapper's first job is to not be noticed. Output is written to stdout before it is parsed, in
the same chunks it arrived in; the parser runs on a copy after the write. Input is forwarded
byte-for-byte, including the terminal's responses to the child's queries (DA, DSR, XTGETTCAP),
which the wrapper never intercepts. The wrapper does not answer queries itself; the real terminal
does. If the child puts the terminal in raw mode or enables mouse reporting, the wrapper is not
involved: it put its own stdin in raw mode at start and passes everything.

The only bytes the wrapper adds are at chunk boundaries, never inside an escape sequence, because
vt100 tells the wrapper when the parser is between sequences. It emits at most one state OSC per
change, plus the modified title if `--title` is on. On exit it restores the terminal's title if it
touched it and prints nothing else.

### Identification: the process tree, not the command line

`zor -- claude` knows the agent. `zor` wrapping a shell does not, and must watch for one. On each
tick, the foreground process group of the pty (`tcgetpgrp` on the master) is resolved to its
processes (`/proc/<pid>/stat` on Linux, `proc_listpids` with `KERN_PROCARGS2` on macOS) and each
process name is normalised (`node /usr/bin/claude` is `claude`; a `.js` entry point's basename
counts) and matched against the agent ids and aliases of the loaded rule sets. Ties go to the
process group leader, then the deepest descendant. This is herdr's `identify_agent_in_job` model,
reimplemented.

Identification is polled, not evented, because there is no portable event for "the foreground
job changed". Cadence follows the state: 500 ms while no agent is identified, 5 s while one is
(agents do not change identity mid-run), and 100 ms for eight seconds after the shell prompt
regains the foreground, since the next agent launch is likely then. A missing agent is confirmed
six polls in a row before the state drops to **none**, so a brief subprocess (the agent shelling
out) does not flicker the indicator.

### Rules: regions, priorities, guards

Each agent has a rule set: a TOML file with an id, aliases, and an ordered list of rules. A rule
names a target state, a priority, a region, and a matcher. The highest-priority matching rule
wins; a rule with `state = "skip"` matches and vetoes the update, which is how a transcript
viewer or a settings screen (both look like nothing) is kept from flipping the state to idle.

Regions are computed lazily from the `ScreenView` on each evaluation:

| Region | Content |
|---|---|
| `title` | the OSC 0/2 title |
| `progress` | the OSC 9;4 state and percent, as `state:percent` text |
| `bottom(n)` | the last *n* rows |
| `bottom_non_empty(n)` | the last *n* rows that have any glyph |
| `whole` | every row |
| `prompt_box` | rows inside the last box-drawing frame that touches the bottom margin |
| `above_prompt_box` | rows above that frame |
| `last_line_above_prompt_box` | the last non-empty row above that frame |
| `after_last_rule` | rows after the last row that is a horizontal rule |
| `after_last_prompt_marker` | rows after the last row starting with the agent's prompt marker |

Matchers are `regex` (a list, any matches), `contains` (a list of substrings, all present), `any`
(a list of sub-matchers, one suffices), and `not` (a sub-matcher that must fail). `not` is the
negative guard: the Claude rule for **blocked** matches "Do you want to proceed?" in
`after_last_rule` but not when the same text is inside `prompt_box`, which is where a user typing
it would appear.

The region vocabulary, the priority ladder (title at 1100, skips at 1000, blocked at 980, working
between 965 and 975, idle at 950) and the guard idea come from studying herdr's manifests. The
schema, the region implementations and every rule file are written fresh from captured panes. No
herdr data is converted or vendored. See *Reference code is studied, not copied* in `design.md`.

### Hysteresis

Raw verdicts are noisy: a spinner frame is a row that changes, a prompt redraw briefly looks idle.
The state machine between verdict and emission:

- **working → idle** is confirmed by three consecutive idle verdicts spaced 100 ms apart, capped at
  700 ms after the first. A working verdict in between resets the count.
- **anything → blocked** and **anything → working** emit immediately. Waiting on a blocked prompt
  costs the user time; a false working costs nothing.
- **startup grace:** for three seconds after an agent is first identified, idle verdicts are not
  emitted. Agents draw their prompt before they start working on a queued argument.
- **none** is emitted when identification loses the agent (after the six-poll confirmation) and
  the screen has been quiet for two seconds.
- A stable state is re-announced every 800 ms only on the event channel, never in-band, so a
  consumer that attached late gets a value without waiting for a change.

These constants are herdr's, which spent months tuning them against real agents. They are the one
thing the wrapper takes from herdr as-is, as numbers, since there is no other way to arrive at
them than the same months.

---

## Output contracts

### In-band: the state OSC

```
ESC ] 7877 ; state=<working|blocked|idle|none> ; agent=<id> ; seq=<n> ST
```

- `7877` is not used by xterm, iTerm2, kitty, mintty, or ConEmu as far as their documentation
  shows; it sits above mintty's 7770 block and well away from iTerm2's 1337 and FinalTerm's 133.
  To be re-checked against each terminal's source before the first release.
- `state` is required. `agent` is present unless the state is `none`. `seq` increments per
  emission so a consumer that sees two payloads in one drain knows the order.
- Terminated by `ST` (`ESC \`), never `BEL`, so a terminal that logs unknown OSCs does not ring.
- Emitted once per change, never periodically. A consumer that wants the current value on attach
  reads the title.

koh 0.11 receives it through `Callbacks::unhandled_osc` into `take_unhandled_oscs()` (a ring of
16 payloads, 256 bytes each; this payload is under 60). fux drains that ring on each pane drain
and updates the pane's agent state in the workspace. Terminals that do not understand OSC 7877
discard it, per ECMA-48. Confirming that on Terminal.app, iTerm2, kitty, alacritty, wezterm,
Termux and under tmux is the first manual test, before any rule is written.

### In-band: the title

With `--title prefix` (the default) the wrapper rewrites the child's OSC 0/2 to
`<glyph> <original title>` where the glyph is `●` working, `◐` blocked, `○` idle, and nothing for
none. `--title replace` emits `<glyph> <agent>` regardless of what the child set. `--title never`
passes titles through. The rewrite is the reason the wrapper parses OSC 0/2 rather than only
copying them: it must know the original to prefix it, and it must re-emit on a state change even
when the child did not.

### Out-of-band: event lines

One JSON object per line, on the path given by `--events` (unix socket, connected as a client; or
a fifo; or fd 3):

```json
{"t":"state","state":"blocked","agent":"claude","seq":12,"ts":1756900000.123}
{"t":"agent","agent":"claude","pid":48211,"ts":1756899990.001}
{"t":"agent","agent":null,"ts":1756901000.500}
{"t":"exit","code":0,"ts":1756901001.000}
```

The stable-state refresh every 800 ms goes here as a `state` line with the same `seq`, so a
consumer can distinguish a change (new `seq`) from a heartbeat. Writes are non-blocking and a
full pipe drops lines rather than stalling the pty; the next line carries the current state, so a
dropped line costs nothing durable.

This is what a notifier script, a status bar, or a test harness reads. It is not what fux reads;
fux is a terminal and takes the OSC.

---

## Rule sets and fixtures

Rule sets are bundled in the binary with `include_str!` and overridable by `--rules <dir>` and by
`$XDG_CONFIG_HOME/zor/rules/*.toml`. The first release ships rules for the agents the author can
capture panes for, Claude Code first; the rest follow as fixtures arrive. There is no remote
manifest update; a rule change is a release.

A fixture is a captured screen: `zor --debug` writes one on request (a keybinding, or
`SIGUSR1`) as a text file with the visible rows, the title, the progress state, and the expected
verdict in a header. `tests/fixtures/<agent>/<name>.txt`. The test suite evaluates every fixture
and asserts the verdict; `zor check` does the same for one file from the shell. A rule set is not
merged without a fixture for every state it can produce and for every guard it carries.

The hysteresis machine is tested separately with a scripted verdict sequence and a mock clock;
no pty is involved. The pty passthrough is tested with a scripted child that emits every escape
sequence class (CSI, OSC with BEL and ST terminators, DCS, split across chunk boundaries) and
asserts the bytes reaching stdout are identical apart from the wrapper's own OSCs.

---

## What fux does with it

- Spawns every pane as `zor --title never -- $SHELL` (or the configured default command). fux
  draws its own status, so it does not want the title touched.
- Reads OSC 7877 from `take_unhandled_oscs()` on each pane drain and sets the pane's agent state
  in `WorkspaceState`.
- Reads the `agent=` field to label the pane in the tab bar.
- Fires its notifier on transitions into **blocked** and **idle**, as before.
- Nothing else. fux carries no rules, no regex, no hysteresis, no process-tree code.

A user running plain koh on a phone runs `zor -- claude` on the host and gets the title glyph in
koh's status line and the bell hook as before.

---

## Risks

### Passthrough fidelity — *the whole product*

A wrapper that corrupts a query response or splits an escape sequence breaks the program inside it
in ways the user blames on the program. **Mitigation:** the wrapper never buffers output (write
first, parse a copy), never answers queries, and inserts its own bytes only when vt100 reports the
parser is in ground state. The passthrough test runs the vttest-style corpus through it.

### Double emulation cost — *bounded*

Two vt100 screens per pane under fux. **Mitigation:** measured with fux's chaos harness at 40
panes before v1; if it matters, the wrapper's screen can be shrunk to the bottom 40 rows since no
region reads higher.

### Process-tree lookup on macOS — *reimplemented, not copied*

`proc_listpids` and `KERN_PROCARGS2` are unpleasant. herdr's `platform/macos.rs` shows what works;
the wrapper writes its own with the same syscalls. Failure degrades to identification by the
command line given to `zor`, so `zor -- claude` always works.

### The OSC number — *pick once*

If 7877 later collides with a terminal's own use, the consumer side is one constant in fux and
one in any script. The `state=` key/value form means a collision is detectable, not silent.

---

## Read from source

- `herdr/src/pane/agent_detection.rs:5-13` — the hysteresis constants: 100 ms recheck, 3
  confirmations, 700 ms cap, 800 ms stable refresh, 3 s startup grace
- `herdr/src/pane.rs:276-284` — process recheck cadences: 6 miss confirmations, 5 s identified,
  8 s acquisition window, 500 ms fast recheck, 2 s idle reset
- `herdr/src/detect/mod.rs:239-271` — `identify_agent_in_job`: group leader first, then best score
- `herdr/src/detect/manifests/claude.toml` — the priority ladder and the `skip_state_update`
  transcript-viewer rule
- `herdr/src/detect/manifest.rs:1104` — `validate_region_name`, the region vocabulary
- `herdr/src/platform/macos.rs:272-390` — `foreground_job`, `tcgetpgrp` on the master fd
- `koh/src/terminal/server.rs:9-20`, `:95-106`, `:210-217` — OSC 9;4 parsing, the unhandled OSC
  ring (16 × 256), `progress()` and `take_unhandled_oscs()`
- `koh/src/client/cli.rs:44-88` — `BellHook`, `KOH_TITLE` in the hook's environment
- `vt100-0.16.2/src/callbacks.rs:23`, `:66` — `set_window_title`, `unhandled_osc`

---

## Decisions

- One binary, one crate, pure Rust, MIT. Depends on vt100, portable-pty, regex, serde, toml,
  libc. Not on koh.
- Passthrough first: write before parse, never answer queries, insert only in ground state.
- Wrap the shell, identify by process tree; `--agent` and a bare agent command short-circuit it.
- OSC 7877 with key/value payload, ST-terminated, once per change. Title prefix by default.
- JSON event lines out-of-band, non-blocking, with an 800 ms heartbeat.
- herdr's hysteresis and recheck constants are adopted as numbers. Everything else is written
  fresh from captured panes; no herdr code or data enters the tree.
- Rules bundled, overridable from a directory, shipped per release. No remote updates.
- A rule set needs a fixture per state and per guard before it merges.
- Linux and macOS, including Termux. Windows out of scope.

## Open questions

- **Should fux also accept the OSC from an unwrapped pane?** A future agent could emit OSC 7877
  itself. Yes, trivially, since fux reads the OSC and does not care who wrote it. Worth
  documenting the OSC as a contract agents may adopt.
