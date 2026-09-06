> Historical record from before the 0.3.0 bevy_ecs rewrite (2026-09-05). The host, popup panes, sidecar supervision, protocol v2/`FUXCTL1` and verification results described here no longer exist. Current architecture: [design.md](design.md); current evidence: [ecs-acceptance.md](ecs-acceptance.md).

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
  rule files you write come from the fixtures you capture, not from herdr's manifests. Where
  `DESIGN.md` states an exact semantic (the rule test, `prompt_box`, gate conjunction, the idle
  fallback, the hold), implement exactly that; it was audited against herdr's behaviour and the
  Claude rules depend on it.

## 1. `screen`: the emulator with callbacks

`src/screen.rs`. A `Screen` wrapping `vt100::Parser` with a `Callbacks` impl that records: the
OSC 0/2 title (`set_window_title`), the bell count (`audible_bell`), the latest OSC 9;4 progress
parsed from `unhandled_osc` (`state` 0 clear, 1 normal, 2 error, 3 indeterminate, plus percent;
ignore other OSC 9 subcommands), and a `changed` flag set by any callback or by `process` when the
screen contents differ. `process(&mut self, bytes)` feeds the parser and returns whether the
parser ended in ground state (`vt100` exposes this through the parser's state; if it does not in
0.16.2, track it by scanning for an unterminated `ESC` in the last chunk and say so in a comment).
`resize(rows, cols)`. Construct the parser with a scrollback of at least `rows` lines (grow it on
resize); the detection window needs it. The title is control-character-stripped and capped at 256
chars; an empty OSC 0/2 clears it.

Implement `rules::view::ScreenView` for it: `lines() -> impl Iterator<Item = Cow<str>>` yielding
the **detection window** from `DESIGN.md` *Rules*: on the primary screen, `rows` lines ending at
the later of the last non-blank viewport row and the cursor row, reaching into scrollback via
`set_scrollback` when the viewport bottom is blank, trailing blank lines dropped; on the
alternate screen, the last `rows` rows; when the whole viewport is blank the window ends at the
viewport bottom. Each line right-trimmed, wide glyphs once, continuation cells skipped. The
joined text (`text() -> &str`, cached) has lines separated by `\n` and ends with `\n`, so `^`
and `\A` anchors sit at line starts. Plus `title() -> &str`, `progress() -> Option<Progress>`
(`None` until the first report; a clear yields `Some(Progress { state: 0, percent: 0 })`),
`size() -> (u16, u16)`.

Tests: title set, cleared, stripped and capped; bell count; OSC 9;4 with each state and with
malformed params; `changed` is false after feeding bytes that repaint identical content; a row
with a CJK glyph reads back as one character; a screen whose bottom half is blank yields a window
that starts in scrollback; the alternate screen yields its own rows; the window restores the
viewport to live afterwards.

## 2. `rules`: regions, matchers, evaluation, identification

`src/rules/{mod,view,region,matcher,schema,eval,ident}.rs`.

- **Schema** (`schema.rs`): serde types for a rule file, `deny_unknown_fields`. Top level: `id`,
  `aliases: Vec<String>`, `process_names: Vec<String>` (what `ident` matches; defaults to
  `[id] + aliases`), `prompt_marker: Option<String>`, `block_markers: Vec<String>` (line prefixes that mean the agent
  has answered since the last prompt; used by `whole_unless_at_prompt`), `rules: Vec<Rule>`. `Gate` is one recursive
  type with optional lists `contains`, `regex`, `line_regex`, `all`, `any`, `not`. `Rule` is a
  `Gate` flattened together with `id`, `state: State`, `priority: i32` (default 0), `region:
  Region` (default `whole`), `visible_idle: bool`, `visible_blocker: bool`, `visible_working:
  bool`. `State` is
  `Working | Blocked | Idle | Skip`. `Region` is the enum from the design's table; the
  parameterised ones parse from `bottom(12)` form with `FromStr` and serialise back. Load-time
  validation, each failure naming the file, the rule and the problem: unique rule ids; every
  regex compiles; every positive gate (`all`, `any` members, the rule itself) has at least one
  matcher; `not` gates have a matcher; `visible_idle` only on idle rules, `visible_blocker`
  only on blocked, `visible_working` only on working; `skip` rules carry no flags; at most 128
  rules, 512 gates, 1024 matchers, 32 matchers per gate, 512 chars per matcher, nesting depth 8.
