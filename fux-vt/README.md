# fux-vt

A bounded, non-reflowing terminal emulator for fux. The fixed-size cell
representation and inherited sequence semantics were informed by Jesse
Luehrs's MIT-licensed implementation; its license is retained in `LICENSE`.
The grid and parser are owned implementations, not wrappers. This document is the
implementation contract; the verification report will distinguish implemented
and verified coverage from planned tests while the branch is in progress.

## Sequence matrix

CSI means ESC `[`. Missing/zero counts default to one unless noted. Coordinates
in sequences are one-based; the API uses zero-based rows/columns. The baseline
is vt100 0.16.2 plus fux's existing reply callback, not every xterm feature.

| Family | Contract/default/reset | Permanent test family |
| --- | --- | --- |
| Printable ASCII / UTF-8 | Printable runs bypass state dispatch in ground state; unicode-width 0.2 supplies widths; incomplete UTF-8 survives calls; invalid input and U+FFFD are discarded like the baseline | `text`, `chunking` |
| C0 | BS subtracts a column; HT advances to next fixed eight-column stop, clamped; LF/VT/FF advance/scroll without CR; CR goes to column zero; BEL, SI/SO and other unhandled C0 have no visible effect | `controls` |
| ESC 7 / 8 | Save/restore position, origin and drawing attributes; saved cursor is clamped after resize | `saved_cursor` |
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
| OSC / DCS / APC / PM / SOS | Consume without storing payload or drawing it; OSC accepts BEL or ST, others ST; cancellation/recovery follows parser state rules | `ignored_strings` |
| Other sequences | Safely parse and ignore; recognized C0 inside CSI still executes; no leakage of ignored string payloads | `ignored_sequences`, `parser_bounds` |

### Deliberate boundary

Upstream does **not** dispatch DECAWM 7; implementing it is a required,
standards-backed correction, tested independently as well as tracked in the
differential inventory. Upstream dispatches 47/1049 but **not** 1047/1048;
the latter remain ignored rather than pretending all alternate-screen aliases
are equivalent. ESC D/E/H, CSI f/s/u/g, CSI 3J, character-set designation and
programmable tab stops are not implemented by the baseline and remain ignored.
ESC =/> affects upstream's application-keypad getter, but fux never acts on
that getter: there is no newly added keypad encoding. No window-title, bell,
clipboard, or window-resize side effects from child output. In particular OSC
52 from a child cannot bypass fux's clipboard policy.

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

Resize is not reflow: retained live rows keep their upper-left cells, extra
bottom rows/columns are discarded, growth is blank, and saved cursors clamp.
History rows retain their original column extent; window reads pad/clip them
without modifying history. Width changes clear live soft-wrap metadata.
Wide halves cut by an edit or resize are repaired before exposing the grid.
Zero dimensions are rejected. Allocation uses checked arithmetic and explicit
cell/row caps; errors leave the existing terminal usable. Each buffer permits
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

Change marks are non-destructive: row versions plus a structural generation
support independent readers. Structural changes (scroll, resize, reset,
eviction, buffer switch) force safe window refresh. Cursor/mode changes are
observable even without cell changes. Stale/exhausted marks must never hide
an update. Row caching in fux uses identities/versions, not whole-screen
revision/width snapshots, and full frames still contain unchanged rows.

## Verification lifecycle

Before migration, temporary differential tests compare cells, attributes,
wide flags, wrap flags, cursor, modes, history and replies at operation
boundaries, whole and under split inputs. Unobservable internal upstream
state is checked by behavioural probes. The corpus adapts the deterministic
adversarial generator and terminal-edge streams from fux-fuzz at main commit
9140af1. Seeds and operation sequences remain permanently after the temporary
oracle dependency is removed; expected values are independently specified,
not captured from the new implementation.

The differential inventory records exact reproductions and permanent test
mappings. The two known tiny-grid crashes never execute in the oracle; they
have explicit independent expectations. Any additional mismatch must be
fixed or narrowly justified with evidence before migration completes.

The independent `fuzz/` package exercises parsing, chunking, resize, windows,
copy, history and invariants. Final completion requires at least 600 seconds
clean on macOS plus all root/harness gates and trace replay described in
`../docs/prompt-fux-vt.md`. Passing coverage is not evidence that every input
is correct. See `../verification/fux-vt.md` for progress and final evidence.
