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
| C0 | BS subtracts a column; HT advances to next fixed eight-column stop, clamped; LF/VT/FF advance/scroll without CR; CR goes to column zero; BEL, SI/SO and other unhandled C0 have no visible effect. BS, LF, VT, FF and CR end a pending wrap; HT keeps it (see CSI ? 7) | `controls`, `conformance::a_pending_wrap_*` |
| ESC 7 / 8, CSI s / u | Save/restore position with its pending wrap (DEC STD 070, Appendix D.6.1), origin and drawing attributes; SCOSC/SCORC (`CSI s` / `CSI u`) share DECSC's slot; saved cursor is clamped after resize | `saved_cursor`, `extended::scosc_*`, `conformance::a_pending_wrap_is_saved_*` |
| ESC = / > | DECKPAM / DECKPNM set and clear `application_keypad()` (default off; reset clears it). State only: fux-vt encodes no keypad input | `opt_in::keypad_mode_is_tracked_and_reset` |
| ESC M / c | Reverse index / full reset; reset clears both buffers and primary history, modes and attributes, but never restarts row identity allocation | `reset`, `scrolling` |
| CSI A B C D E F G H d f | Relative up/down/right/left, next/previous line, horizontal absolute, cursor position, vertical absolute, HVP (as CUP); inherited margin clamping and origin semantics | `cursor`, `extended::hvp_*` |
| CSI @ P X | Insert/delete/erase characters, bounded to the row; no orphan wide halves; insertion/deletion blanks have default attributes; erase uses current attributes | `editing`, `wide_edits` |
| CSI L M S T | Insert/delete lines; scroll up/down within margins; counts bounded to affected region; partial-region operations do not add history | `scrolling` |
| CSI J / K, CSI ? J / K | Erase display/line: absent/0 forward, 1 backward, 2 all; current erase attributes; no protected cells | `erase` |
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
| CSI m | 0/reset; 1/bold, 2/dim (mutually exclusive inherited intensity); 3/italic, 4/underline, 5/slow and 6/rapid blink (mutually exclusive), 7/inverse, 8/hidden, 9/strikeout; resets 22/23/24/25/27/28/29; 30–37/40–47, 90–97/100–107; 39/49/59 defaults; 38/48/58 (foreground, background, underline colour): `5;n` and `2;r;g;b` read as xterm reads them (values the list ends before are 0; another kind takes only itself), and ITU-T T.416's (13.1.8) colon forms `5:n`, `2:r:g:b` and `2:space:r:g:b`, the colour space ignored, as in xterm; a colour out of range is skipped and the rest of the SGR applies. `4:0` ends underline and `4:1` to `4:5` (kitty's underline styles) set it: xterm ignores them, every other engine in `compare/` reads them, and fux-vt keeps no style | `sgr`, `extended::blink_*`, `conformance::sgr_*`, `conformance::an_invalid_sgr_*`, `conformance::underline_styles_*` |
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
are equivalent. ESC D/E/H, CSI g, CSI 3J, character-set designation and
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
  why.
- **The specifications** each behaviour is checked against are listed in
  [`references/README.md`](https://github.com/gold-silver-copper/fux/blob/main/references/README.md): ECMA-48, DEC
  STD 070 and the VT520 manual, xterm's ctlseqs, Unicode 17.0. A test
  cites the section that sets its expected value.

The differential tests against the vt100 crate that fux-vt replaced live in
history at `b8fa0d8`.