- **Regions** (`region.rs`): `fn extract<'a>(region, view: &'a impl ScreenView) -> RegionText<'a>`,
  computed lazily and memoised per evaluation in a small `RegionCache` that also caches the
  lowercased form for `contains`. Implement every row of the design's table exactly as written
  there; in particular: a **horizontal rule** is a line whose trimmed text starts with a run of `─`
  (U+2500) where either nothing follows the run or the run is at least three long (`──` is a
  rule, `─── done` is a rule, `---` is not);
  `prompt_box` is the lines strictly between the second rule counting up from the bottom and the
  next rule below it, empty with fewer than two rules; `bottom_non_empty(n)` starts at the
  *n*-th non-empty line from the bottom and runs to the end, blanks included; `top_non_empty(n)`
  mirrors it from the top; `after_last_prompt_marker` uses the rule
  set's `prompt_marker` (a line equal to the marker, or starting with the marker and a space);
  `whole_unless_at_prompt` is empty when the last such marker line has no later line starting
  with one of `block_markers`, and the whole text otherwise. `progress` is
  `format!("{state}:{percent}")` or empty when `progress()` is `None`.
  Document each region's exact definition in its doc comment; that comment is the contract rule
  authors read.
- **Gates** (`matcher.rs`): a gate matches when **all** of its parts hold: every `contains`
  needle is in the lowercased region text (needles are lowercased at load); every `regex`
  matches the region text (lines joined with `\n`, compiled once at load); every `line_regex`
  matches at least one line; every `all` sub-gate matches; at least one `any` sub-gate matches
  if `any` is non-empty; no `not` sub-gate matches.
- **Evaluation** (`eval.rs`): `fn evaluate(set: &RuleSet, view: &impl ScreenView) -> Verdict`.
  Evaluate every rule, take the highest priority match; ties resolve to the earlier rule in the
  file. `Verdict { state, visible_idle, visible_blocker, visible_working, rule: Option<RuleId>,
  region }`. **No
  rule matching means `Idle` with both flags false and `rule: None`**: the working signals
  vanishing is how an agent shows it finished, and the state machine relies on this to leave
  working. A `Skip` verdict means "leave the current state alone"; return it as such, the state
  machine handles it. Flags survive only when the winning rule's state carries them.
- **Identification** (`ident.rs`): `fn identify(job: &Job, sets: &[RuleSet]) ->
  Option<(AgentId, Pid)>`. `Job { leader: Pid, processes: Vec<Process> }`, `Process { pid, ppid,
  comm, argv0: Option<String>, argv: Vec<String>, env_agent: Option<String> }` from `platform`.
  A process with `env_agent` (the `ZOR_AGENT` variable) that names a loaded set wins outright.
  Otherwise normalise: effective name is `argv0` if present (macOS; `None` on Linux) else
  `comm`; `tmux` is never an agent; if the name is a generic runtime or shell (`sh bash zsh fish
  node bun python python3`), scan `argv[1..]` for the wrapped script: return `None` on an eval
  flag (`-e --eval -p --print` for node and bun; `-c -m` for python; `-c` for shells); skip
  other flags and the value following one that takes a value (`-r --require --loader --import
  --experimental-loader --inspect-port -W -X -o -S -L`, `=` forms included); stop at `--`; strip
  surrounding quotes; basename; strip `.js .mjs .cjs .py`. If the result matches no set,
  `fs::canonicalize` the path and try the target's basename. Match against `process_names`.
  Among matches: the leader if it matches; else the highest score (3 when the normalised name
  differs from `comm`, which covers a wrapped script and a changed process title; 2 for a direct
  binary; 1 for a bare runtime name), earliest process on ties.

