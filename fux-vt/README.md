# fux-vt

A bounded terminal emulator for fux, reflowing on resize only when asked. The fixed-size cell
representation and inherited sequence semantics were informed by Jesse
Luehrs's MIT-licensed implementation; its license is retained in `LICENSE`.
The grid and parser are owned implementations, not wrappers. This document is the
implementation contract; "Verification" says how it is checked.

## Sequence matrix

CSI means ESC `[`. Missing/zero counts default to one unless noted. Coordinates
in sequences are one-based; the API uses zero-based rows/columns. The baseline
is vt100 0.16.2 plus fux's existing reply callback, not every xterm feature.

| Family | Contract/default/reset | Permanent test family |
| --- | --- | --- |
| Printable ASCII / UTF-8 | Printable runs bypass state dispatch in ground state; unicode-width 0.2 supplies widths; incomplete UTF-8 survives calls; invalid input and U+FFFD are discarded like the baseline | `text`, `chunking` |
| Grapheme clusters | A character that continues the extended grapheme cluster of the cell printed just before the cursor joins that cell: spacing marks, variation selectors, ZWJ sequences, flags, Indic conjuncts. Boundaries follow UAX #29 (Unicode 17) as a state machine fed one character at a time, from tables `gen/` generates, never from the text a cell stores; the one departure is that nothing joins after a Prepend character. A narrow cell whose cluster's string width becomes two is widened, the cell under the cursor becoming its second half; in the last column it stays narrow. Anything that moves the cursor or edits a row ends the cluster; SGR, modes and queries do not. A cluster is never split: past `Cell::CLUSTER_CAPACITY` (128) bytes, or past its row's text budget, the rest of it is dropped (a cell always holds a start of its cluster), and what follows lands where UAX #29 puts it | `unicode::tests` (all 766 conformance cases, every assigned code point against unicode-segmentation), `properties::printed_text_is_segmented_as_the_model_says`, `extended::grapheme_*`, `extended::long_clusters_*`, `extended::a_full_cluster_*`, fuzz operation `fd` |
| C0 | BS subtracts a column; HT advances to next fixed eight-column stop, clamped; LF/VT/FF advance/scroll without CR; CR goes to column zero; SO puts G1 in GL and SI G0 (see ESC ( / ESC )); BEL and other unhandled C0 have no visible effect. BS, LF, VT, FF and CR end a pending wrap; HT keeps it (see CSI ? 7) | `controls`, `conformance::a_pending_wrap_*` |
| ESC 7 / 8, CSI s / u | Save/restore position with its pending wrap (DEC STD 070, Appendix D.6.1), origin, drawing attributes, and the character sets with the shift between them, as xterm does; SCOSC/SCORC (`CSI s` / `CSI u`) share DECSC's slot; saved cursor is clamped after resize | `saved_cursor`, `extended::scosc_*`, `conformance::a_pending_wrap_is_saved_*` |
| ESC = / > | DECKPAM / DECKPNM set and clear `application_keypad()` (default off; reset clears it). State only: fux-vt encodes no keypad input | `opt_in::keypad_mode_is_tracked_and_reset` |
| ESC ( F / ESC ) F | SCS (VT520 manual, Tables 5-13 and 5-14; ECMA-35): designate G0 / G1, `0` DEC Special Graphics and any other set ASCII, as xterm reads them in UTF-8 (the national sets draw nothing different there). While GL holds Special Graphics, printable 0x5f to 0x7e print as xterm draws them: a blank, ◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·; other characters are unchanged. `TERM=xterm-256color`'s `smacs`/`rmacs` are `ESC ( 0` / `ESC ( B`. G2, G3 (`ESC *`, `ESC +`), the 96-character sets and single and locking shifts beyond SO/SI are unhandled. RIS and DECSTR designate ASCII into both, G0 in GL | `conformance::dec_special_graphics_draw_lines` |
| ESC M / c | Reverse index / full reset; reset clears both buffers and primary history, modes and attributes, but never restarts row identity allocation | `reset`, `scrolling` |
| CSI ! p | DECSTR, soft reset, as xterm does it: the VT520 manual's table (p. 5-150) and DEC STD 070's Soft Terminal Reset (p. 4-37) show the cursor and reset DECOM, DECCKM, DECKPAM, both screens' scroll regions, the pen, the character sets (ASCII, G0 in GL), and the saved cursor (home, normal attributes); DECAWM goes back to its default, on, as in xterm, where the VT520 table says no autowrap and DEC STD 070 off (see "Departures from the references"). The screen, the cursor and a pending wrap, the alternate screen, bracketed paste, focus reporting, mouse modes and kitty flags stay | `conformance::a_soft_reset_*` |
| CSI A B C D E F G H d f | Relative up/down/right/left, next/previous line, horizontal absolute, cursor position, vertical absolute, HVP (as CUP); inherited margin clamping and origin semantics | `cursor`, `extended::hvp_*` |
| CSI b | REP (ECMA-48 8.3.103): the preceding graphic character printed Pn more times (0 or none: once), wrapping, scrolling and taking the character set as printing it again would. ECMA-48 leaves REP undefined after a control function; as in xterm, there is then nothing to repeat: after any control, sequence or string, REP's own included, REP does nothing until a character is printed. After a grapheme cluster it repeats the character that took the cell, without the marks joined to it, as xterm does (see "Departures from the references"). Copies past those that fill the screen and its retained history are skipped a whole row's worth at a time, which leaves everything as printing them would, so one REP prints at most that many | `conformance::rep_*` |
| CSI @ P X | Insert/delete/erase characters, bounded to the row; no orphan wide halves; the blanks inserted, brought in by a deletion or erased take the pen's foreground and background colours and no other attribute, as xterm's do (`bce`, background colour erase, which `xterm-256color` advertises) | `editing`, `wide_edits`, `conformance::blanks_brought_in_*` |
| CSI L M S T | Insert/delete lines; scroll up/down within margins; counts bounded to affected region; partial-region operations do not add history. The lines brought in, by these and by LF and RI, take the pen's colours (`bce`, as for CSI @ P X) | `scrolling`, `conformance::blanks_brought_in_*` |
| CSI J / K, CSI ? J / K | Erase display/line: absent/0 forward, 1 backward, 2 all; erased cells take the pen's colours (`bce`, as for CSI @ P X); no protected cells | `erase`, `conformance::blanks_brought_in_*` |
| CSI r | Top/bottom defaults 1/height; invalid range resets to full screen; homes to top margin, matching baseline | `margins` |
| CSI ? 6 h/l | Origin mode per buffer; homes on change; defaults off | `margins` |
| CSI ? 7 h/l | Autowrap, defaults on. A glyph printed in the last column leaves the cursor on it with a wrap pending, DEC STD 070's Last Column Flag (Appendix D.6.1; `Screen::pending_wrap`); the next glyph first moves to the start of the next line, marking the row soft-wrapped. A glyph too wide for what is left of the line wraps the same way, and marks its row though the last column stays blank; only on the last row below the scroll region, where the glyph overwrites its own row instead, is nothing marked. `Screen::cursor_position` is therefore always on the screen. Printing, BS, LF, VT, FF, CR, RI, every cursor movement, CUP, ED, EL, ECH, ICH, DCH, IL and DL (where carried out), DECSTBM, DECOM and RIS end it, as the appendix lists; HT keeps it, as xterm and the engines in `compare/` do though the appendix lists HT; SU, SD, SGR, modes and queries keep it. With autowrap off a glyph overwrites the last available cell instead; the flag is still set, and resetting DECAWM keeps it, as in xterm, so a glyph printed after DECAWM is set again wraps | `autowrap`, `conformance::a_pending_wrap_*`, `conformance::a_glyph_that_wraps_*` |
| CSI ? 1 / 25 / 2004 h/l | Application cursor (off), cursor visibility (on), bracketed paste (off) | `modes` |
| CSI ? 1004 h/l | Focus reporting, `focus_reporting()` (off; reset clears it). State only: fux-vt sends no focus reports, and DECRQM does not report it | `focus_and_cursor_shape` |
| CSI Ps SP q | DECSCUSR: `cursor_shape()` records Ps as given, absent or 0 being the terminal's default (reset clears it). State only: fux-vt draws no cursor | `focus_and_cursor_shape` |
| CSI ? 47 h/l | Switch to/from separate alternate buffer without clearing it; primary history retained; alternate has no history | `alternate` |
| CSI ? 1049 h/l | Save cursor/attributes, clear and enter alternate; leave and restore primary cursor/attributes | `alternate` |
| CSI ? 9 / 1000 / 1002 / 1003 h/l | X10 press / press-release / button-motion / any-motion; latest set wins; reset only clears matching active mode | `mouse` |
| CSI ? 1005 / 1006 h/l | UTF-8 / SGR encoding state, latest set wins and matching reset restores legacy; fux still emits legacy bytes for non-SGR, not a new UTF-8 encoder | `mouse` |
| CSI m | 0/reset; 1/bold and 2/dim, kept apart (both can be on, as in xterm; 22 ends both); 3/italic, 4/underline, 21/doubly underlined (an underline: no style is kept), 5/slow and 6/rapid blink (mutually exclusive), 7/inverse, 8/hidden, 9/strikeout; resets 22/23/24/25/27/28/29; 30–37/40–47, 90–97/100–107; 39/49/59 defaults; 38/48/58 (foreground, background, underline colour): `5;n` and `2;r;g;b` read as xterm reads them (values the list ends before are 0; another kind takes only itself), and ITU-T T.416's (13.1.8) colon forms `5:n`, `2:r:g:b` and `2:space:r:g:b`, the colour space ignored, as in xterm; a colour out of range is skipped and the rest of the SGR applies. `4:0` ends underline and `4:1` to `4:5` (kitty's underline styles) set it: xterm ignores them, every other engine in `compare/` reads them, and fux-vt keeps no style | `sgr`, `extended::blink_*`, `conformance::sgr_*`, `conformance::an_invalid_sgr_*`, `conformance::underline_styles_*` |
| CSI 5n / 6n / 0c | Replies `ESC[0n`, absolute one-based cursor report, `ESC[?1;2c`; missing DA parameter is zero; preserve baseline reply coordinates, reporting a cursor waiting to wrap one past the last column; no replies for intermediates/private variants. `Options::identity` changes the DA answer and the column a cursor waiting to wrap is reported at (see "Opt-in outputs") | `replies` |
| OSC / DCS / APC / PM / SOS | Consume without storing payload or drawing it; OSC accepts BEL or ST, others ST; cancellation/recovery follows parser state rules. With `Options::events` only, OSC payloads are buffered (see "Opt-in outputs") | `ignored_strings` |
| Other sequences | Safely parse and ignore; recognized C0 inside CSI still executes; no leakage of ignored string payloads. A complete CSI or escape sequence fux-vt does not implement is given to `Sink::unhandled` (default: discarded): unknown final bytes, intermediates or private markers, ED/EL modes above 2, DSR other than 5/6. Unknown DEC private mode numbers are consumed quietly, and sequences cut short by parser bounds are not reported | `ignored_sequences`, `parser_bounds`, `extended::sequences_*` |

