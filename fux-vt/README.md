# fux-vt

A bounded, non-reflowing terminal emulator for fux. The fixed-size cell
representation and inherited sequence semantics were informed by Jesse
Luehrs's MIT-licensed implementation; its license is retained in `LICENSE`.
The grid and parser are owned implementations, not wrappers. This document is the
implementation contract; the verification report distinguishes implemented
coverage from the remaining end-to-end completion gates.

## Sequence matrix

CSI means ESC `[`. Missing/zero counts default to one unless noted. Coordinates
in sequences are one-based; the API uses zero-based rows/columns. The baseline
is vt100 0.16.2 plus fux's existing reply callback, not every xterm feature.

| Family | Contract/default/reset | Permanent test family |
| --- | --- | --- |
| Printable ASCII / UTF-8 | Printable runs bypass state dispatch in ground state; unicode-width 0.2 supplies widths; incomplete UTF-8 survives calls; invalid input and U+FFFD are discarded like the baseline | `text`, `chunking` |
| C0 | BS subtracts a column; HT advances to next fixed eight-column stop, clamped; LF/VT/FF advance/scroll without CR; CR goes to column zero; BEL, SI/SO and other unhandled C0 have no visible effect | `controls` |
| ESC 7 / 8 | Save/restore position, origin and drawing attributes; saved cursor is clamped after resize | `saved_cursor` |
| ESC = / > | DECKPAM / DECKPNM set and clear `application_keypad()` (default off; reset clears it). State only: fux-vt encodes no keypad input | `opt_in::keypad_mode_is_tracked_and_reset` |
| ESC M / c | Reverse index / full reset; reset clears both buffers and primary history, modes and attributes, but never restarts row identity allocation | `reset`, `scrolling` |
| CSI A B C D E F G H d | Relative up/down/right/left, next/previous line, horizontal absolute, cursor position, vertical absolute; inherited margin clamping and origin semantics | `cursor` |
| CSI @ P X | Insert/delete/erase characters, bounded to the row; no orphan wide halves; insertion/deletion blanks have default attributes; erase uses current attributes | `editing`, `wide_edits` |
| CSI L M S T | Insert/delete lines; scroll up/down within margins; counts bounded to affected region; partial-region operations do not add history | `scrolling` |
| CSI J / K, CSI ? J / K | Erase display/line: absent/0 forward, 1 backward, 2 all; current erase attributes; no protected cells | `erase` |
| CSI r | Top/bottom defaults 1/height; invalid range resets to full screen; homes to top margin, matching baseline | `margins` |
| CSI ? 6 h/l | Origin mode per buffer; homes on change; defaults off | `margins` |
| CSI ? 7 h/l | Autowrap, defaults on; right-margin cursor is parked until next printable glyph; disabled wraps overwrite the last available cell instead | `autowrap` |
| CSI ? 1 / 25 / 2004 h/l | Application cursor (off), cursor visibility (on), bracketed paste (off) | `modes` |
| CSI ? 47 h/l | Switch to/from separate alternate buffer without clearing it; primary history retained; alternate has no history | `alternate` |
| CSI ? 1049 h/l | Save cursor/attributes, clear and enter alternate; leave and restore primary cursor/attributes | `alternate` |
| CSI ? 9 / 1000 / 1002 / 1003 h/l | X10 press / press-release / button-motion / any-motion; latest set wins; reset only clears matching active mode | `mouse` |
| CSI ? 1005 / 1006 h/l | UTF-8 / SGR encoding state, latest set wins and matching reset restores legacy; fux still emits legacy bytes for non-SGR, not a new UTF-8 encoder | `mouse` |
| CSI m | 0/reset; 1/bold, 2/dim (mutually exclusive inherited intensity); 3/italic, 4/underline, 7/inverse; resets 22/23/24/27; 30–37/40–47, 90–97/100–107; 39/49 defaults; 38/48 indexed and RGB via semicolon or colon forms supported by baseline | `sgr` |
| CSI 5n / 6n / 0c | Replies `ESC[0n`, absolute one-based cursor report, `ESC[?1;2c`; missing DA parameter is zero; preserve baseline reply coordinates, including parked cursor; no replies for intermediates/private variants | `replies` |
| OSC / DCS / APC / PM / SOS | Consume without storing payload or drawing it; OSC accepts BEL or ST, others ST; cancellation/recovery follows parser state rules. With `Options::events` only, OSC payloads are buffered (see "Opt-in outputs") | `ignored_strings` |
| Other sequences | Safely parse and ignore; recognized C0 inside CSI still executes; no leakage of ignored string payloads | `ignored_sequences`, `parser_bounds` |

## Opt-in outputs

`Parser::new` uses `Options::default()`: everything below is off, and the
behaviour is exactly the table above. `Parser::with_options` enables either
part independently; `Parser::process_with` delivers to a `Sink` whose
`reply`/`event` methods default to discarding.

