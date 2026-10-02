# fux-vt-ghostty: fux-vt beside Ghostty

An excluded package with its own lockfile. It links two terminal emulators
into one binary:

- fux-vt, by path: the code under test;
- Ghostty's terminal core, through `libghostty-vt` (Rust bindings over
  Ghostty's C API), built from Ghostty's source by Zig.

It gives both the same output, and after every piece of it, and every
resize, compares what each shows. A difference is shrunk to the smallest
case that still shows it and printed with the command that replays it.
It is not built or run by fux's own gates: it needs Zig and a Ghostty
checkout, which `run.sh` fetches.

Ghostty is a reference, not the truth. Where the two differ, the difference
says where to look. xterm's behaviour and the standards decide which is
right, and some differences are choices fux-vt makes on purpose. Each family
below records which.

## Running it

```sh
fux-vt/ghostty/run.sh                    # families expected to agree, 20000 cases (~3 s)
fux-vt/ghostty/run.sh --list             # every family, what it covers, its status
fux-vt/ghostty/run.sh cases              # the named cases, one per behaviour
fux-vt/ghostty/run.sh survey             # each family alone: how often it differs, and the smallest case
fux-vt/ghostty/run.sh run --family sgr --family text --cases 2000 --seed 7
fux-vt/ghostty/run.sh run --all          # every family
fux-vt/ghostty/run.sh replay --size 1x5 'abcde\x08X'
fux-vt/ghostty/run.sh --no-reflow        # fux-vt set up as fux sets it up
fux-vt/ghostty/run.sh --cargo test       # any cargo subcommand, in the same environment
fux-vt/ghostty/run.sh --cargo clippy --all-targets -- -D warnings
```

The first run fetches Zig 0.16.0 and Ghostty into `~/.cache/fux-vt-ghostty`
(or `$FUX_VT_GHOSTTY_CACHE`), and building libghostty-vt takes about a
minute. After that, `run.sh` only rebuilds what changed. Once it has built,
`fux-vt/ghostty/target/release/fux-vt-ghostty` can be run directly.

- `run` exits 1 if any case differed, and prints up to five distinct shrunk
  cases. Each has its families, the `replay` command, what differed and
  both screens side by side (`.` blank, `_` cursor, `↩` soft-wrapped, `≠`
  a row that differs).
- `cases` exits 1 if a named case in a family expected to agree differs.
- `--seed` replays a run exactly. Every random choice comes from it.

## Setup and pins

`run.sh` pins everything the reference depends on:

| What | Pin | Why |
| --- | --- | --- |
| `libghostty-vt` | `=0.2.1` (and `libghostty-vt-sys` 0.2.2 in `Cargo.lock`) | the Rust bindings |
| Ghostty | `7aa9591746ff` (2026-07-22), through `GHOSTTY_SOURCE_DIR` | the first commit built by Zig 0.16. The bindings pin `a887df42` (2026-07-11), which needs Zig 0.15. Between the two, the C API changed only in the kitty-graphics temporary-file option (built out here: `default-features = false`) and one new data key |
| Zig | 0.16.0 | Zig 0.15.2 cannot link anything on macOS 27 |
| macOS SDK | the newest one before 27, through an `xcrun` shim | Zig's bundled libc++ does not compile against the macOS 27.0 SDK. Zig asks `xcrun --sdk macosx --show-sdk-path`, which ignores `SDKROOT` |

To move to a newer Ghostty, update `libghostty-vt` and check its pinned
commit and Zig version. Then diff `include/ghostty/vt/` between that commit
and the one `run.sh` checks out, before changing `ghostty_commit`.

## What is compared

After creation and after every step:

| Field | fux-vt | Ghostty |
| --- | --- | --- |
| Every visible cell: its text (whole grapheme cluster), width (narrow, wide, wide tail) and style | `Screen::cell` | `Terminal::grid_ref` over the active area |
| Style: foreground, background, underline colour, bold, dim, italic, underline (any style), blink (either speed), inverse, hidden, strikeout | `CellRef` | `Style`, plus a blank cell's own background colour |
| Each row's soft-wrap flag | `Screen::row_wrapped` | `Row::is_wrapped` |
| Cursor and pending wrap | `cursor_position`, whose column is one past the last while a wrap is pending | `cursor_x`, `cursor_y`, `is_cursor_pending_wrap` |
| Cursor visibility, DECAWM, DECOM, alternate screen, DECCKM, DECKPAM, bracketed paste, focus reporting, kitty keyboard flags | `Screen` | modes 25, 7, 6, 1, 66, 2004 and 1004, `active_screen`, `kitty_keyboard_flags` |
| Title | the last `Event::Title` | `title` |
| Cursor position and status reports (`CSI r;c R`, `CSI 0 n`) | `Sink::reply` | `on_pty_write` |
| History: the text and wrap flag of every row fux-vt keeps, against Ghostty's most recent rows | `row_from_bottom` | `Point::History` |