## Opt-in outputs

`Parser::new` uses `Options::default()`: everything below is off, and the
behaviour is exactly the table above. `Parser::with_options` enables each
independently; `Parser::process_with` delivers to a `Sink` whose
`reply`/`event`/`unhandled` methods default to discarding.

| Option | Contract | Tests |
| --- | --- | --- |
| `events` | OSC 0 → `IconName` then `Title`; OSC 1 → `IconName`; OSC 2 → `Title`; OSC 52 `Pc;Pd` → `Clipboard { selection, data }` unless `Pd` is `?` (a query); other OSC numbers produce nothing. BEL executed in ground, escape or CSI state → `Bell` (BEL terminating an OSC is not a bell). Payloads are raw bytes. An OSC string is terminated by BEL or by ESC (the start of ST); CAN/SUB cancel it without an event. At most `OSC_PAYLOAD_LIMIT` (64 KiB) payload bytes are buffered per string; a longer string is consumed with no event and its buffer is released immediately. Events are identical under any chunking | `opt_in::events_*`, `opt_in::osc_payloads_are_bounded_and_cancellable`, fuzz header bit `0x10` |
| `extended_replies` | DECXCPR `CSI ? 6 n` → `CSI ? row ; col R` (same coordinates as DSR 6n, including a cursor waiting to wrap); secondary DA `CSI > c` / `CSI > 0 c` → `CSI > 1 ; 10 ; 0 c`; DECRQM `CSI ? Ps $ p` → `CSI ? Ps ; Pm $ y` with `Pm` 1 set / 2 reset for modes 1, 6, 7, 25, 47, 1049, 9, 1000, 1002, 1003, 1005, 1006, 2004 and 0 (not recognized) otherwise; ANSI DECRQM `CSI Ps $ p` → `CSI Ps ; 0 $ y`. Primary DA and DSR 5n/6n are unchanged | `opt_in::extended_replies_*`, fuzz header bit `0x20` |
| `kitty_keyboard` | Tracks one kitty keyboard flag stack per screen (primary, alternate): `CSI > Ps u` pushes (a full stack of 32 drops its oldest), `CSI < Ps u` pops Ps (default 1), `CSI = Ps ; Pm u` sets (Pm 1 replace, 2 add, 3 remove); flags saturate at 255; `CSI ? u` answers `CSI ? flags u`. `CSI > 4 ; Pv m` sets modifyOtherKeys (Pv 0 or absent: off); other `CSI > m` resources are unhandled. Full reset clears all of it. Read with `Screen::kitty_keyboard_flags` / `modify_other_keys`. State only: the host encodes keys. Off, every one of these sequences is unhandled | `extended::kitty_*`, `extended::modify_other_keys_*`, `extended::without_the_option_keyboard_*`, `properties::kitty_keyboard_flags_agree_with_two_plain_stacks`, fuzz rows-byte bit `0x20` |
| `reflow` | Resize re-wraps the primary screen and its history at the new width: each logical line (soft-wrapped rows and the row ending it) loses its blank tail and is laid out again without splitting wide glyphs (a glyph that does not fit leaves a blank spacer and starts the next row; a later reflow, and a copy, skip that spacer, so reflowing narrower and back gives the text it started with). The cursor stays on its character, or as far past the text as it was, at most waiting to wrap. Surplus rows go blank lines below the cursor first, then the oldest lines into history, then history past its limit; rows below the screen once the cursor's row reaches its top are dropped. A reflowed line's k-th row keeps the identity of its k-th row before, if it had one; every row takes a new version and the resize forces a full refresh. The scroll region is reset. The alternate screen resizes without reflow. Two passes over the rows, no line gathered in memory; transactional like every resize | `extended::*reflow*`, `properties::reflow_narrower_and_back_keeps_every_line`, fuzz rows-byte bit `0x10` |
| `identity` | An `Identity { name, version }`. Primary DA answers `CSI ? 62 ; 22 c`; secondary DA answers `CSI > 1 ; Pv ; 0 c`, Pv the version as `major*10000 + minor*100 + patch`, with or without `extended_replies`; XTVERSION (`CSI > q`, `CSI > 0 q`) answers `DCS > \| name version ST`, unless name and version exceed `Identity::MAX_LEN` (48) bytes, when it is unhandled; DSR 6n and DECXCPR report a cursor waiting to wrap at the last column, as xterm does. Without one, XTVERSION is unhandled | `extended::an_identity_*`, `extended::identity_*`, fuzz rows-byte bit `0x40` |