Tests: each region on a hand-written screen with the expected lines, including `prompt_box`
with zero, one and two rules and a blank line inside `bottom_non_empty(3)`; a `---` line is not
a rule; each gate part, conjunction across parts, `any` as disjunction, `not` as a guard,
nesting; `contains` is case-insensitive; `line_regex` versus `regex` on a multi-line region;
priority ties go to the earlier rule; no match yields plain `Idle`; `Skip`; flags dropped on a
state mismatch; each validation failure names the rule; identification prefers the leader,
resolves `node /x/bin/claude` and `node -r ./hook.js /x/claude.js` to `claude`, returns `None`
for `node -e …`, `python -m …`, `tmux` and a plain shell, honours `ZOR_AGENT`, follows a
symlink, and gives a title-changed process (argv0 ≠ comm) score 3; `whole_unless_at_prompt`
with and without a block marker after the last prompt line.

## 3. `state`: the hysteresis machine

`src/state/mod.rs`. Pure, clock-injected: `Machine::new(config)`, `fn observe(&mut self, verdict:
Option<Verdict>, agent: Option<AgentId>, now: Instant) -> Vec<Event>`, `fn tick(&mut self, now)
-> Vec<Event>`, `fn next_deadline(&self) -> Option<Instant>`. The constants from `DESIGN.md`
*Hysteresis* in a `Config` with those defaults:

- **Hold.** Only working → idle with `visible_idle == false` is held. The first such verdict
  opens the hold (no publish) and `next_deadline` moves to 100 ms; each further plain-idle
  verdict adds a confirmation; at 3 confirmations the idle is published (four verdicts in a row).
  Any other verdict, or a change of agent, cancels the hold. If the hold is still open 700 ms
  after it opened, `tick` publishes the idle: the cap forces publication, it never cancels.
- **Immediate.** Every other transition publishes on its first verdict: into working, into
  blocked, blocked → idle, working → idle with `visible_idle`, and a change of any `visible_*`
  flag with the state unchanged.
- **Startup grace.** For 3 s after `AgentFound` (first identification or a replacement) idle
  verdicts are dropped; working and blocked pass. This deviates from herdr, which drops every
  verdict in the window; say so in a comment.
- **Skip** and a `None` verdict (no agent identified) leave the state alone and cancel any hold.
- **Agent exit.** The caller reports `Exited { agent }` when the shell is back in the foreground
  (see §4): publish `idle` with `exited: true` immediately, then on the caller's `AgentLost`
  publish `none`. A six-miss loss (foreign foreground job) comes straight as `AgentLost`.
- **Blocked re-announce.** While the current state carries `visible_blocker`, emit
  `Event::Heartbeat` every 800 ms with the same `seq`; the emitter sends it on the event channel
  only.
- **Heartbeat.** Any stable state repeats as `Event::Heartbeat` every 800 ms with the same `seq`,
  event channel only.

`Event` is `Changed { state, previous, agent, seq, visible: Flags, exited }`, `Heartbeat
{ state, agent, seq, visible: Flags }`, `AgentFound { id, pid }`, `AgentLost`; `Flags` is three
bools. `seq` is a `u64`
that increments on `Changed` only. `observe` takes `Option<Verdict>`, the current
`Option<AgentId>`, an `exited: bool`, and `now`.

Tests, all with a fake clock: the three-confirmation path publishing on the fourth verdict; a
working verdict mid-hold cancelling it; the 700 ms cap publishing with only one confirmation;
`visible_idle` bypassing the hold; blocked publishing immediately from working and from a held
idle; blocked → idle publishing immediately; startup grace swallowing idle but not blocked;
`Skip` and `None` cancelling a hold without publishing; exit producing `idle(exited)` then `none`;
blocked re-announce cadence and its absence for a plain blocked; heartbeat cadence; `seq`
monotonic and unchanged across heartbeats.

## 4. `platform`: process tree and terminal control

`src/platform/{mod,linux,macos}.rs`, `cfg`-gated, the only `unsafe`.

