# Code audit: fux, fux-vt and fuxix

A reading of the three crates as a whole, at `51f75d7` (after PR #107), and what came of it. Every source file was read line by line: the reading was split by area among six passes, each told only what a reader needs (to understand a part alone, to change it without breaking what it does not mention, to trust its names and comments), not what to look for. Each finding below names its file and lines. Those acted on were checked again against the code before they were changed; the commit that changed each one is given beside it.

**The rule for every change:** no behaviour changes, nothing gets slower (`fux-bench --against main`), no memory figure grows (`run.sh footprint`). A finding whose fix would change behaviour is left, as a follow-up, with what it would change.

## What the code does well

These are worth keeping as they are; several make the cleanup below safe.

- **Every fast path keeps its general path as an oracle, and a test holds them equal.** fux-vt's `fast_paths_equal_the_general_path*` compares whole parser state; `lay_out_runs` against `lay_out_cells`, `erase` against `erase_reference`, `compact::Line` against `cell::Line`, `range_eq` against `range().eq()`; in fux, `composing_in_part_is_composing_whole`, `echoes_painted_the_short_way_show_what_whole_paints_show`, and the session fuzz target. Without these, much below could not be touched.
- **No panics anywhere**, by `get`, `checked_*`, `try_reserve`, and `copy_from`/`copy_within` as the only checked wrappers, as `clippy.toml` records. Resize builds its replacement before touching the original.
- **Comments say why, and cite their sources:** DEC STD 070 pages, xterm function names, ctlseqs, the kitty spec, Ghostty's files. Departures from the references are named in fux-vt's README, each with a replay.
- **Layouts are pinned at compile time** (`Cell` 32 bytes, `Compact` 8, `Kept` 24, `Meta` 24), each bit map documented beside it.
- **Error words are pinned by tests** (`command_errors_keep_their_words`, `usage_errors_keep_their_words`, the JSON byte-for-byte test); each failure is its own variant with `Display` and `source` written out.
- **fuxix:** one unsafe operation a block, each with a specific SAFETY comment; the macOS PTY workarounds documented down to xnu's functions, with deterministic tests.
- **unicode.rs** maps its state machine rule by rule to UAX #29 and is tested against GraphemeBreakTest.txt and unicode-segmentation for every code point. **style.rs** explains its whole design before its code.
- **Small, single-purpose modules where it counts:** `view.rs`, `words.rs`, `json.rs`'s writer, `layout.rs`'s arithmetic, each saturating step with a short reason.

## Findings

Ranked by what they cost a reader. **Act** means changed in this PR; **leave** gives the reason. Ids are by area: S storage (fux-vt grid, cells, history), P parser and screen, K keys, X server, client, protocol and fuxix, R render, overlay and copy, M session, commands and config.

### 1. Comments and docs that say what the code does not do

The costliest kind: a reader trusts them, and they are wrong. All are fixed by rewording, with no code change.

| Id | Where | What | Decision |
| --- | --- | --- | --- |
| X1 | `src/server.rs:17-18` | `PAINT`: "at most one per client per this"; echoes and quiet panes are painted sooner | act |
| X11 | `src/client.rs:1-2`, `src/outer.rs:24-27`, `docs/rewrite-design.md:99, 277-285` | The client "a dumb pipe"; it now hands its terminal over and only watches | act |
| X10 | `src/outer.rs:101-104` | `TITLE_PUSH`: "the client pops it … (`client::LEAVE`)"; `LEAVE` has no pop, and the server pops it when it gives a terminal back | act |
| X14, X15 | `src/server.rs:149, 236-239`; `src/session.rs:36-37` | Buffers and `send_stream` described as before the hand-off; `Outgoing::Bytes` "(OSC 52)" carries queries, the bell and kitty pushes too | act |
| X4 | `src/server.rs:720` | `serve_conn`'s doc line left on `serve_tty`; `serve_conn` has none | act |
| X9, X12 | `src/protocol.rs:757`; `src/client.rs:563-564` | A test comment names `Frame::chunked`, which does not exist; `pump`'s "every other frame is decoded, so a bad one is an error, and ignored" | act |
| P1 | `fux-vt/src/screen.rs:17-18, 362-365, 642-643, 686-687`; README rows | "State only: fux-vt encodes no keys / reports nothing"; `encode_key`, `encode_mouse` and `encode_focus` read these modes | act |
| P2 | `fux-vt/src/link.rs:7-9` | "erasing … never touches the arrays"; a whole-row erase unlinks the row (`grid.rs`, `erase`) | act |
| P3 | `fux-vt/src/screen.rs:12, 315, 320-323` | `UNKNOWN` documented for the pen style, which is set eagerly; only the blank style is lazy. `pen_table`'s doc does not parse | act |
| P7 | `fux-vt/src/screen.rs:1743-1748` | Two NEL comments side by side, "to the first column" and "as CR takes it"; it goes to the left margin | act |
| P8, P9 | `fux-vt/src/parser.rs:221-223, 346-347, 599-601` | `osc_limit` and `OSC_PAYLOAD_LIMIT` docs leave out the palette and the zero limit; `Options::palette` predates host colours | act |
| P25 | `fux-vt/README.md:139` | `ByteQueue` "the bounded byte queue"; it has no bound | act |
| S4 | `fux-vt/src/grid.rs:1546, 1560-1562`; `grid/tests.rs:64-76`; `clippy.toml:7-19`; `fux-vt/src/bytes.rs:9-10` | `move_row` spoken of as a deque's two slices (`Order` is a vector and an index); `clippy.toml` and `bytes.rs` say `copy_within` is called in one helper alone, and `move_row` calls it directly; the `rotate_*` rationale names calls that no longer exist | act |
| S8 | `fux-vt/src/lib.rs:171-176` | `Row::text_len` "bytes of text the row keeps"; it counts only clusters over 17 bytes | act (doc only; the value stays) |
| S9 | `fux-vt/src/grid.rs:592-595` | `mutate_row`'s contract, the invariant `used` depends on, ends in a sentence that does not parse | act |
| S20, S29 | `fux-vt/src/cell.rs:129-131`; `grid.rs:111-113` | The `flags` bit comment garbled; "the two flags fit where the struct had padding", of three | act |
| S24 | `fux-vt/src/grid.rs:1589, 1599-1600` | `resized` says how, not what (a resize without reflow), and cites "hunt 8 finding 017", which the repo does not hold | act |
| K2, K3, K4 | `fux-vt/src/keys.rs:27, 102-104, 174-176` | `KeyPress`: "`A`, not `S-a`", but `S-a` becomes `a`; `Kitty::shifted`'s doc copied from `base`; `Direction::name` "`Left`" returns `left` | act |
| K11 | `fux-vt/src/keys/decode.rs:57-58, 189-190, 205, 222-223` | `deadline`, `expect`, `REPLY_WINDOW` and `waiting` describe less than the code does | act |
| R1, R2 | `src/render.rs:1-3, 624-626` | "send only the changed runs, inside synchronized output" (the echo is not); `compose_into` "neither allocates when used again" (it does) | act |
| R7, R10 | `src/render.rs:1466, 1544-1546` | `paint`'s doc sits on `same_keys`; `echo`'s doc leaves out the CUP it may write, and the default SGR it assumes every paint leaves | act |
| R11, R23 | `src/render.rs:2051-2054, 2566-2570`; `src/overlay.rs:132, 962, 1062, 1695` | Two test docs joined onto the wrong tests; overlay docs naming a Ctrl-U the prompt lacks, leaving out `q`, and calling any pane "the focused" | act |
| M5, M6, M19 | `src/pane.rs:161-162, 274-276`; `tests/support/mod.rs:27-31` | Doc lines left on the wrong items (`end_of`, `short_temp_dir`); a comment on the arm above the one it is about | act |
| X24 | `docs/rewrite-design.md` | Marked historical, still extended, and stale: `pre_exec` and `setsid` (a launcher now), macOS and `O_CLOEXEC`, three poll deadlines of seven, fuxix 0.1.6, lints that "warn" | act: the stale lines are marked or corrected, the document stays historical |
| P17, K16 | `screen.rs:884, 1136, 1377`; `parser.rs:731`; `decode.rs:38-39, 1408`; `encode.rs:474, 698`; `grid.rs:2875` | Reasons given as history ("as it always did", "as before"), which a reader cannot check | act: the present reason stated |

### 2. State spread across places, and rules no one wrote down

| Id | Where | What | Decision |
| --- | --- | --- | --- |
| X2 | `src/server.rs` `Conn`: `painted`, `next_paint`, `settled`, `echo`, `starved` | When a client is painted is decided by five fields written in six places (`paint`, `read_pane` twice, `serve_tty`, `read_conn`, `frame`) and read in two | act: gathered into one type with named transitions |
| X3 | `src/server.rs:85-100, 128-132` | The handed-over terminal as loose fields (`tty`, `tty_out`, `title_saved`), with two unwritten rules: one is held only while a client is attached, and every `Exit` gives it back (why the stop and close checks may look only at `out`) | act: one type, and the rules written on it |
| X7 | `src/server.rs:776-782, 880-884` | The same "note the echo pane" block after both kinds of input | act |
| X20 | `src/server.rs:342-389` | Nothing says why a `Slot`'s index into `conns` stays good for a tick | act: a comment |
| M16 | `src/layout.rs:340-346`; `src/session.rs:2236-2242` | `swap` uses `PaneId(u32::MAX)` as a placeholder, safe only because `advance` never gives that id out; neither side says so | act: a comment on both |
| R5 | `src/render.rs:153-216, 393-396` | `draw_row`'s `tiled` and "onto blank cells": in the memo path the cells hold the last frame's row, correct only because live rows are the screen's width, which is written nowhere | act: written down |
| R4 | `src/render.rs:183-187, 1532` | `changed_rows` and `changed_rows_between` sound related and are not; its `whole` means the opposite of "composed whole" elsewhere | act: renamed |
| S19 | `fux-vt/src/grid.rs:262-266` | `left` and `right` public, kept with `lr` only by a comment | leave: nothing writes them directly today; making them private changes many reads for no reader gain beyond the comment |
| P4, P5 | `fux-vt/src/screen.rs:456-510, 1765-1832, 1990-2030`; `228-231, 2165-2191, 2236-2263, 2362-2453` | RIS lists every field of `new` again by hand; DEC private modes are numbered in four lists, 1004 a hidden exception | leave: restructuring RIS risks behaviour for a readability gain the tests cannot fully guard; P5's doc on 1004 is fixed with section 1 |
| X6 | `src/view.rs:111`, `src/server.rs:153, 257-264`, `src/client.rs:154-162` | Whether the title was saved is tracked three times, two by rescanning every paint | leave: scanning only the bytes that can carry the title would be identical, but subtle; a follow-up |

### 3. Duplication

| Id | Where | What | Decision |
| --- | --- | --- | --- |
| R3 | `src/render.rs:359, 1534, 1674` (memo), `1537, 1682` (row and links), `1581, 1693` (cell) | "Is this row unchanged?" written three times | act: one helper, measured |
| X5 | `src/client.rs:452-500, 503-572` | `watch` and `pump` share their signal, poll and relay code, and have drifted | act |
| M8 | `src/session.rs:1210-1221, 1231-1241` | `output` and `release_frames` repeat the same five steps before a read | act |
| M9 | `src/session.rs:704-733, 1881-1898` | A workspace built twice; the default tab name `"main"` in three places | act |
| S10 | `fux-vt/src/grid.rs:607, 644, 674, 694, 1378, 1019` | `version` and `used` updated by hand six times, the `used` invariant held by each copy | act, measured |
| X8 | `src/protocol.rs:161-176, 335-393, 199-203` | Frame kinds and role bytes numbered twice each; `Frame::Terminal` is in neither encoding test | act: named constants, and `Terminal` added to both tests |
| K5, K6 | `decode.rs:500-513, 760, 773-780`; `encode.rs:241-254, 267, 505-523` | The CSI final-letter table four times, the `CSI n ~` numbers three; `ss3` decodes CSI finals too | leave the tables (hot on both sides; shared tables change codegen); act: cross-references, and `ss3` renamed |
| K7, K8 | `encode.rs:270-272, 480-484`; `decode.rs:162`, `encode.rs:62` | `legacy` recomputes `xterm_bits` instead of calling it; `PASTE_END` defined twice | act, measured |
| K9 | `decode.rs:303-328, 531-547, 570-582` | How a string ends, written three times with its comment | leave: the three differ in BEL and their limits, and are cold; a shared helper reads no better |
| P14, P15 | `parser.rs:521-534`, `palette.rs:447-479`; `lib.rs:32-71`, `palette.rs:277-311` | OSC 10-19 read twice; two fixed-buffer reply builders | leave: palette replies are measured hot (`Answer` exists because formatting cost more) |
| R14, R15 | `src/render.rs:194-216 / 687-716`; `824-842 / 1261-1286` | Pane row set-up copied between the full and memo paths; the panel's "n more" windowing twice | act for R15; leave R14 (hot, small) |
| S11, S13 | `grid.rs:1998-2023 / 2084-2112`; `cell.rs` / `compact.rs` `Line` | Reflow's two paths and the two `Line`s share code by design, held equal by tests | leave: each is the other's oracle |
| X23 | `src/outer.rs:55-99` | The palette question bytes written three times | leave: constants; a test pins their order |
| K24, M20 | `src/input.rs:84, 189` | `"64 KiB"` written out beside `PASTE_LIMIT`; focus bytes beside `encode_focus` | act for the limit; leave the focus bytes (a borrow keeps the helper out) |

### 4. Names that say something else

| Id | Where | What | Decision |
| --- | --- | --- | --- |
| S2 | `fux-vt/src/grid.rs:2709, 2898` | A private `struct Copy` hides the `Copy` trait, forcing `std::marker::Copy` | act: renamed |
| S3 | `fux-vt/src/grid.rs:8, 2263, 2166` | Two `trimmed`s in one file, one a method meaning something else | act |
| S5 | `grid.rs:901`, `history.rs:302` | `make_room` frees links in one, reserves cells in the other | act |
| S6 | `fux-vt/src/grid.rs:476` | `kept_slot` looks up screen slots ("kept" is history's word) | act |
| S7 | `fux-vt/src/grid.rs:219`, `lib.rs:93` | Fields named `spill` hold a `Text`, of which `Spill` is half | act (all crate-private) |
| R9 | `src/render.rs:976, 1023, 1043-1046` | `Axis::Horizontal` read as "a vertical line"; `h` a separator, then a rect | act |
| M15, M23 | `src/layout.rs:65, 76, 511-544` | `Rect::at(y, x)` but `contains(x, y)`; a variable `x` holding y positions | act for M23; leave M15 (public; callers in tests and fuzz) |
| K23 | `src/input.rs`, `src/server.rs` | "escape" for every decoder deadline, answers included | act: the docs say so |
| R26 | `src/copy.rs:54` | `pub struct Copy` shares the trait's name | leave: public in the published `fux` crate, used by `diff` |

### 5. Code that cannot run, or does nothing

| Id | Where | What | Decision |
| --- | --- | --- | --- |
| M4 | `src/pane.rs:322-323` | `Pane::shell`: set, never read; its doc names quoting done elsewhere | act |
| M7 | `src/session.rs:1614-1621` | A loop marking every view dirty after `set`, which `run_command` already does | act |
| M10 | `src/session.rs:1899` | A `MoveTo::Beside` arm after `Beside` has returned | act |
| R8 | `src/render.rs:366-368, 377-378, 1693-1697` | Branches for grids of different sizes no caller can reach | act, measured |
| R16 | `src/render.rs:866, 874` | `grid.cursor = None` where it is already none | act |
| S17, S23 | `fux-vt/src/grid.rs:858-863, 1231` | `adopt_links`' store-and-take round trip; `colour` re-checks what both callers checked | act |
| P21 | `parser.rs:1045`; `screen.rs:1046-1052, 1259-1262, 1284` | An arm after an early return, a rebinding for nothing, `if width != 0` then `if width == 0`, a comment on the wrong line | act (the arm keeps a comment; the wildcard lint needs it) |
| K14 | `fux-vt/src/keys/decode.rs:484-486` | Arms for steps `single` never returns | act: a comment (the wildcard lint needs them) |
| S12 | `cell.rs:648-650`, `compact.rs:559-561, 579-581` | Unreachable `continue`s leave a cell pointing into the buffer just taken | act: blank as the sibling branch does |
| R13, R22 | `src/render.rs:96-99, 743-746`; `src/copy.rs:413-416` | One condition computed twice; an empty needle checked twice | act |

### 6. Speed attributes without a reason

| Id | Where | What | Decision |
| --- | --- | --- | --- |
| S22 | `grid.rs:48, 757, 1050`; `history.rs:300-301, 367-368, 378` | `inline(always)`, `inline(never)` and `cold` with no reason; `make_room` cold under a cold caller | act: each measured, removed if it does nothing, its reason written if it does |
| P16 | `screen.rs:445, 1145, 1844, 1879, 1889, 1904` | Out-of-line functions without a reason, or with one kept in another file | act, as S22 |

### 7. Large files and long functions

| Id | Where | What | Decision |
| --- | --- | --- | --- |
| S1 | `fux-vt/src/grid.rs` (2939 lines) | Storage, reflow (`1722-2275`, `2573-2843`) and the cursor's rules (`2390-2565`) in one file | act: reflow moved to `grid/reflow.rs` |
| M1 | `src/session.rs` (2727 lines) | The model, repair, teardown, output, a 270-line `execute`, `ls` and capture | leave: commands share most of `Session`'s private helpers; a split moves code without making one part readable alone |
| R6, R17, R21, M24, M25 | `render.rs:627-880, 1657-1779`; `copy.rs:618-803`; `config.rs:456-521`; `session.rs:1414-1441` | Long functions doing several jobs | act for R6's panels; leave the rest, each a single dispatch whose arms read in order |

### 8. Tests

| Id | Where | What | Decision |
| --- | --- | --- | --- |
| T1 | `fux-vt/src/{cell,compact,grid,parser}/tests.rs`, `src/render.rs` | The test generator (splitmix64) written five times inside the two crates | act: one copy a crate, the sequences unchanged |
| R24, M27 | `src/overlay.rs:994-1001`, `src/outer.rs:463-469`, `render.rs`, `session.rs`, `copy.rs` | A test session built by hand some twenty times | act |
| M28 | `tests/*.rs` | `focused()` three times, `painted()` twice, and other helpers outside `support` | act |
| R25 | `render.rs:1803`; `overlay.rs:1196, 1703` | Test helpers shadow real functions of the same name | act: renamed |
| S28 | `fux-vt/src/grid/tests.rs:17` | `…resize_peak_include_metadata` asserts only the plateau | act: renamed to what it checks |
| M29 | `tests/lifecycle.rs:49-87` | A test server started without the lock `support` says every start takes | act |
| M30 | `tests/frames.rs:26-36`; `attach.rs:100`; `pressure.rs:157, 244-251, 316-318` | Margins under a second that a loaded machine can miss | leave: none has failed in CI; widening them slows the suite. Noted |

### 9. Behaviour: follow-ups, not changed here

Each would change what fux or fux-vt does, which this PR does not.

| Id | Where | What it would change |
| --- | --- | --- |
| K1 | `fux-vt/src/keys/decode.rs:207-219` | **A bug.** While an over-long OSC or DCS answer is dropped, `pending` is empty, so `deadline` gives `ESCAPE_DELAY` (35 ms), not `REPLY_DELAY`; after it the rest of the answer is decoded as keys, against the module's own doc. The fix: `REPLY_DELAY` while `discarding` |
| K19 | `encode.rs:526` | `legacy` sends a lone ESC for F13 and above |
| K20 | `decode.rs:782-784, 804` | `CSI u` with code 0 or 1 decodes to a control character no key name can express |
| K21 | `encode.rs:178`, `decode.rs:865-883` | The kitty keypad range: encoded to 57427, decoded to 57426 (KP_BEGIN dropped) |
| X16 | `src/server.rs:738-748, 971-977` | A resize read from the terminal and then the client's `Resize` for the same size repaint twice (kept so the client's `Resize` behaves as before) |
| X17 | `src/server.rs:958-963`, `src/client.rs:437-446` | The client waits without a limit for `Frame::Terminal` |
| X18 | `src/server.rs:177-181` | A failed write to the terminal drops it, and keys are then read by no one |
| M11, M14 | `src/command.rs:729-756, 346`; `config.rs:652`, `command.rs:430, 595` | `capture-pane foo` says "unknown flag"; `NotLines` promises `-N` and plain `N` is taken; undocumented aliases (`clipboard write-only`, `list`, a bare client number) |
| P10 | `fux-vt/src/screen.rs:1694-1717` | DECSC keeps the cursor per screen and the pen shared; may differ from xterm (not checked) |
| K12, M32 | `decode.rs:738-742`; `src/input.rs:207-208` | Allocations per CSI key and per mouse event (faster, not different; left for a measured PR of their own) |

## What was not read closely

- `fux-vt/src/unicode/tables.rs`: generated by `fux-vt/gen`, and says so.
- The packages beside the crates (bench, walk, fuzz, diff, fux-vt's compare and gen) were read only where a finding led to them.
- The integration tests were read for their helpers and timing, not test by test.

## Changes made

Filled in as each finding is acted on, with its commit.