Cells are read as `CellRef`s (`Screen::cell`, `Window::cell`, `Row::cell`,
`Row::cells`), which find a long cluster's text in its row. A consumer that
stores or transports screen contents keeps them in `Cells`, which holds long
clusters the same way (`row.cells().collect::<Cells>()`, `Cells::set`,
`Cells::set_text`; `properties::cells_agree_with_a_plain_list_of_cells`); `Cell::new` (inline, at most `Cell::INLINE_CAPACITY`, 17,
bytes), `Cell::wide_continuation` and `Attributes::new`/`with_*` build the
rest. Parser output never goes through them.

### Deliberate boundary

Upstream does **not** dispatch DECAWM 7; implementing it is a required,
standards-backed correction, tested independently as well as tracked in the
differential inventory. IL/DL outside scrolling margins are ignored, unlike
upstream's accidental row edits there. Both match XTerm(411)
(`fux-vt/compare`'s xterm engine replays them). Upstream dispatches 47/1049 but **not** 1047/1048;
the latter remain ignored rather than pretending all alternate-screen aliases
are equivalent. ESC D/E/H, CSI g, CSI 3J and
programmable tab stops are not implemented by the baseline and remain ignored
(and reported unhandled); CSI f/s/u are implemented as HVP and SCOSC/SCORC.
ESC =/> set the `application_keypad()` getter, as upstream's did, but fux
never acts on it: there is no keypad encoding. By default there are no
window-title, bell, clipboard, or window-resize side effects from child output,
and no OSC payload is retained. In particular OSC 52 from a child cannot bypass
fux's clipboard policy: fux never enables `Options::events`. Window resize from
child output remains unsupported in every mode.

Excluded: graphics protocols (a host that draws images parses them from the
byte stream itself); key encoding (the kitty keyboard option tracks state
only); history beyond its configured ring; alternate-screen history. New compatibility decisions must
be individually documented and covered, not hidden in broad differential
allowlists.

## Parser

The owned parser follows Paul Williams's DEC ANSI transition model:
https://vt100.net/emu/dec_ansi_parser . Implementation is direct; no parser
crate is used. The UTF-8 terminal policy overrides the historical eight-bit
control interpretation in ground state: raw C1 bytes are invalid UTF-8 and
are discarded, and UTF-8-encoded C1 characters are ignored. Seven-bit ESC
forms remain recognized. Each state documents cancellation, ESC re-entry,
parameter/intermediate overflow and string termination. Parameters saturate
at u16::MAX; at most 32 numeric fields and two intermediates are retained;
overflow moves to ignore-until-final. Ignored strings retain no payload.

A cell is 32 bytes: its attributes and 17 bytes of text, enough for single
emoji with modifiers, flags, keycaps and most accented text. A longer
cluster, up to 128 bytes (Unicode's stream-safe limit, which holds every
emoji sequence), goes in its row's text, which the cell locates. A row's
text is at most 32 bytes a cell and 128 more; overwritten clusters leave
their text until the row runs out of room and is compacted. A cluster that
would not fit even then keeps what fits inline, whole chars, in its cell.
A combining scalar after an empty preceding cell attaches to a space;
at column zero it attaches to the previous row only when that row is
soft-wrapped. Otherwise it is dropped. An over-capacity mark is dropped, not
allocated.
A one-column grid drops wide glyphs without moving or wrapping the cursor.

## Storage, identity and windows

The implementation owns row-major arenas and bounded row-slot metadata for
the primary and alternate grids. Logical order is independent of physical
slot. Primary history is a ring of at most `history_lines` rows; zero disables
it. Full-screen upward scroll retains departing rows; partial scroll,
reverse scroll and insert/delete lines discard displaced rows. Surviving
rows retain their IDs. Recycled slots receive new IDs. A row's version changes
on each edit that changes its cells or its wrap flag, and on no other: erasing
cells that are already blank, or writing a glyph over the same glyph with the
same attributes, leaves the version as it was, while inserting or deleting
characters (CSI @ and P) always counts as a change. An edit never changes a
row's identity. Reset and history clearing invalidate
removed IDs. Identity exhaustion must be an explicit error, never wrap/reuse.

Without `Options::reflow`, resize is not paragraph reflow: rows keep their upper-left cells and columns
past the new width are discarded. Rows move around the cursor, so the line it
is on stays visible: a shrink first discards rows below the cursor, then moves
rows above it into history (bounded by the history limit; the alternate
screen keeps none, so they are discarded); a grow first pulls rows back from
history above, then adds blank rows below. Both cursors move with their rows
and clamp.
History rows retain their original column extent; window reads pad/clip them
without modifying history. Every effective resize clears live soft-wrap metadata, matching the inherited
resize contract; historical wrap metadata is retained.
Wide halves cut by an edit or resize are repaired before exposing the grid.
Zero dimensions are rejected. Allocation uses checked arithmetic and explicit
cell/row caps; errors leave the existing terminal usable. Processing may
retain an already-applied input prefix; even a partially completed scroll
forces every old window mark to refresh. Resize replacement is transactional. Each buffer permits
at most 64 Mi retained cells (32 bytes each, and at most 32 bytes of long
cluster text each) and 1,048,576 retained rows.
Storage grows geometrically only to the configured cap as history fills;
empty history is not eagerly allocated. At capacity, scrolling reuses slots
without allocating. Resize builds replacement storage before swapping it in,
so peak storage can include old and new buffers.

Windows are immutable views with bounded width/height and history offset.
They never mutate a global scrollback setting. Copy uses inclusive endpoints,
normalizes wide continuations to their leaders, joins soft wraps, trims blank
padding at hard ends, and enforces cell/byte caps while constructing output.
Display-only padding beyond a history row's original extent is not copied,
including at soft joins after widening. An explicitly selected trailing
empty row retains its hard line break; the application's whole-pane copy
trims trailing empty rows separately. The conservative cell budget charges
full touched rows at the window width; the byte budget applies before hard
padding is trimmed. `Window::row` exposes underlying retained-row data and
metadata; use `Window::cell` for clipped viewport cells.

Change marks are non-destructive: row versions plus a structural generation
support independent readers. Structural changes (scroll, resize, reset,
eviction, buffer switch) force safe window refresh. Cursor/mode changes are
observable even without cell changes. Stale/exhausted marks must never hide
an update. `dirty_rows_since` walks every retained row; a reader of the
visible screen alone uses `dirty_live_rows_since`, which yields the same live
rows with their places on the screen and reads only the screen's rows. Row caching in fux uses identities/versions, not whole-screen
revision/width snapshots, and full frames still contain unchanged rows.

## Verification

- **Tests** (`cargo test -p fux-vt`): each family of the sequence matrix has
  its permanent tests, named in the table above; `tests/golden` holds the
  expected screens of the corpus inherited from the vt100-crate baseline
  ([`tests/golden/README.md`](tests/golden/README.md) maps each to what it
  covers); `tests/properties.rs` and `tests/invariants.rs` check fux-vt
  against small independent models and its own invariants over generated
  input; `tests/versions.rs` the row identities and versions. Expected
  values are specified independently, never captured from fux-vt.
- **Fuzzing**: the [`fuzz/` package](https://github.com/gold-silver-copper/fux/blob/main/fux-vt/fuzz/README.md)
  documents its nightly toolchain and run commands; its targets check
  parsing in any chunking, resize, windows, copy, history and the
  invariants, against model oracles. CI replays its corpus nightly.
- **Other terminals**: [`fux-vt/compare`](https://github.com/gold-silver-copper/fux/blob/main/fux-vt/compare/README.md)
  runs fux-vt beside nine other engines (Ghostty, alacritty, libvterm, avt,
  wezterm, vt100, xterm.js, tmux and xterm itself) and fails where they
  outvote it, field by field; its families record what still differs and
  why. Where the engines split and fux-vt follows a recorded choice (the
  departures below among them), `run.sh verdicts` checks that family
  against xterm alone.
- **The specifications** each behaviour is checked against are listed in
  [`references/README.md`](https://github.com/gold-silver-copper/fux/blob/main/references/README.md): ECMA-48, DEC
  STD 070 and the VT520 manual, xterm's ctlseqs, Unicode 17.0. A test
  cites the section that sets its expected value.

The differential tests against the vt100 crate that fux-vt replaced live in
history at `b8fa0d8`.

## Departures from the references

The references (ECMA-48, DEC STD 070, the VT520 manual, ITU-T T.416, listed
in [`references/README.md`](https://github.com/gold-silver-copper/fux/blob/main/references/README.md))
decide what fux-vt does wherever they speak, except here, where xterm
departs from them and fux-vt follows xterm: fux sets `TERM=xterm-256color`,
and programs are written and tested against xterm. The engine verdicts are
from `compare/run.sh replay --engines all` with the case given; `run.sh
verdicts` checks each against xterm.

- **HT keeps a pending wrap.** DEC STD 070, Appendix D.6.1, lists HT among
  the functions that clear the Last Column Flag. xterm, Ghostty, alacritty,
  libvterm, wezterm, xterm.js and tmux keep it; avt clears it
  (`--size 2x5 'abcde\tX'`).
- **Resetting DECAWM keeps a pending wrap.** Appendix D.6.1 lists
  RESET_MODE(AUTO_WRAP) among them. Every engine in `compare/` keeps it
  (`--size 2x5 'abcde\e[?7l\e[?7hX'`).
- **With DECAWM off, a glyph in the last column still leaves a wrap
  pending,** which a glyph printed after DECAWM is set again carries out.
  Appendix D.6.1 sets the flag only while Auto Wrap is on. xterm, Ghostty,
  alacritty and xterm.js set it; libvterm, avt, wezterm and tmux do not
  (`--size 2x2 '\e[?7lca\e[?7hX'`).
- **Bold (SGR 1) and faint (SGR 2) can both be on.** ECMA-48 (8.3.117)
  makes them one attribute, intensity, which 22 sets back to "neither bold
  nor faint", so each would replace the other. xterm, Ghostty, alacritty,
  libvterm, xterm.js and tmux keep both; avt and wezterm let faint replace
  bold (`--size 1x3 '\e[1m\e[2mX'`).
- **DECSTR turns autowrap on.** The VT520 manual (DECSTR, Table 5–6) says
  "No autowrap", and DEC STD 070 (Soft Terminal Reset, p. 4-37) "Auto Wrap
  Off (NVM if present)". xterm, libvterm, wezterm and xterm.js turn it on;
  Ghostty, alacritty, avt and tmux leave it as it was
  (`--size 2x3 '\e[?7l\e[!pabcd'`). `xterm-256color`'s `is2` and `rs2`,
  which `tput init` and `tput reset` send, begin with DECSTR and never set
  DECAWM again, while the entry advertises automatic margins (`am`).
- **REP after a grapheme cluster** (ECMA-48 8.3.103). The standard repeats
  the preceding graphic character, which after `e` and U+0301 is the
  combining mark; xterm repeats `e`, the character that took the cell, and
  so does fux-vt (`fux-vt-compare replay --engines all --size 1x8
  'e\u{301}\e[2b'`: Ghostty agrees, the other engines split three ways).
- **REP after a control function** (ECMA-48 8.3.103 leaves it undefined, so
  this is no departure but a choice where the standard is silent). xterm
  repeats nothing until a character is printed again, and so does fux-vt,
  REP after REP included; xterm.js and tmux agree, and Ghostty, alacritty,
  libvterm, avt and wezterm repeat the last character again
  (`fux-vt-compare replay --engines all --size 1x8 '-\e[2b\e[2b'`).
