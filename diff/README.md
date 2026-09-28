# fux-diff: fux beside its last release

An excluded workspace with its own lockfile. It builds two copies of fux into
one binary:

- the code in this repository (`fux`, `fux-vt` and `fuxix` by path);
- the last release of each, from crates.io (`baseline`, `baseline-vt` and
  `baseline-ix` in `Cargo.toml`).

It feeds both the same random inputs and compares everything each gives
back. It is for a change that means to leave behaviour as it was: a
refactor, a speed-up, a smaller or more idiomatic rewrite. It is not built or
run by fux's own gates.

Its only dependencies are fux, fux-vt and fuxix, twice each. It forbids the
same lints as fux.

## Commands

From the repository root:

```sh
cargo build --release --manifest-path diff/Cargo.toml

# Every area, at the default number of cases (about a minute of CPU):
diff/target/release/fux-diff

# Some areas, ten times as many cases, another seed:
diff/target/release/fux-diff --scale 10 --seed 7 sessions layout

# What each area compares:
diff/target/release/fux-diff --list
```

- Each area prints what it found alike and how much it compared.
- A difference prints the area, the seed, what differed, both sides, and
  what led to it (a session's last 40 events, a tree and the changes made to
  it, a terminal's output, and so on).
- The exit status is 1 if any area differed.
- Each area draws from a seed of its own, derived from `--seed`, so running
  one area alone replays it exactly.
- A release build checks arithmetic for overflow, as a debug build does.

## Areas

| Area | What both are given | What is compared |
| --- | --- | --- |
| `sessions` | Whole sessions of random events: keys through every mode and overlay, pastes (too long, and past a pane's input limit), commands with and without a client and with targets that exist and do not, program output, resizes, attaches, detaches, exits, input read by the program, paints and shutdowns | After every event: what it returned, and every view (mode, notice, focus, `dirty`, maps), pane (name, title, size, queued input), workspace and tab (names and layout trees), the outbox, the paste buffers and the bindings |
| `screens` | The same sessions | Everything in `sessions`, and after every event the paint of each client's composed screen, from nothing and from its last screen, byte for byte |
| `commands` | Random command lines, half of them `select-*` lines | The command or usage error, the usage message, the label, and every word read as each kind of target |
| `layout` | Random trees, normalized and not, with weights from 0 to `u32::MAX`, in areas from empty to `u16::MAX` wide, on the screen and past its edge | Placements (panes and separators), neighbours in every direction, and resizes, splits, removals and swaps: what each returns and the tree it leaves |
| `protocol` | Random frames, up to twice the largest payload; random byte streams of frames, broken frames, stray headers and bad lengths | Encoded bytes (`encode`, `encode_into`, each stream's frames, `encode_input`); frames, errors and bytes held, decoding whole and in pieces; `check`; raw frames' paints, inputs and decodings |
| `terminal` | fux-vt fed random output (well-formed sequences, and some hostile or broken ones) in pieces, with resizes, with events and extended replies on and off | After every piece: replies, events and errors, every retained row's identity, version, wrap flag, place and cells, the cursor, every mode, the scroll region, and the rows changed since the last look |
| `input` | Random client input in pieces, with Escape timeouts; random key names with modifiers; pastes; held command lines; pushes and reads on a pane's input queue | Decoded keys, pastes and focus changes, and the Escape deadline; each key's bytes in both cursor modes; each paste plain and bracketed; when a held line is due; the queue's results and contents |
| `copy` | Panes' screens and history from random lines; searches from random places, both ways; every copy-mode error in a session | Matches, the text each kind of selection copies, row positions; each error's outcome and notice (and that each is still reached) |
| `text` | Random lines of words, key names, config lines and files | `split`, `quote`, `join` and `shell_line` with and without fish; key names read and printed; each config line's result and the configuration left; config files; JSON strings and base64 |
| `system` | errno values, nonblocking requests on pipes, sockets and PTYs, PTY sizes, pipes | fuxix's errnos, results and reads that do not wait, window sizes, bytes through a pipe; the config file's path in every mix of `XDG_CONFIG_HOME` and `HOME`, each in a process of its own |

## When the two differ on purpose

- **A change meant to alter behaviour.** Expect a difference in the areas it
  touches. Run the others, which should still agree.
- **A change to fux's library API.** A renamed type or variant, for example,
  can stop the area that uses it from building for both copies. The code for
  both is one macro per area (`stack!`); give the current side what it needs
  there, and compare what both give back in the same form.
- **After each release,** bump the three `baseline` versions in `Cargo.toml`
  to it, and `cargo update -p` them.

## Not covered

- **The server's connection handling.** Reading and handling client frames,
  refusing connections, and painting on a timer all live in `server.rs`,
  behind a real socket and real processes. The walk (`walk/`) and the
  integration tests cover them.
- **Performance.** Timing isn't compared; this compares only what fux does.