Normalized away, each for a stated reason:

- **A printed space and an empty cell are both blank.** The two store
  spaces differently.
- **The cell after a wide glyph has no text and no style of its own.** It is
  drawn by the glyph before it.
- **Ghostty's spacer at the end of a row is a blank.** It is where a wide
  glyph that did not fit would have started.
- **Underline style and blink speed count only as on or off.** fux-vt keeps
  no underline style, and Ghostty no blink speed.
- **Device attributes and mode reports are not compared.** They name the
  terminal.
- **An empty title on Ghostty's side is not compared.** Ghostty's RIS clears
  its title, while fux-vt reports titles as they are set and keeps none.
- **Mode 2027 (grapheme clusters) is on in Ghostty.** fux-vt always measures
  a cluster as a whole.
- **Ghostty always keeps history**, in bytes (64 MiB). Without scrollback,
  Ghostty leaves a stale soft-wrap flag on the row it recycles when it
  scrolls (`replay --size 1x3 abcd`).
- **Cases that resize keep 10000 rows of history in fux-vt.** A screen that
  grows pulls rows back from history, and only history neither side has
  evicted is the same.
- **Random cases settle the cursor before each resize.** They send
  `CSI ? 7 h`, CR, `.`, CR, so autowrap is on and the cursor sits on a glyph
  (`--newline-before-resize` in a replay). This is because of the choices
  the two make on purpose:
  - With a wrap pending, Ghostty keeps it pending across a reflow; fux-vt
    puts the cursor after the text.
  - After a line that exactly fills the new width, Ghostty moves the cursor
    to a new row; fux-vt leaves it waiting to wrap.
  - Growing pulls blank rows back from history in fux-vt only.
  - Ghostty reflows only with autowrap on.

  Named cases and replays do not settle the cursor.
- **The generators avoid empty SGR parameters within a list.** Ghostty
  ignores a trailing one where xterm reads it as 0 (`replay '\e[2;mX'`).

fux-vt is set up as ratty sets it up: reflow, an identity, the kitty keyboard
protocol, and events for titles. `--no-reflow` sets it up as fux does, which
leaves out the families that need ratty's setup (`kitty`, `reports`,
`resize`).

## Families

`--list` prints them with their current status. A family **agrees** when no
seed tried finds a difference. It **differs** with a recorded reason: a
fux-vt defect still to fix (most from the audit of fux-vt 0.2.0, numbered
F1–F10 in the reasons), a documented fux-vt choice, or a Ghostty quirk. When
a fix lands, run its family alone (`run --family NAME --family text --cases
20000` over several seeds). Once it agrees, set its status to
`Status::Agree` in `src/families.rs`, so every later run keeps it agreeing.

Random cases are 1–3 cells now and then, mostly up to 9×17, and sometimes up
to 40×100. Their history is 0, 3 or 50 rows (10000 with resizes), and they
have up to 10 steps of up to 10 snippets each. The shrinker takes away
steps, snippets, and whole characters of text snippets, never bytes of a
sequence: a sequence cut short is another sequence, from no family.

## Files

| File | What |
| --- | --- |
| `run.sh` | fetch and pin Zig and Ghostty, build, run |
| `src/main.rs` | commands |
| `src/families.rs` | the families: generators, statuses and reasons |
| `src/cases.rs` | the named cases |
| `src/case.rs` | running, generating and shrinking cases; reports |
| `src/snapshot.rs` | what is compared, the comparison and the side-by-side view |
| `src/vt.rs`, `src/ghostty.rs` | each terminal read into a snapshot |
| `src/escape.rs` | bytes as replayable text, and back |
| `src/rng.rs` | splitmix64, as in `diff/` |
