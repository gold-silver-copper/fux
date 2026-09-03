# Prompt: the agent-state wrapper, 0.1.0

Paste the section below into a Claude Code session opened in an empty directory that will become
the wrapper's repo. Put `docs/wrapper-design.md` from the fux repo next to it as `DESIGN.md`
first; the prompt refers to it. Written 3 Sep 2026 against herdr 0.8.2, koh 0.11.0 and vt100
0.16.2. The crate name is `zor` (Slavic root for sight; free on crates.io as of 3 Sep 2026).

---

Build `zor` 0.1.0 from `DESIGN.md` in this directory. Read it in full before writing anything;
every decision below is either stated there or is a consequence of it. `zor` is a pty
passthrough that wraps a shell or an agent, keeps a `vt100::Screen` of what the child draws,
identifies the agent by the pty's foreground process group, evaluates per-agent rules with
hysteresis, and announces the state in-band as `OSC 7877` and out-of-band as JSON event lines.
It is transparent to the program inside it. It depends on nothing from koh, herdr or fux.

Work on `main` in this fresh repo with small commits, one per section below, each leaving the
tree building and the tests green. Finish with a `CHANGELOG.md` entry for 0.1.0 and a `README.md`
that is the *Surface* section of `DESIGN.md` plus install instructions.

## Ground rules

- **Edition 2024, `rust-version = "1.91"`, `license = "MIT"`.** `unsafe_code` is allowed only in
  `src/platform/`, behind `#![deny(unsafe_op_in_unsafe_fn)]`, with a `// SAFETY:` comment per
  block. Everywhere else `#![forbid(unsafe_code)]`. `dead_code = "deny"`. Clippy at
  `-D warnings` with `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`,
  `clippy::indexing_slicing` denied outside tests; put that in `Cargo.toml` `[lints]` and
  `clippy.toml`.
- **Dependencies:** `vt100 = "=0.16.2"`, `portable-pty = "=0.9.0"`, `regex`, `serde`,
  `serde_json`, `toml`, `libc`, `clap` (derive), `signal-hook`. Nothing async; the process is two
  blocking threads and a poll loop. Add nothing else without a comment in `Cargo.toml` saying why.
- **Layering, enforced by a CI grep:** `rules/` imports nothing from `pty/`, `platform/` or
  `emit/`; `state/` imports nothing from any other module. `rules/` sees the screen only through
  the `ScreenView` trait in `rules/view.rs`, implemented for `vt100::Screen` in `screen.rs`.
- **Tests** are named as a behavioural sentence, cite the `DESIGN.md` section they cover in a
  comment on the first line, and never sleep on wall-clock time: every timing test injects a
  clock. Integration tests live in `tests/`, one file per module boundary, plus
  `tests/fixtures/<agent>/*.txt`.
- **No herdr code or data.** You may read a herdr checkout if one is at `../references/herdr` for
  behaviour, constants and signal vocabulary; you may not copy a line of Rust or TOML from it. The
  rule files you write come from the fixtures you capture, not from herdr's manifests.

## 1. `screen`: the emulator with callbacks

`src/screen.rs`. A `Screen` wrapping `vt100::Parser` with a `Callbacks` impl that records: the
OSC 0/2 title (`set_window_title`), the bell count (`audible_bell`), the latest OSC 9;4 progress
parsed from `unhandled_osc` (`state` 0 clear, 1 normal, 2 error, 3 indeterminate, plus percent;
ignore other OSC 9 subcommands), and a `changed` flag set by any callback or by `process` when the
screen contents differ. `process(&mut self, bytes)` feeds the parser and returns whether the
parser ended in ground state (`vt100` exposes this through the parser's state; if it does not in
0.16.2, track it by scanning for an unterminated `ESC` in the last chunk and say so in a comment).
`resize(rows, cols)`.

Implement `rules::view::ScreenView` for it: `rows() -> impl Iterator<Item = Cow<str>>` of the
visible rows with trailing whitespace trimmed, `title() -> &str`, `progress() -> Option<Progress>`,
`size() -> (u16, u16)`. Wide glyphs contribute once; continuation cells are skipped.

