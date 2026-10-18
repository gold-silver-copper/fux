# fux-bench: fux's speed against master, in instructions

An excluded package with its own lockfile. It runs fixed workloads through
fux and fux-vt in process and counts the instructions each retires, on the
working tree and on another commit (`master` by default), in one run: no
stored baseline, and a busy machine moves the counts far less than times. `feel`
measures wall-clock latency and throughput through real servers, beside
tmux, zellij and herdr.

CI does not run it; `fux-vt/compare/run.sh full` runs `--against master`, and
`run.sh deep` runs `feel` and `info`. It forbids fux's lints and builds with
fux's release profile (`lto = "thin"`, `codegen-units = 1`): any other
inlines differently and changes the counts.

## Commands

From the repository root:

```sh
cargo run --release --manifest-path bench/Cargo.toml -- --against master
cargo run --release --manifest-path bench/Cargo.toml -- --against HEAD~1 --floors FLOORS.json --only paint/
cargo run --release --manifest-path bench/Cargo.toml -- --calibrate --json FLOORS.json

bench/target/release/fux-bench list          # the workloads
bench/target/release/fux-bench time          # each one's thread CPU time here, and MB/s
bench/target/release/fux-bench run vt/ascii  # one workload once, as --against counts it
bench/target/release/fux-bench info          # MB/s beside Ghostty and alacritty, and fux-diff --speed
bench/target/release/fux-bench feel          # latency, throughput and footprint beside tmux, zellij and herdr
```

| Command and flags | Meaning |
| --- | --- |
| (default) `--against REF` | every workload on REF (default `master`) and on the working tree, in instructions |
| `--repeats N` | rounds (default 15) |
| `--jobs N` | threads (default: every core) |
| `--floors FILE` | judge each workload against its floor from a calibration |
| `--calibrate` | an A/A run of the working tree built twice: each workload's floor, written to the JSON |
| `--only TEXT` | only workloads whose names contain TEXT; repeatable |
| `--json FILE` | where the JSON goes (default `bench/target/fux-bench/against.json`, `info.json` or `feel.json`) |
| `run WORKLOAD [--baseline]` | one workload, or its baseline, once |
| `time [WORKLOAD...]` | thread CPU time, best of 3, and MB/s; names match by substring |
| `--corpus DIR` | for `run` and `time`: recordings other than `fux-vt/compare/corpus/` |
| `feel --muxes LIST --parts LIST --keys N` | see [`feel`](#feel-what-a-person-at-the-terminal-feels) |

Exit status: 1 if `--against` flags a workload, 2 on an error.

## The workloads

| Workload | What runs | Fed |
| --- | --- | --- |
| `vt/NAME` | fux-vt alone, set up as fux sets up a pane (`fux::pane::OPTIONS`), 10,000 rows of history | 4 KiB at a time, as `fux-vt/compare`'s `bench` feeds it |
| `pane/NAME` | `fux::pane::Pane::output` | 16 KiB at a time, as fux's server gives a pane what it reads (up to 64 KiB a tick) |
| `vt/corpus:NAME` | fux-vt alone on one recording at its size, repeated to 4 MiB | 4 KiB at a time |
| `vt/vtebench:NAME`, `vt/kitty:NAME`, `vt/avt:NAME`, `vt/foot:alt-random-writes`, `vt/yes:crlf`, `vt/yes:lf`, `vt/sixel:frames` | fux-vt alone on each of the census's speed payloads (`src/census.rs`; vendored data in `payloads/` with its licences; kitty's GPL payloads re-made by our own code), 8 MiB at 50×200 | 4 KiB at a time |
| `paint/corpus` | a `fux::session::Session` without processes, one client whose pane is each recording's size; after each read the screen is composed (`render::compose_into`) and painted (`render::paint_into`) | every recording in turn, to 4 MiB |
| `paint/split` | the same with two panes side by side given the same output | the same |
| `decode/legacy`, `decode/kitty` | fux's decoder of a client's terminal on the keys typed in the recordings, as a legacy terminal and as a kitty terminal with the flags fux pushes (5) send them | to 1 MiB |
| `encode/legacy`, `encode/kitty` | those keys encoded for a pane, legacy and with every kitty flag | 2^20 keys |

`NAME` is a synthetic workload (`src/synthetic.rs`, `fux-vt/compare`'s
generators), 16 MiB on a 50×200 screen, or `corpus`: every recording in
`fux-vt/compare/corpus/` in turn at its own size, to 4 MiB. A new recording
is a workload at once.

Each workload has a **baseline**: the same run without the part measured
(making or loading the bytes; for `paint/*`, feeding the session too, so
what is left is composition and paint). `paint/*` also report frames and
bytes per frame in the JSON.

## How `--against` counts

1. Both sides are built by one recipe. Each side's tree (REF's, with the
   working tree's `bench/` in it, and the working tree's as it is, changes
   and untracked files included) is `git archive`d into a path of one
   length, `src/old-TREE` and `src/new-TREE`, built with fux's release
   profile into one target dir, and kept as `bin/old-TREE` and
   `bin/new-TREE`, all under `$CARGO_TARGET_DIR/fux-bench-against` (or
   `bench/target/against`). A tree already built is not built again.
