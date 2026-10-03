# fux-bench: fux's speed against main, in instructions

An excluded package with its own lockfile. It runs fixed workloads through
fux and fux-vt, in process, and counts the instructions each retires, on
the working tree and on another commit (`main` by default), in the same
run on the same machine. So no baseline is stored, and other work on the
machine does not move the figures as it moves times.

It is not built or run by fux's own gates or by CI. It forbids the same
lints as fux, and builds with fux's release profile (`lto = "thin"`,
`codegen-units = 1`): with any other, inlining differs from fux's own
build, and the counts with it.

## Commands

From the repository root:

```sh
# Every workload, the working tree beside main (about a minute and a half):
cargo run --release --manifest-path bench/Cargo.toml -- --against main

# Beside another commit, more repeats, only some workloads:
cargo run --release --manifest-path bench/Cargo.toml -- --against v0.17.0 --repeats 9 --only paint/

bench/target/release/fux-bench list          # the workloads
bench/target/release/fux-bench time          # each one's thread CPU time here, and MB/s
bench/target/release/fux-bench run vt/ascii  # one, once, as --against counts it
bench/target/release/fux-bench info          # MB/s beside Ghostty and alacritty, and fux-diff --speed
```

`--against` exits 1 if a workload is flagged, 2 on an error. It writes
`bench/target/fux-bench/against.json`, and `info` writes
`bench/target/fux-bench/info.json` (`--json FILE` for another place).

## The workloads

| Workload | What runs | Fed |
| --- | --- | --- |
| `vt/NAME` | fux-vt alone, set up as fux sets up a pane (`fux::pane::OPTIONS`), 10,000 rows of history | 4 KiB at a time, as `fux-vt/compare`'s `bench` feeds it |
| `pane/NAME` | `fux::pane::Pane::output`: frames held in synchronized output, titles, replies | 64 KiB at a time, as fux's server reads a pane |
| `vt/corpus:NAME` | fux-vt alone on one recording, at its size, over and over to 4 MiB | 4 KiB at a time |
| `paint/corpus` | a session (`fux::session::Session`, without processes) with one client, whose pane is each recording's size; after each read, the client's screen composed (`render::compose_into`) and painted (`render::paint_into`), as the server does | every recording in turn, to 4 MiB |
| `paint/split` | the same, two panes side by side given the same output | the same |
| `decode/legacy`, `decode/kitty` | fux's decoder of a client's terminal, on the keys typed in the recordings, as a legacy terminal sends them and as a kitty-protocol terminal does with the flags fux pushes (5) | each step's keys as one read, the Escape deadline passing after it, to 1 MiB |
| `encode/legacy`, `encode/kitty` | those keys encoded for a pane, in legacy mode and with every kitty flag | to 2^20 keys |

`NAME` is a synthetic workload, made as `fux-vt/compare`'s `bench` makes it
(`src/synthetic.rs` copies its generators: ASCII, dense and medium cells,
cursor motion, scrolling, a scroll region, Unicode), 16 MiB on a 50×200
screen; or `corpus`, every recording in `fux-vt/compare/corpus/` in turn,
each at its own size, to 4 MiB. A recording added to the corpus is a
workload at once.

Each workload has a **baseline**: the same run without the part measured.
For most that is making or loading the bytes; for `paint/*` it is also
feeding the session, so that what is left is composition and paint alone.
`paint/*` also report the frames painted and bytes sent per frame, in the
JSON.

## How `--against` counts

1. REF is checked out in a worktree under `bench/target/against/run`,
   removed after the run (one left by a stopped run is removed at the
   next). This crate's source, as it is in the working tree, is copied into
   it and built there, with REF's toolchain file, so both sides run the
   same workloads, each against its own fux, fux-vt and fuxix. They are
   two binaries: one cannot link two copies of a crate of one version.
   REF's build directory is kept (`bench/target/against/target`), so a
   later run builds only the crates under test again.
2. The working tree's side is the binary running, so run it through `cargo
   run --release`, which builds it from the working tree first. A debug
   build refuses.
3. Every workload and its baseline runs as a child process of its own,
   counted:
   - **macOS:** `/usr/bin/time -l`'s `instructions retired`;
   - **Linux:** `perf stat -e instructions:u`, where perf may count (with
     `kernel.perf_event_paranoid` at 2 or less), else valgrind's cachegrind
     (`I refs`), which takes fifty times as long or so.
4. `--repeats` times on each side (default 5), REF first in even repeats
   and the working tree first in odd ones, on as many threads as the
   machine has (`--jobs`): counts do not depend on what else runs.
5. A side's count is the fewest instructions of its runs less the fewest
   of its baseline's. On macOS the count includes what the kernel does for
   the process (interrupts, preemption, page faults), which only adds, and
   adds more on a busy machine: the fewest is the least disturbed. The
   **noise** is the spread of a side's runs over its count.
6. A workload is **flagged** when the working tree retires more than
   `--threshold` percent (default 3) more than REF, and more than either
   side's noise. A workload whose noise is above the threshold is listed as
   too noisy to judge.

Measured on a 12-core Mac under a load average of 80 to 200 from other
builds, the working tree beside an identical `main`, three runs: every
workload within 0.8% (mostly within 0.2%), noise at most 1.7%. The whole
`cargo run … --against main`, building both sides with REF's build
directory empty, took 65 s; REF builds in 23 to 77 s cold and 7 to 20 s
warm, and counting takes 11 to 15 s. A slowdown put in on purpose (a
pass over every byte in `Parser::process_with`) was flagged on every
`vt/` workload, at 4 to 12%, and on no other.

## `info`

Informational figures, not compared against REF:

- `fux-vt/compare/run.sh bench --engines ghostty,alacritty --mb 4`: MB/s
  for fux-vt beside Ghostty and alacritty, by wall time, best of 3. The
  first run builds `fux-vt/compare` (Ghostty by Zig: minutes).
- `fux-diff --speed`: fux-vt's parse time beside its last release.