Tests: title set and cleared; bell count; OSC 9;4 with each state and with malformed params;
`changed` is false after feeding bytes that repaint identical content; a row with a CJK glyph
reads back as one character.

## 2. `rules`: regions, matchers, evaluation, identification

`src/rules/{mod,view,region,matcher,schema,eval,ident}.rs`.

- **Schema** (`schema.rs`): serde types for a rule file. Top level: `id`, `aliases: Vec<String>`,
  `process_names: Vec<String>` (what `ident` matches; defaults to `[id] + aliases`),
  `prompt_marker: Option<String>`, `rules: Vec<Rule>`. `Rule`: `id`, `state: State`,
  `priority: i32`, `region: Region`, and exactly one of `regex: Vec<String>`,
  `contains: Vec<String>`, `any: Vec<Matcher>`, plus optional `not: Box<Matcher>`. `State` is
  `Working | Blocked | Idle | Skip`. `Region` is the enum from the design's table; `bottom(n)` and
  `bottom_non_empty(n)` parse from the string form with `FromStr` and serialise back. Reject a file
  with duplicate rule ids or an invalid regex at load time with an error naming the file, the rule
  and the problem.
- **Regions** (`region.rs`): `fn extract<'a>(region, view: &'a impl ScreenView) -> RegionText<'a>`,
  computed lazily and memoised per evaluation in a small `RegionCache`. `prompt_box` finds the last
  run of rows whose first non-space glyph is a box-drawing character (U+2500–U+257F) and that
  reaches the bottom non-empty row; `after_last_rule` treats a row of three or more of `─`, `═`,
  `-` or `=` as a rule; `after_last_prompt_marker` uses the rule set's `prompt_marker`. Document
  each region's exact definition in its doc comment; that comment is the contract rule authors
  read.
- **Matchers** (`matcher.rs`): `regex` (any of the list, compiled once at load), `contains` (all
  substrings, case-sensitive), `any` (one sub-matcher matches), `not` (the sub-matcher fails).
  Matching is over the region text joined with `\n`.
- **Evaluation** (`eval.rs`): `fn evaluate(set: &RuleSet, view: &impl ScreenView) -> Verdict`.
  Evaluate every rule, take the highest priority match; ties resolve to the earlier rule in the
  file. `Verdict` carries the state, the matched rule id and region, or `Verdict::NoMatch`. A
  `Skip` state means "leave the current state alone"; return it as such, the state machine handles
  it.
- **Identification** (`ident.rs`): `fn identify(processes: &[Process], sets: &[RuleSet]) ->
  Option<(AgentId, Pid)>`. `Process` is `{ pid, ppid, pgid, name, argv }` from `platform`.
  Normalise a name: basename of argv[0]; if that is an interpreter (`node`, `bun`, `deno`,
  `python3?`, `sh`, `bash`, `zsh`), the basename of the first argv entry that is not a flag,
  with `.js`/`.mjs`/`.py` stripped. Match against each set's `process_names`. Prefer the process
  group leader; otherwise the deepest descendant by `ppid` chain.

Tests: each region on a hand-written screen with the expected rows; each matcher including `not`
as a guard; priority ties; `Skip`; a rule file with a bad regex fails to load with the rule id in
the message; identification prefers the leader, resolves `node /x/bin/claude` to `claude`, and
returns `None` for a plain shell.

## 3. `state`: the hysteresis machine

`src/state/mod.rs`. Pure, clock-injected: `Machine::new(config)`, `fn observe(&mut self, verdict:
Option<Verdict>, agent: Option<AgentId>, now: Instant) -> Vec<Event>`, `fn tick(&mut self, now)
-> Vec<Event>`, `fn next_deadline(&self) -> Option<Instant>`. The constants from `DESIGN.md`
*Hysteresis* in a `Config` with those defaults:

- working → idle needs 3 consecutive idle verdicts at least 100 ms apart, all within 700 ms of the
  first; a working verdict resets the count.