2. The binaries run as `run/old` and `run/new`, names of one length: a
   path of another length moves the counts. `--against HEAD` on a clean
   tree builds the one tree twice and is the self-test: it flags nothing.
3. Each workload and its baseline run as child processes, counted by
   `/usr/bin/time -l` on macOS, or on Linux by `bench/perf.sh` (`perf stat
   -e instructions:u` pinned to one core type, with
   `kernel.perf_event_paranoid` at 2 or less; `FUX_PERF` names perf if it
   is not on PATH, `FUX_PERF_CPUS` the CPUs), else valgrind's cachegrind
   (about fifty times slower). A perf count whose PMU ran for less than all
   the time it was enabled (multiplexed, or moved between core types) is
   refused, never scaled.
4. In each of `--repeats` rounds every workload runs four times (each
   side, and each side's baseline), the side that goes first alternating.
5. A round's change pairs both sides' runs in it, each less its side's
   median baseline. A workload's **change** is the median of its rounds',
   its **spread** their median absolute deviation.
6. Its **floor** comes from a calibration (`--calibrate`, an A/A run apart
   from the one judged): max(0.25%, the A/A change, 3 × the A/A spread). A
   change past it is **slower** or **faster**, else **inconclusive**; a run
   whose spread is past two thirds of it is **noisy**; a floor over 1% means
   the tool is **broken** for that workload. Across the recordings, a sign
   test that counts each program's recordings once flags a broad loss.
   Exit 1 on a slower workload or a broad loss, 2 on a broken one. The JSON
   keeps every round and count, and perf's report of every run goes beside
   it (`NAME.perf.txt`).

For fux-vt beside other engines in instructions per byte, see
`fux-vt/compare/run.sh bench --instructions`.

## `info`

Figures not compared against REF: `fux-vt/compare/run.sh bench --engines
ghostty,alacritty --mb 4` (MB/s by wall time, best of 3; its first run
builds Ghostty, which takes minutes) and `fux-diff --speed`.

## `feel`: what a person at the terminal feels

```sh
cargo run --release --manifest-path bench/Cargo.toml -- feel
bench/target/release/fux-bench feel --muxes fux,tmux --parts latency --keys 500
```

`--muxes` takes `direct,fux,tmux,zellij,herdr` (the default; any not installed is
skipped; `direct` is the pane program on a PTY of this process, no
multiplexer), `--parts` takes `latency,throughput,footprint`, and `--keys`
the keys per latency run (default 2000). Wall time is measured; it is
reported, never gated on.

The user's own servers are never touched. Each multiplexer runs on its
own socket and configuration under `fux-feel-PID` (removed after), in
`$TMPDIR` if it is short, else `~/.cache/fux`:
fux as `fux __serve SOCKET` (as a client starts one) with `HOME`,
`XDG_CONFIG_HOME` (its `config.fux`) and `XDG_STATE_HOME` there; tmux as `tmux
-L fux-bench-PID-N -f /dev/null`; zellij in its own session with its
config, data, `HOME` and `ZELLIJ_SOCKET_DIR` there, without plugins or pane
frames; herdr as `herdr server` and a `herdr` client with `HOME` and every
XDG directory there (its socket is in its config directory), its sidebar,
tab bar, borders, gaps, scrollbars, mobile layout, mouse capture and update
checks off, `--focus` on the pane split for the echo. herdr bounds its
scrollback in bytes (10 MB by default), not rows. `FUX_SOCKET`, `FUX_PANE`,
`TMUX`, `TMUX_PANE`, and zellij's and herdr's variables are removed from
all it starts. Patterns (the ready marker, the throughput markers, the
echoed glyphs) are found in the text the client writes, escape sequences
skipped, as herdr moves the cursor before each cell it paints. The client's PTY is 41×120 (a
40×120 pane, the corpus's size, and a bar), `TERM=xterm-256color`. The
pane programs are this binary's (`__echo`, `__flood`, `__serve`,
`__fill`).

| Part | How |
| --- | --- |
| latency | `__echo` answers each key `a`–`z` with its circled letter; a key is timed from the write to the client's PTY to the read that brings the glyph back. Keys 17 to 25 ms apart (past fux's 16 ms paint interval), idle and beside a `__flood` pane. Median, p99, mean, max, misses (no echo in 2 s; ten in a row end the run), bytes and paints per key |
| throughput | `__serve` writes, when asked, a reset, the workload, and a marker no workload has; timed until the client has the marker. Every corpus recording and synthetic workload (16 MiB); bytes sent and paints |
| footprint | with a shell: server and client CPU over 10 s; server memory, then with 4 more panes, 4 more with full 10,000-row histories, and 50 shells |

It writes `bench/target/fux-bench/feel.json` and prints a summary.

Compare multiplexers within one run, not across runs: wall times wander.
One recording's throughput mostly reflects how its output was split into
reads (fux paints at most every 16 ms); the 16 MiB workloads are throughput
proper. Paints are counted by the `CSI ? 2026 h` fux begins each with, which
tmux and zellij do not send, so only fux's are counted.