| Option | Contract | Tests |
| --- | --- | --- |
| `events` | OSC 0 → `IconName` then `Title`; OSC 1 → `IconName`; OSC 2 → `Title`; OSC 52 `Pc;Pd` → `Clipboard { selection, data }` unless `Pd` is `?` (a query); other OSC numbers produce nothing. BEL executed in ground, escape or CSI state → `Bell` (BEL terminating an OSC is not a bell). Payloads are raw bytes. An OSC string is terminated by BEL or by ESC (the start of ST); CAN/SUB cancel it without an event. At most `OSC_PAYLOAD_LIMIT` (64 KiB) payload bytes are buffered per string; a longer string is consumed with no event and its buffer is released immediately. Events are identical under any chunking | `opt_in::events_*`, `opt_in::osc_payloads_are_bounded_and_cancellable`, fuzz header bit `0x10` |
| `extended_replies` | DECXCPR `CSI ? 6 n` → `CSI ? row ; col R` (same coordinates as DSR 6n, including a parked cursor); secondary DA `CSI > c` / `CSI > 0 c` → `CSI > 1 ; 10 ; 0 c`; DECRQM `CSI ? Ps $ p` → `CSI ? Ps ; Pm $ y` with `Pm` 1 set / 2 reset for modes 1, 6, 7, 25, 47, 1049, 9, 1000, 1002, 1003, 1005, 1006, 2004 and 0 (not recognized) otherwise; ANSI DECRQM `CSI Ps $ p` → `CSI Ps ; 0 $ y`. Primary DA and DSR 5n/6n are unchanged | `opt_in::extended_replies_*`, fuzz header bit `0x20` |

`Cell::new`, `Cell::wide_continuation` and `Attributes::new`/`with_*` let a
consumer that stores or transports screen contents rebuild cells exactly;
`Cell::new` refuses contents over `Cell::CONTENTS_CAPACITY` (22) bytes. Parser
output never goes through them.

### Deliberate boundary

Upstream does **not** dispatch DECAWM 7; implementing it is a required,
standards-backed correction, tested independently as well as tracked in the
differential inventory. IL/DL outside scrolling margins are ignored, unlike
upstream's accidental row edits there. Both corrections have executed
XTerm(411) evidence from `../verification/fux-vt-xterm.py`. Upstream dispatches 47/1049 but **not** 1047/1048;
the latter remain ignored rather than pretending all alternate-screen aliases
are equivalent. ESC D/E/H, CSI f/s/u/g, CSI 3J, character-set designation and
programmable tab stops are not implemented by the baseline and remain ignored.
ESC =/> set the `application_keypad()` getter, as upstream's did, but fux
never acts on it: there is no keypad encoding. By default there are no
window-title, bell, clipboard, or window-resize side effects from child output,
and no OSC payload is retained. In particular OSC 52 from a child cannot bypass
fux's clipboard policy: fux never enables `Options::events`. Window resize from
child output remains unsupported in every mode.

Excluded: paragraph reflow; graphics protocols; kitty keyboard; grapheme
segmentation beyond a base glyph plus combining marks; history beyond its
configured ring; alternate-screen history. New compatibility decisions must
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

Cells retain at most 22 UTF-8 bytes, with the baseline's conservative rule:
append a combining scalar only when current byte length is below 18. A
combining scalar after an empty preceding cell attaches to a space; at column
zero it attaches to the previous row only when that row is soft-wrapped.
Otherwise it is dropped. An over-capacity mark is dropped, not allocated.
A one-column grid drops wide glyphs without moving or wrapping the cursor.

## Storage, identity and windows

The implementation owns row-major arenas and bounded row-slot metadata for
the primary and alternate grids. Logical order is independent of physical
slot. Primary history is a ring of at most `history_lines` rows; zero disables
it. Full-screen upward scroll retains departing rows; partial scroll,
reverse scroll and insert/delete lines discard displaced rows. Surviving
rows retain their IDs. Recycled slots receive new IDs. Cell mutations change
row versions rather than row identities. Reset and history clearing invalidate
removed IDs. Identity exhaustion must be an explicit error, never wrap/reuse.

Resize is not paragraph reflow: rows keep their upper-left cells and columns
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
at most 64 Mi retained cells (32 bytes each) and 1,048,576 retained rows.
Storage grows geometrically only to the configured cap as history fills;
empty history is not eagerly allocated. At capacity, scrolling reuses slots
without allocating. Resize builds replacement storage before swapping it in,
so peak storage can include old and new buffers. The final verification
report must state measured footprints, including metadata and peak resize.

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
an update. Row caching in fux uses identities/versions, not whole-screen
revision/width snapshots, and full frames still contain unchanged rows.

## Verification lifecycle

Before migration, temporary differential tests compared cells, attributes,
wide flags, wrap flags, cursor, modes, history and replies at operation
boundaries, whole and under split inputs. Passing checkpoint `b8fa0d8` retains
the recorder, adapters and diagnostic switch in history. The test, feature
and dependency were removed only after the permanent mapping in
[`tests/golden/README.md`](tests/golden/README.md) was committed. Unobservable internal upstream
state is checked by behavioural probes. The corpus adapts the deterministic
adversarial generator and terminal-edge streams from fux-fuzz at main commit
9140af1. Seeds and operation sequences remain permanently after the temporary
oracle dependency is removed; expected values are independently specified,
not captured from the new implementation.

The differential inventory records exact reproductions and permanent test
mappings. The two known tiny-grid crashes never execute in the oracle; they
have explicit independent expectations. Any additional mismatch must be
fixed or narrowly justified with evidence before migration completes.

The independent [`fuzz/` package](fuzz/README.md) documents its exact nightly
toolchain, seed generation and bounded run command. It exercises parsing, chunking, resize, windows,
copy, history and invariants. Final completion requires at least 600 seconds
clean on macOS plus all root/harness gates and trace replay described in
`../docs/prompt-fux-vt.md`. Passing coverage is not evidence that every input
is correct. See `../verification/fux-vt.md` for progress and final evidence.