- `fn foreground_pgid(child: Pid) -> Option<Pid>`, cheap, called every tick: Linux parses
  `tpgid` (field 8 after the closing `)`) from `/proc/<child>/stat`; macOS `proc_pidinfo(child,
  PROC_PIDTBSDINFO)` and `e_tpgid`. On Linux fall back to `tcgetpgrp(master)` when `tpgid` reads
  0; never use it on macOS, where it is unspecified on a ptmx.
- `fn leader(pgid) -> Option<Process>`, the cheap lookup tried first.
- `fn job(child: Pid, pgid) -> Job`, the full listing: Linux walks `/proc/<pid>/task/*/children`
  breadth-first from `child` and from `pgid`, keeping processes whose `pgrp` (stat field 5)
  equals `pgid`, with `comm` from stat and argv from `/proc/<pid>/cmdline`; macOS
  `proc_listpids(PROC_PGRP_ONLY, pgid)`, `pbi_pgid` confirmed via `PROC_PIDTBSDINFO`, argv and
  `argv0` from `sysctl KERN_PROCARGS2` (argv0 is the process title; Linux has no equivalent and
  leaves it `None`). Read `ZOR_AGENT` from
  `/proc/<pid>/environ` and from the `KERN_PROCARGS2` environment block into `env_agent`. An
  empty job on any error; identification then falls back to `--agent` or the command `zor` was
  given, if that names a set.
- The **probe scheduler** lives in `platform/probe.rs` (no `unsafe`): tick every 500 ms while
  unidentified, 300 ms while identified, 100 ms while the state machine reports a hold; each tick
  reads `foreground_pgid`; run `leader` then `job` when the pgid changed, every 5 s while
  identified, after 30 s with no pgid at all, and inside an acquisition window (8 s, opened when
  the pgid changed while unidentified or when the screen changed with no agent; probe at 500 ms
  for the first 1.5 s, then every 2 s). Report `Exited` when the identified agent is gone and the
  foreground job contains the pane shell (the child pid, with or without other members); count a miss when the foreground
  job is something else with no agent in it, and report `AgentLost` after six consecutive misses.
  A different agent, or the same agent under a new pid, is `AgentFound` again, and the caller
  clears title and progress evidence.
- `fn set_raw(fd) -> Result<Guard>`: `tcgetattr`/`cfmakeraw`/`tcsetattr` with the guard restoring
  on drop and on panic. `fn winsize(fd) -> (u16, u16)`.

Tests: a spawned `sleep` child appears as its group's leader with the right `comm` and argv; a
`sh -c 'sleep 30'` job lists both processes; `ZOR_AGENT` set on a spawned child is read back;
`set_raw` restores on drop (compare `tcgetattr` before and after). The probe scheduler is tested
with a fake clock and a scripted pgid sequence: cadence per state, the 5 s reprobe, the
acquisition window's two phases, six misses, and exit-versus-miss classification. Mark the
tty-dependent ones `#[ignore]` under CI without a tty and say so.

## 5. `pty`: spawn and passthrough

`src/pty.rs`. `Pty::spawn(command, argv, size, env)` with `portable-pty`, `TERM` and the parent
environment inherited, `ZOR_PID` set so a nested `zor` can detect it and pass through without a
second emulator. Two threads: **reader** copies master → stdout, writing each chunk to stdout
*before* sending a copy over a channel to the main loop; **writer** copies stdin → master. The main
loop owns `Screen`, `Machine`, the probe scheduler and the emitters, and blocks on the channel
with a timeout equal to the machine's next deadline or the probe's, whichever is sooner. It
evaluates rules only when the screen `changed` since the last evaluation or a hold is pending;
an idle pane that draws nothing costs nothing. `SIGWINCH` (via `signal-hook`) forwards the new size, `ws_xpixel` and `ws_ypixel` included,
from `TIOCGWINSZ` on stdin to `TIOCSWINSZ` on the master and the rows and cols to `Screen`; the
initial spawn size carries the pixel fields too. zor sets no `TERM` or `COLORTERM`, answers no
XTGETTCAP or DA, and tracks no kitty keyboard state; the real terminal does all of that.
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