- transitions into working or blocked emit on the first verdict.
- idle verdicts within 3 s of the agent first being identified are ignored.
- agent lost: the caller passes `agent: None` only after its own six-poll confirmation; the
  machine then waits 2 s of no screen change before emitting `none`.
- `Skip` and `NoMatch` leave the state alone.
- an 800 ms heartbeat event (`Event::Heartbeat(state)`) while a state is stable, for the event
  channel only.

`Event` is `Changed { state, agent, seq }`, `Heartbeat { state, agent, seq }`, `AgentFound(id,
pid)`, `AgentLost`. `seq` is a `u64` that increments on `Changed` only.

Tests, all with a fake clock: the three-confirmation path including a reset; the 700 ms cap
expiring; blocked emitting immediately; startup grace swallowing idle; `none` after the quiet
period; heartbeat cadence; `seq` monotonic and unchanged across heartbeats.

## 4. `platform`: process tree and terminal control

`src/platform/{mod,linux,macos}.rs`, `cfg`-gated, the only `unsafe`.

- `fn foreground_pgid(master_fd) -> Option<Pid>` via `tcgetpgrp`.
- `fn processes_in_group(pgid) -> Vec<Process>`: Linux reads `/proc/*/stat` for pgid and ppid and
  `/proc/<pid>/cmdline` for argv; macOS uses `proc_listpids`, `proc_pidinfo` with
  `PROC_PIDTBSDINFO` for pgid and ppid, and `sysctl KERN_PROCARGS2` for argv. Both return an
  empty vec on any error; identification then falls back to the command `zor` was given.
- `fn set_raw(fd) -> Result<Guard>`: `tcgetattr`/`cfmakeraw`/`tcsetattr` with the guard restoring
  on drop and on panic. `fn winsize(fd) -> (u16, u16)`.

Tests: a spawned `sleep` child appears in its own group with the right name; `set_raw` restores
on drop (compare `tcgetattr` before and after). Mark them `#[ignore]` under CI without a tty and
say so.

## 5. `pty`: spawn and passthrough

`src/pty.rs`. `Pty::spawn(command, argv, size, env)` with `portable-pty`, `TERM` and the parent
environment inherited, `NAME_PID` set so a nested `zor` can detect it and pass through without a
second emulator. Two threads: **reader** copies master → stdout, writing each chunk to stdout
*before* sending a copy over a channel to the main loop; **writer** copies stdin → master. The main
loop owns `Screen`, `Machine`, the platform poller and the emitters, and blocks on the channel
with a timeout equal to the machine's next deadline or the identification cadence, whichever is
sooner. `SIGWINCH` (via `signal-hook`) forwards the new size to the master and to `Screen`.
`SIGCHLD` or a closed master ends the loop; exit with the child's status, restoring the terminal
first.

Injection: the wrapper's own bytes are queued and written to stdout only when the last chunk
processed ended in ground state; otherwise they wait for the next chunk. Never write them from the
reader thread; the main loop writes them, with a mutex on stdout shared with the reader so a
chunk and an injection never interleave.

Tests (`tests/passthrough.rs`): a scripted child (a tiny binary in `tests/bin/`, or `printf` with
known sequences) emits CSI, OSC with BEL and with ST terminators, DCS, a sequence split across two
writes with a 50 ms pause, and a DA query; assert the bytes reaching the wrapper's stdout equal the
child's output with the wrapper's own OSC 7877 sequences removed, and that the DA response
written to the wrapper's stdin reaches the child unchanged. Exit code is propagated for 0, 1 and a
signal death.

## 6. `emit`: the OSC and the event lines

`src/emit/{osc,title,events}.rs`.

- `osc::state(state, agent, seq) -> Vec<u8>` produces
  `ESC ] 7877 ; state=… ; agent=… ; seq=… ESC \` exactly as `DESIGN.md` gives it; `agent=` is
  omitted for `none`. Also `osc::parse(payload: &[u8]) -> Option<StateReport>` for consumers and
  for the round-trip test; export it from the library so fux can use the same parser.
- `title`: with `prefix`, on every child OSC 0/2 rewrite to `<glyph> <original>`; on a state
  change, re-emit the last title with the new glyph; with `replace`, emit `<glyph> <agent>`; with
  `never`, pass through. Glyphs `●` `◐` `○`; none for `none`. On exit, if a title was ever
  rewritten, emit the last original title unprefixed.
- `events`: one JSON object per line with `t`, `state`/`agent`/`seq`/`pid`/`code` as in the
  design and `ts` as seconds since the epoch with millisecond precision. Sink is a unix socket
  (connected once at start; reconnect on `EPIPE` at most once per second), a fifo, or fd 3. All
  writes non-blocking; a `WouldBlock` drops the line and increments a counter reported at exit
  under `--debug`.

Tests: OSC round-trip through `parse`; the wrapper's OSC fed to a `vt100::Parser` with a callbacks
impl records it in `unhandled_osc` with params `["7877", "state=…", …]` (this is the koh
contract); title prefix, replace, never, and the restore on exit; an event line parses back and
carries the expected fields; a full pipe drops rather than blocks.

## 7. `cli` and `main`

`src/cli.rs`, `src/main.rs`. Clap with the surface from `DESIGN.md`: `zor [opts] [--] cmd…`,
`--events`, `--title`, `--no-osc`, `--rules`, `--agent`, `--debug`, and the subcommands `check
<fixture> [--agent]` and `agents`. Default command `$SHELL -l` or `/bin/sh -l`. Rule sets load
from the bundle (`include_str!` under `rules/*.toml`, registered in `src/rules/bundle.rs`), then
`$XDG_CONFIG_HOME/zor/rules/*.toml`, then each `--rules` dir; a later set with the same `id`
replaces an earlier one entirely. `--debug` prints each verdict and each machine event to stderr
with the matched rule id. `SIGUSR1` writes a fixture file (format below) to `$TMPDIR` and prints
its path to stderr.

## 8. Fixtures, the Claude rule set, and `check`

Fixture format, `tests/fixtures/<agent>/<name>.txt`:

```
# agent: claude
# title: ● Claude Code
# progress: 3:0
# expect: working
# matched: spinner_footer
<visible rows, exactly as displayed, trailing whitespace trimmed>
```

`check` loads a fixture, evaluates it against the named or auto-detected rule set, and prints the
verdict, exiting 1 on mismatch with `expect`. The test suite walks every fixture and does the same.

Write `rules/claude.toml` from fixtures you capture yourself by running `zor --debug -- claude`
and pressing `SIGUSR1` in each state: idle at the prompt, working with the spinner, blocked on a
permission prompt, blocked on a plan approval, the transcript viewer (`ctrl+o`), and idle with
"Do you want to proceed?" typed into the prompt box as a guard test. Signals worth encoding, from
observation: the title spinner glyph range (braille U+2800–U+28FF and half circles U+25D0–U+25D3),
the `esc to interrupt` footer, the `╭─╮` prompt box, `❯` as the prompt marker, and `Do you want
to proceed?` and `Yes` `No` option rows above the box for blocked. The Claude set needs a fixture
per state and for the guard before it is committed; if you cannot run Claude Code in this
session, commit the rule file as `rules/claude.toml.draft` with the fixtures it still needs listed
at the top, and leave the bundle empty.

## 9. CI and release

`.github/workflows/ci.yml`: fmt, clippy, test on `ubuntu-latest` and `macos-latest`, the layering
grep, an `aarch64-linux-android` `cargo check` with the NDK. `cargo publish --dry-run` on tags.

## Deliverables

- A green `cargo test` on Linux and macOS.
- `README.md`, `CHANGELOG.md` for 0.1.0, `DESIGN.md` untouched except a note under *Open
  questions* for anything you had to decide that it did not.
- A closing summary listing each place you deviated from `DESIGN.md` and why, and every test that
  is `#[ignore]`d and what it needs to run.