- `osc::state(&Report) -> Vec<u8>` produces
  `ESC ] 7877 ; state=… ; agent=… ; seq=… ESC \` exactly as `DESIGN.md` gives it; `agent=` is
  omitted for `none`; `visible=idle,blocker,working` lists the set flags and is omitted when
  none; `exited=1` when set; `message=` percent-encoded, 128 bytes max, only for self-reports
  passed through. `osc::parse` accepts unknown keys and ignores them. Also `osc::parse(payload: &[u8]) -> Option<StateReport>` for consumers and
  for the round-trip test; export it from the library so fux can use the same parser.
- `title`: with `prefix`, on every child OSC 0/2 rewrite to `<glyph> <original>`; on a state
  change, re-emit the last title with the new glyph; with `replace`, emit `<glyph> <agent>`; with
  `never`, pass through. Glyphs `●` `◐` `○`; none for `none`. On exit, if a title was ever
  rewritten, emit the last original title unprefixed.
- `events`: one JSON object per line with `t`, `state`, `previous`, `agent`, `seq`, `pid`,
  `code`, `title` (the child's current unprefixed title), `visible` (array of set flag names,
  omitted when empty) and `exited` when true, as in the design and `ts` as seconds since the epoch with millisecond precision. Sink is a unix socket
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
with the matched rule id, and logs any `OSC 7877` or `OSC 21337` the child emits itself so the
self-report path can be studied before it is wired (do not act on them in 0.1.0). `SIGUSR1` writes a fixture file (format below) to `$TMPDIR` and prints
its path to stderr.

## 8. Fixtures, the Claude rule set, and `check`

Fixture format, `tests/fixtures/<agent>/<name>.txt`:

```
# agent: claude
# title: ● Claude Code
# progress: 3:0
# expect: working
# matched: spinner_footer
<the detection window, exactly as displayed, trailing whitespace trimmed>
```

`SIGUSR1` writes the detection window, not the raw viewport, so a fixture is what the rules saw.

`matched` is required and names the rule id the evaluator must pick (or `none` for the idle
fallback). `check` loads a fixture, evaluates it against the named or auto-detected rule set, and
prints the verdict and rule id, exiting 1 on mismatch with `expect` or `matched`. The test suite walks every fixture and does the same.

Write `rules/claude.toml` from fixtures you capture yourself by running `zor --debug -- claude`
and sending `SIGUSR1` in each state: idle at the prompt, working with the spinner, blocked on a
permission prompt, blocked on a plan approval, blocked on a select list (`enter to select`), the
transcript viewer (`ctrl+o`), the model picker, and idle with "Do you want to proceed?" typed
into the prompt box as a guard test. Signals to verify from the captures and encode if present:
the title starting with a spinner glyph (braille U+2800–U+28FF or half circles U+25D0–U+25D3)
followed by a space as working at the top priority, and starting with `✳` as a low-priority idle;
`progress` equal to `0:0` (a cleared report) as low-priority idle; a footer line starting with `⏸` or `⏵` and
containing `esc to interrupt`, or an activity line starting with one of `*·✢✶✻✽` and ending in
`…`, in `bottom_non_empty(12)` as working; `esc to cancel` together with `enter to confirm` or
`enter to select` in `after_last_rule` as blocked with `visible_blocker`; `do you want to
proceed?` corroborated by a command preview and a numbered `yes`/`no` line, on `whole`, as
blocked with `visible_blocker`; a `prompt_box` whose
first line starts with `❯` as idle with `visible_idle`, guarded by `not` against the blocked
texts; skip rules for the transcript viewer and the model picker. Mark the spinner and footer rules `visible_working`. Set `prompt_marker = "❯"`; Claude needs
no `block_markers`.
Confirm from a capture that the prompt is delimited by two full-width `─` rules, which is what
`prompt_box` assumes. The Claude set needs a fixture per state and for the guard before it is
committed; if you cannot run Claude Code in this
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
