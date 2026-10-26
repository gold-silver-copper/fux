# fux-vt: threats and bounds

What a program in a pane can make fux-vt do, and the numbers that hold
it back: the threat model (1), every bound with its test (2), memory
(3), work and the per-call ceiling (4), guard mutants (5) and the
checker's invariants (6). The code and tests cite it by row (B01 to
B46 but B10 and B29, which are gone, and B34a) and by section number.

"The defaults" are fux's: a 50×200 screen (N = 10,000 cells, R = 50
rows, C = 200 columns) and H = 8 MiB of history.

## 1 The threat model

### 1.1 What a program in a pane controls

- Every byte it writes, in any order, amount and chunking. fux feeds
  what it read whole (16 KiB a read; a held frame, or a program's last
  output as it exits or is respawned over, more); a library host may
  pass a whole file at once.
- Through those bytes, all the state they reach: modes, margins, the
  pending wrap, wide glyphs, protection, both screens, styles,
  hyperlinks, prompt marks, OSC payloads, DECRQSS requests, sixel data,
  REP counts up to 65,535, and how long a sequence stays unfinished.
- Not the timing: `process` has no clock, and the key decoder's
  deadlines come from the host's calls.
- Not host calls, hash keys or addresses. Replies never carry memory.

The key decoder reads the client's own terminal, not a pane, but a
paste's content is whoever filled the clipboard's, so it is hostile too.

### 1.2 What the host controls and promises

fux-vt's own limits: rows and columns are `u16`, and rows × columns ≤
`MAX_CELLS` = 67,108,864, else `Err(Capacity)` and nothing changes
(B38). History holds at most `MAX_ROWS` = 1,048,576 rows and `MAX_CELLS`
cells, whatever its byte budget.

fux's promise, within which every bound and the per-call ceiling hold:
a pane is at most 4,096 rows × 4,096 columns (`view::MOST`: a client's
screen less its bar, or a tab given its size by `size` or `resize`; the
probes run at 4,095 × 4,096);
history is at most 1 GiB (8 MiB by default); a piece is what fux read
(`Pane::output`), or what a budget stop left of one (`Pane::unread`): one
read of at most 16 KiB, a program's last 1 MiB as it exits or is
respawned over, or a held frame, shown once past 2 MiB (`FRAME_LIMIT`),
so at most 2 MiB and what was appended last: one read, or that last
1 MiB; a call of n bytes has 4.4's ceiling (`Work::ceiling`)
max(1, ⌈n ÷ 4096⌉) times; the `Feature`s are `fux::pane::OPTIONS`',
every one but `ExtendedReplies` and `RectangleChecksums`.

The host also sets the picture budget (`Parser::set_picture_bytes`,
96 MiB by default, B01; set lower, large pictures clip), its cell size
(`set_host_graphics`; `CellSize` refuses one under 2×6 pixels, so that
a picture's placement stays within the ceiling, B05), the paste limit
(`with_paste_limit`; `usize::MAX` removes it) and the `Feature`s, which
set the OSC limit (B20).

Each host call has its own bound per call: `resize` (B37), `row_by_id`
and `index_of` (B40), `pictures()` (B09), `sixel::encode` (B08),
`Picture::new` (B07), `Cells::new`, `Cells::resize` and
`Cells::try_from_iter` (B39), `FrameScan::end` (B19),
`ByteQueue::push_with` (B41). The host promises its own call rate, and
that `push_with`'s closure only adds (B41 makes a broken promise
harmless).

Objects the host retains are its memory, not the parser's budget:
- an `Arc<Picture>` outlives its eviction (B06); fux keeps each client's
  last paint, at most one budget's worth more per pane per client;
- a cloned parser deep-copies grids, history, text, links and buffers,
  and shares pictures and the style table until either side adds a
  style; a clone taken mid-picture copies the picture being drawn
  (≤ 33.75 MiB);
- events and replies go to the `Sink` synchronously; fux-vt queues none;
- the key decoder's `out` grows at most one `Input` a byte (B26).

Shell marks (OSC 133). Every mark in the output counts, whichever
program sent it: any program can end the command running, with a status
of its choice, or start one. A mark costs a fixed amount of work and no
memory.

### 1.3 What happens when an allocation fails

The program-sized allocations are fallible: each degrades where it
happens, counts one in `Parser::refused()`, and the call reads every
byte it was given. It returns `Err(Error::Capacity)` only to say that a
refusal happened in it.

| Site | Degrade |
|---|---|
| A row scrolled into history | The row goes as with no history; the screen still scrolls |
| The alternate screen | The primary stays shown, and the mode reads reset |
| RIS or a clear, with no room for new grids | History goes, then the grids are blanked in place |
| The sixel decoder's growth | The picture keeps what it has, as at the budget (B03) |
| The ring or a map shrinking | It keeps its room until the next trim (B17) |
| A prompt mark's or a picture's line feed | The parser's state is set first, so it is never stuck in the string |

Everything else allocates infallibly, and a failure there aborts the
process (no unwinding). Those allocations are bounded by the screen's
size and the crate's constants, not by what a program sends (a row's
text, its link runs, the style table's screen share, DECCRA's copy, the
OSC buffer, tab stops, the key decoder's buffers): a program can fill
them, not grow them. The bounds keep both the abort and the OOM killer
away; infallible allocation is no safety proof.

### 1.4 What happens at a limit

| Kind | Meaning | Rows |
|---|---|---|
| Clip | What fits is kept | B03, B04, B20 |
| Evict | The oldest, or the hidden first, goes | B01, B02, B11, B15, B17, B27, B46 |
| Drop to its end | A string past its limit is read to its end and dropped, alike however split | B20's rest, B21, B23, B25 |
| Refuse | `Err` or `None`, and nothing changes | B07, B37, B38, B39 |
| Skip work that changes nothing | Copies that can't change the result are skipped | B33, B34, B34a |

## 2 The bounds

Conventions for every row:
- Chunkings: each probe feeds its input at 1 B, 16 B and 4 KiB a call,
  and whole (`WHOLE`: all of it, given again from each stop), as
  `tests/bounds/probe.rs` `CHUNKS` lists. A call of n bytes is held to
  the ceiling (`Work::ceiling`, 4.4) × max(1, ⌈n ÷ 4096⌉). Rows with
  megabytes of setup feed at no less than
  16 B or 4 KiB, as each probe says.
- Sizes: 1× is the smallest input that reaches the limit; 10× and 100×
  repeat it. `bNN_1x` and `bNN_10x` run in CI; `bNN_100x` is ignored and
  run at release under a 2 GiB memory cap.
- Each probe asserts, after every call: the heap's peak against the
  row's bound (from section 3), each counter it names against c × bytes
  (4.2), a lower bound on each of them (4.3), and the ceiling (4.4). At
  the end, the invariant checker is clean (section 6).
- Features: every `Feature` on, unless the row says otherwise. Screen: the defaults, unless the row says.
- Seeds: every row's reproducer is a frozen seed in `tests/seeds.rs`
  (written to `fuzz/seeds/`), replayed by
  `every_seed_reaches_its_limit` to check it reaches its limit.
- `tests/bounds/known-failing.txt` lists rows known to fail, with their
  error; a listed row that passes fails the suite. It is empty now.

In the test column, `bNN` is the probe of that name in
`tests/bounds/rows.rs`, run by
`bNN_1x`, `bNN_10x` and `bNN_100x` in `tests/bounds.rs`. B06, B08, B09,
B26 and B41 bound host-side objects or host calls.

| ID | What is bounded | The limit (counter) | At the limit | Input that reaches it | Test |
|---|---|---|---|---|---|
| B01 | Pictures kept, in bytes | Kept pictures' pixels on both screens, with what newer ones cover of them (B46), + the picture being drawn + its growth transient ≤ P = `PICTURE_BYTES` = 96 MiB a parser (holds one 4096×2160 picture with its growth) | Pictures no longer shown go first, then the oldest; a decode alone past P clips (B03) | `ESC P q "1;1;4096;2160 ESC \` × 64 | `pictures_hold_no_more_than_their_budget`, `pictures_fill_their_budget_exactly`, `pictures_at_the_budget_are_kept_and_a_byte_past_lets_one_go`, `pictures_no_longer_shown_go_first` (`screen/tests.rs`); `b01` holds the heap with extents declared and nothing drawn, so it does not reach the budget |
| B02 | Placements, by count | ≤ 64 on both screens together (`MAX_PICTURES`) | The 65th lets one go, as B01 picks | 65 × `ESC P q @ ESC \` | `b02`; `a_screen_keeps_64_pictures_and_the_65th_lets_the_oldest_go` |
| B03 | The sixel decoder's buffer and growth | Buffer ≤ `MAX_PIXELS` × 4 = 35,389,440 B; each side doubles, clamped to `MAX_SIDE` = 4096 and `MAX_PIXELS` = 4096 × 2160; growth copies or zeroes ≤ 2 × the final buffer (`pixels` ≤ 4,096 a byte + 2 × `MAX_PIXELS` a picture), doubling even beside pictures that fill P, which it lets go for the room, while P holds twice the picture (always at the default P; a host's lower P may make it grow just to what it needs). Raster attributes grow nothing | Sixels past the limits or P aren't painted; the picture keeps what fits | A picture growing a column at a time over 340 bands; width and raster ladders `"1;1;w;2160` | `b03`; `a_kept_picture_holds_just_its_pixels` |
| B04 | Sixel decode work | `pixels` ≤ 4,096 per input byte; pixels are kept at grid resolution, the aspect applied when shown; a repeat past 4,096 counts 4,096 | — | `ESC P q "360;1` + `!4096~$` × 600 | `b04` |
| B05 | A declared extent, and placement | An extent with nothing drawn allocates and paints nothing; covering the last cell row of the extent before, as each does, is B46's (≤ 9,492 `pixels` and 70,656 B a picture at a 10×20 cell). Placement fills rows, ≤ 2,291 `cells` a byte at a 10×20 cell and 7,636 at 2×6 (landscape); 4,349 and 14,487 (portrait); a host cell under 2×6 refused (`CellSize::new`) | — | 215 × `ESC P q "1;1;4096;2160 ESC \` in 4 KiB, P2 = 0, 1, 2; and portrait | `b05`; `a_declared_extent_keeps_no_pixels`, `a_picture_needs_a_host_cell_of_a_sixel_band_or_more` |
| B06 | Pictures the host retains (host side) | An `Arc<Picture>` outlives eviction, outside the budget; a clone mid-picture ≤ steady + 2 × 53,084,160 B | — | A clone mid-picture; a lowered budget while the host holds one | `b06` |
| B07 | `Picture::new` (host call) | `None` unless `rgba.len()` = w × h × 4 with no overflow and both sides ≤ 4096 | `None` | `u32::MAX` square with no pixels | `b07` |
| B08 | `sixel::encode` (host call) | Output ≤ 12 B a crop pixel + 6 B a colour a band + colour definitions; work ≤ 3 `pixels` a crop pixel. fux caches encodings per client, ≤ 16 MiB (`SIXEL_BYTES`) | — | A 2000×1000 256-colour checkerboard | `b08` |
| B09 | `Screen::pictures()` (host call) | `rows` ≤ 128 × (⌊log2(history rows + 1)⌋ + 1 + R) + 64 + the rows its placements cover, a call | — | 8 MiB of empty history, 64 pictures; 64 pictures 4,096 pixels tall | `b09` |
| B11 | The style table, in bytes | Its room past the screen's share (S(2N): 1,146,880 B a grid at 50×200) counts in history's bytes; a successor grid or a cloned parser's copy is charged at once | History's oldest rows go | 150,000 distinct direct backgrounds, 1 MiB of history | `b11`; `a_style_table_past_the_screens_share_takes_history_room` |
| B12 | Style lookups | Expected O(1) probes: std's keyed hash of both words; ≤ 64 styles share a place (`SPREAD`, checked) | — | A family of 8 Ki to 32 Ki styles that hashed alike unkeyed | `b12` |
| B13 | The style sweep | `styles` ≤ 7 per input byte, amortized: a sweep after keeping L styles comes after max(floor − L, L) new ones, floor = max(4,096, N ÷ 8) | — | 20,000 new backgrounds on one cell over 700 full rows | `b13`; `deccara_maps_each_of_three_styles` |
| B14 | Hyperlinks held | ≤ `LINK_BYTES` = 4 MiB a grid of real heap (strings, `Arc` headers, the id map's buckets, both tables while it grows); URI ≤ 2,083 B, id ≤ 250 B | A link that doesn't fit after a sweep prints unlinked; after a failed sweep the next 256 are refused untried (`STALL`) | 2,000 links of 2,083-byte URIs; 60,000 1-byte links with distinct ids | `b14` |
| B15 | Link runs | A screen row ≤ 256 runs × 32 B at 200 columns; a history row's runs shrink as it enters and count in history's bytes | History's oldest rows go | Two links alternating cell by cell, 1 MiB of history | `b15`; `a_row_with_one_link_holds_no_memory_for_it` |
| B16 | The link sweep | `links` ≤ 256 per input byte, amortized; its transient is the links kept, 16 B a run, nothing per history row | — | 4 MiB held, then refused opens; the same over 400,000 empty rows | `b16` |
| B17 | History, in bytes | `History::bytes()` counts capacities (ring, blocks, text, maps, side stores, link runs, styles), trimmed to ≤ the budget; a map gives back room past 4 × its length + 64; heap ≤ 1.1 × H + 64 KiB | The oldest rows go | Rows of 2,049 cells; 349,525 empty rows, then full ones | `b17`; `guards.rs` `history_*` |
| B18 | Line feeds into history | `cells` ≤ C + 3 and `rows` ≤ 2 per line feed, amortized | — | 100,000 `\n` after full rows | `b18` |
| B19 | The frame scan | `scanned` ≤ 2 × the bytes held + 16 a call | — (fux cuts a frame at 2 MiB or 1 s) | BSU, then `ESC [` + 64 Ki digits in pieces; a long DECRST | `b19`; the fuzz targets' frame-end agreement (`common.rs`) |
| B20 | The OSC buffer | Room ≤ the OSC limit + 1: 65,537 B (`OSC_PAYLOAD_LIMIT` + 1), 65 B with only `PromptMarks`, else 1 | A longer string is read to its end, its event `cut` (`Kept`), nothing past the room kept | OSC 52 of 64 KiB + 1 | `b20`; `the_osc_buffer_takes_the_room_a_string_needs_up_to_16_kib`, `the_osc_buffer_has_room_for_its_limit_alone` |
| B21 | DECRQSS, APC, PM, SOS, ignored DCS | DECRQSS keeps ≤ 4 B (`REQUEST_LIMIT`); the rest keep 0 B; `bytes` ≤ 1 a byte | A longer request answers "not known" | 64 KiB of APC; sixel with `Pictures` off | `b21` |
| B22 | Parameters | 32 `u16` per sequence, no heap | A 33rd isn't kept | `CSI` + `1;` × 1,000 + `m` | `b22` |
| B23 | The paste decoder | A paste's room ≤ limit + 5 = 65,541 B; the delivered `String` ≤ 3 × limit; `keys` ≤ 2 a byte + 258 a read | `PasteTooLong`; the rest dropped to the end marker | `ESC[200~ESC[201~` × 5,461 in one 64 KiB read | `b23`; `a_paste_of_64_kib_is_kept_and_one_byte_more_is_not` |
| B24 | Bytes the key decoder holds | ≤ `HELD` = 258 B (+ ≤ 2 × 258 B while a read is cut); `keys` ≤ 2 a byte + 516 a read while an answer is held | — | `ESC`, then 1 MiB of `a`; a 258-byte DCS answer 1 byte at a time | `b24`; `key_work.rs` |
| B25 | Answer windows | OSC ≤ 128 B (`OSC_LIMIT`), DCS ≤ 256 (`DCS_LIMIT`), key CSI ≤ 32, `CSI ?` ≤ 64; past a window held to 256 B, then dropped | Dropped to its end, nothing typed, however split | `ESC [ ?` + 300 × `1` + `c`, whole and in pieces | `b25` |
| B26 | The key decoder's output (host side) | ≤ one `Input` per input byte | — | 64 KiB of single keys | `b26` |
| B27 | The kitty keyboard stack | ≤ 32 entries a screen | The oldest goes | 33 × `CSI > 1 u` | `b27` |
| B28 | Replies | ≤ 16 reply bytes per input byte; each `Reply` ≤ 64 B | — | 585 × DECRQSS `m` with every rendition; each request's longest answer | `b28` |
| B30 | Cluster text | A cluster ≤ 128 B (`CLUSTER_CAPACITY`); a row's long text ≤ 32C + 128, short ≤ 17C; `text` ≤ 256 a byte | Cut at 128 B; a row with no room drops what would join | `e` + 64 × U+0301 | `b30`; `a_long_cluster_that_ends_at_the_limit_does_not_compact` |
| B31 | A zero-width character after a full row | `text` ≤ 257 a byte | Dropped | 1,000 × U+200B after a full 1×200 row | `b31` |
| B32 | Glyph width | Every glyph ≤ 2 columns; U+17D8 is 1 | — | `XYZW\r` U+17D8 on 1×10 | `b32`; `every_character_is_as_wide_as_the_width_rule_says` |
| B33 | ASCII REP | `cells` ≤ 14,564 a byte; copies past what the screen and history can show are skipped (≤ 720 a REP on 2×10 with 1 KiB of history) | Skipped, same result | 455 × `X ESC[65535b` in 4 KiB | `b33`; `repeat_makes_its_ascii_copies_and_returns` |
| B34 | Non-ASCII REP | `cells` ≤ 23,832 a byte | As B33 | 372 × `一 ESC[65535b` | `b34`; `rep_spends_a_copy_by_its_width` |
| B34a | REP of a glyph that joins itself | `text` ≤ 12 a byte + 2,752 to fill the cluster once; `cells` ≤ 12 a byte | Copies joining a full cluster are skipped | 372 × U+1100 `ESC[65535b` | `b34a` |
| B35 | Screen-wide functions | `cells` ≤ N ÷ 2 a byte; `text` ≤ R × (49C + 128) ÷ 4 a byte; a scroll inside left and right margins ≤ 3N `cells` and R × (49 × span + 128) `text` a byte | — | 4 KiB each of DECRQCRA, DECCRA, DECFRA, DECERA, DECSERA, DECCARA, DECRARA, DECALN, ED 2, 1049, DECIC, DECDC, SL, SR; margin LF, SU and wrapping text | `b35` at 50×200, 500×500, 4095×4096; `bounds.rs` `budget` tests |
| B36 | Scrolls inside a region | `slots` ≤ 2 × region rows and `cells` ≤ region cells a sequence, the count clamped to the region; ≤ N ÷ 4 cells and 2R ÷ 3 slots a byte | — | `ESC[4095L` at 4095×1; IL across the ring's end | `b36` |
| B37 | `resize` and reflow (host call) | Heap ≤ steady + another history term; `rows`, `cells` ≤ 3 × (history cells + screen cells + history rows) a call | A rejected resize changes nothing, synchronized output and the cluster being printed included | Full history, 200 → 199 → 200 columns; 65535×65535 | `b37`; `a_rejected_resize_leaves_synchronized_output_on`, `a_rejected_resize_leaves_a_grapheme_open` |
| B38 | The screen's size | Rows × columns ≤ `MAX_CELLS` = 67,108,864; history ≤ `MAX_ROWS` rows | `Err(Capacity)`, nothing changed | 65535×65535 | `b38` |
| B39 | `Cells::new`, `Cells::resize` and `Cells::try_from_iter` | Length ≤ `MAX_CELLS` | `Err(Capacity)`, the cells unchanged | `Cells::new(usize::MAX)` | `b39`; `cells_at_the_most_are_made` |
| B40 | Row lookups by id (host call) | `rows` ≤ R + ⌊log2(history rows + 1)⌋ + 1 a call; the index's first 256 spans are free (20 KiB a grid), each past costs 80 B of history's budget | `None` for a row that is gone | 8 MiB of empty history; `x ESC[L CR LF` repeated | `b40`; `row_lookup.rs` |
| B41 | `ByteQueue::push_with` (host call) | After the closure, what was taken ≤ the buffer's length | — | A closure that clears the vector, then a push | `b41` |
| B42 | Small fixed state | Tab stops a byte a column; the palette about 1.3 KB; kitty stacks 32 entries | — | HTS at every column | `b42` |
| B43 | Events | Delivered synchronously; fux-vt keeps none | — | 400 × OSC 9 | `b43` |
| B44 | Refused allocations | Each refusal counts in `Parser::refused()` and degrades as 1.3 says; every later byte is read | 1.3's table | `ESC]133;A BEL hello` with a refusal forced (`refuse_next`) | `b44` |
| B45 | Key and mouse encoders (host calls) | ≤ 64 B a call | A mouse position past its encoding is clamped | Every key, modifier and mode; mouse at each encoding's edges | `b45` |
| B46 | Covering | A placement covers ≤ `COVER_WORDS` = 4,000,000 words of 64 pixels of the pictures under it, the room to note them included (16 B a row shown of each picture covered, and the covered rows' words) and a copy of what was noted of one the host holds; the room, the words promised included, is inside P (B01). It reads the newer picture's drawn grid positions on the rows of those under it, once a grid row: ≤ the positions it drew (B04) | One under it past the words, or past P, goes whole, the newest covered first | 64 pictures, each a column right of the last, opaque (P2 0) and drawn (P2 1) | `b46`; `a_picture_covers_only_the_pixels_it_lies_on` |

## 3 Memory

### 3.1 The steady state at the defaults

One parser, every term a capacity:

| Term | Formula | Bytes |
|---|---|---|
| Screen cells, both grids | 2 × N × 8 | 160,000 |
| Row records, both grids | 2 × R × 104 | 10,400 |
| Row text, both grids | 2 × R × (2 × 6,528 + 2 × 3,400 + 128) | 1,998,400 |
| Screen rows' link runs | 2 × R × 256 × 32 | 819,200 |
| Links held, both grids | 2 × `LINK_BYTES` (B14) | 8,388,608 |
| Style tables, the screens' share | 2 × S(2N) (B11) | 2,293,760 |
| History, with its text, links, maps and styles | 1.1 × H + 64 KiB (B17) | 9,293,004 |
| History's free row-id spans, both grids | 2 × 256 × 80 (B40) | 40,960 |
| The parser's buffers | OSC 65,537 + the decoder + DECRQSS | ≈ 67,000 |
| Pictures, the decoder and its growth inside | P (B01) | 100,663,296 |
| Small fixed state | tab stops, palette, kitty stacks | ≈ 6,000 |

- Steady: 123,740,628 B = 118.01 MiB (22.01 MiB without pictures).
- While processing, with the largest transient (3.2): 128,354,362 B =
  122.41 MiB.
- With a resize: 136,431,232 B = 130.11 MiB, the peak engine heap per
  parser at fux's defaults.

### 3.2 Transient peaks

No two happen at once. While processing: the style sweep or a ring or
map shrinking, ≤ 0.5 × 1.1 × H (4.40 MiB); the link sweep ≤ 3.44 MiB;
a map's growth ≤ 2.64 MiB; DECCRA's copy ≈ 1.02 MiB; sixel growth is
inside P. A resize or RIS (host calls) builds a new history and screens
beside the old: + 12.10 MiB.

### 3.3 Outside the parser

- fux, per pane and client: ≤ P of evicted pictures in its last paint.
- fux, per pane: its frame buffer (≤ 2 MiB + 16 KiB, or 2 MiB + 1 MiB
  as its program's last output is read, at its exit or a respawn over
  it), and output read but not parsed after a budget stop
  (`Pane::unread`: a held frame, one 16 KiB read and a program's last
  1 MiB as it exits or is respawned over, twice while joined).
- fux, per client: encoded sixel data, ≤ `SIXEL_BYTES` = 16 MiB.
- A parser cloned mid-picture: + ≤ 33.75 MiB.
- The key decoder: 258 B held (+ 2 × 258 while a read is cut), a paste's
  65,541 B and its `String` (≤ 196,608 B), and `out` until drained.

### 3.4 How the bounds tests measure it

- `tests/bounds.rs` (`required-features = ["checks"]`) makes
  `fux-vt-peak`'s `ThreadPeak` its `#[global_allocator]`. It counts the
  live bytes and their peak for each thread apart, so the test harness's
  other threads never count, and a `realloc` counts the new block beside
  the old until the old is freed, so a growth's transient shows. It is a
  crate of its own because fux-vt forbids `unsafe`.
- One probe at a time: each holds a process-wide mutex
  (`one_at_a_time`), so the binary holds one probe's memory.
- A probe makes its parser, resets the peak (`Heap::begin`), feeds its
  input at its chunking, and after every call checks the peak above the
  base against the row's bound, built from 3.1 and 3.2 (`steady()` in
  `probe.rs` sums those terms, a little looser: 48 B a link run and 43 B
  a style entry).
- What it can't see: the allocator's own size classes and overhead (it
  counts requested sizes), and a block freed on another thread, which
  counts as freed there. fux-vt makes no threads.

## 4 Work

### 4.1 The counters

With the `checks` cargo feature (off by default; the bounds tests, the
fuzz targets and mutation runs turn it on, never a release), fux-vt
counts its work in a per-thread `Work`, read by `Work::now()` and
differenced with `Work::since`. The counters come from one table
(`work.rs`), each added to with saturating arithmetic. Without the feature nothing is counted.

| Counter | Counts |
|---|---|
| `bytes` | Input bytes the parser's loop and its bulk scans examine |
| `scanned` | Bytes `FrameScan::end` examines |
| `cells` | Grid cells written, cleared, moved or read, and REP copies tried or skipped |
| `slots` | Row slots moved |
| `rows` | History rows pushed, popped, read or searched; rows walked by resize, reflow, lookups and `pictures()` |
| `text` | Row text bytes copied, compacted, cloned or rescanned |
| `pixels` | Sixel pixels painted or filled, growth's copied or zeroed bytes ÷ 4, and the words covering reads, writes and makes room for |
| `styles` | Cells visited by style sweeps |
| `inserts` | New styles put in a table |
| `lookups` | Style-table finds by hash |
| `links` | Runs, rows, id entries and sort steps of the link sweep |
| `keys` | Bytes the key decoder scans or copies |
| `sums` | Cells DECRQCRA's checksum reads |
| `spans` | Rows a scroll inside left and right margins copies a span of |
| `edits` | Cells a rectangle edit rewrites one at a time (DECCRA, DECFRA, DECERA, DECSERA, DECCARA, DECRARA) |

### 4.2 The constants per input byte

Total work ≤ c × input bytes (+ a constant per call or per copy where
stated). Per call holds for each call; amortized over any run of calls.

| Operation | Counter | c per byte | Kind | Row |
|---|---|---|---|---|
| Line feed into history | `cells`, `rows` | C + 3, 2 | amortized | B18 |
| Sixel decode | `pixels` | 4,096 (+ 2 × `MAX_PIXELS` a picture for growth) | per call | B03, B04 |
| Sixel placement | `cells` | 2,291 / 7,636 landscape, 4,349 / 14,487 portrait | per call | B05 |
| Covering | `pixels` | `COVER_WORDS` a placement | per call | B46 |
| Style sweep | `styles` | 7 | amortized | B13 |
| Link sweep | `links` | 256 | amortized | B16 |
| The frame scan | `scanned` | 2, + 16 a call | per call | B19 |
| APC, PM, SOS, ignored DCS | `bytes` | 1 | per call | B21 |
| Paste and key decoding | `keys` | 2, + 258 a read (516 while an answer is held) | per call | B23, B24 |
| Replies | reply bytes | 16 | per call | B28 |
| Cluster join | `text` | 256 | per call | B30 |
| Zero-width after a full row | `text` | 257 | per call | B31 |
| ASCII REP | `cells` | 14,564 | per call | B33 |
| Non-ASCII REP | `cells` | 23,832 | per call | B34 |
| Self-joining REP | `text`, `cells` | 12 (+ 2,752 once), 12 | per call | B34a |
| Screen-wide functions | `cells`, `text` | N ÷ 2, R × (49C + 128) ÷ 4 | per call | B35 |
| Scrolls inside left and right margins | `cells`, `text` | 3N, R × (49 × span + 128) | per call | B35 |
| Region scrolls | `cells`, `slots` | N ÷ 4, 2R ÷ 3 | per call | B36 |
| Host calls: `resize`, lookups, `pictures()`, `encode` | `rows`, `cells`, `pixels` | per call, as B37, B40, B09, B08 say | per call | |

### 4.3 The lower bound each probe asserts

A bound on the sum alone can't tell a counter stuck at 0, so each probe
also asserts that each counter it names counted at least what it knows
was done (`Probe::at_least`): `bytes` ≥ the bytes parsed (every probe);
`cells` ≥ REP copies shown, a rectangle's area, a placement's cells, a
margin scroll's span; `slots` ≥ the rows a region scroll moved; `rows` ≥
rows pushed into history, ≥ 1 a lookup; `text` ≥ cluster
bytes joined; `pixels` ≥ the painted area; `styles` ≥ the cells a forced
sweep read; `inserts` and `lookups` ≥ the distinct styles fed; `links`
≥ the links at a sweep;
`scanned` ≥ the bytes held; `keys` ≥ the bytes fed to the decoder;
reply bytes ≥ what was asked.

### 4.4 The per-call ceiling and the work budget

Every call does at most U = 32,000,000 cells of work, W = Σ counter ×
weight, counted in cells: each kind's weight (`budget::weight`) is how
many cells one of its units costs, rounded up, so the budget stops no
later than it did when weights were nanoseconds:

| `bytes` | `scanned` | `cells` | `slots` | `rows` | `text` | `pixels` | `styles` | `inserts` |
|---|---|---|---|---|---|---|---|---|
| 1 | 1 | 1 | 3 | 4 | 1 | 1 | 1 | 20 |

| `lookups` | `links` | `keys` | `sums` | `spans` | `edits` |
|---|---|---|---|---|---|
| 10 | 4 | 1 | 9 | 81 | 8 |

The work budget. A few bytes can ask for work in proportion to the
screen or to history, and fux reads every pane on one thread, so that
work is spent from a per-thread budget as it is done (`budget::spend`).
`Parser::process_until_stop` looks at the budget after each sequence or
string, each line feed, and each wrap that scrolled inside margins; once
`LIMIT` = U ÷ 2 is spent since its last stop, it stops at that byte
boundary and returns `Stop::Budget`. Nothing is lost: the host gives it
the rest later. fux's pane loop stops reading that pane at a budget
stop and reads the rest on the pane's next turn, never in the same one,
so one turn does at most one budget per flooding pane. The other half
of U is room for the sequence that crossed `LIMIT` and for the work no
site spends (text and C0 controls, at most a row's or a few units a
byte, which the call's length bounds).

A screen-wide sequence that could take a call that has already spent
something past U is left for the next call, to run alone
(`Screen::holds_back`). Above about 550,000 cells one such sequence
alone can cost more than U, so a call's ceiling is
`Work::ceiling(size)`: U, or the most one screen-wide sequence costs at
that size (`budget::sequence_most`), whichever is larger.

What is checked where:
- In CI, the bounds suite (a `checks` build) asserts W ≤ the ceiling
  for every call of every probe (× max(1, ⌈n ÷ 4096⌉) for a call of n bytes),
  and `tests/bounds.rs` `budget` asserts, at 4095×4096, that each class
  of screen-wide function stops at the budget and ends with the screen
  one unbudgeted call makes. `guards.rs`
  `a_call_stops_at_half_the_ceiling_and_not_before` guards `LIMIT`.
- The worst measured time of one 4 KiB call is in `fux-vt/README.md`'s
  "The time of one call" bullet.

## 5 Guard mutants

A guard mutant is a mutant cargo-mutants makes in a bounded path (a
limit's comparison, a clamp, a limit constant, a counter's method) that
the bounds and safety tests must catch: if one survives, a limit can
move without a test noticing. The guard set is
`fux-vt/compare/mutants-guards.txt`, run by
`fux-vt/compare/mutants.sh guards` nightly in `fuzz.yml`.

### 5.1 Writing a limit so it has mutants

fux-vt forbids `clippy::arithmetic_side_effects`, so its arithmetic is
`saturating_*` and `checked_*`, which cargo-mutants doesn't mutate;
and cargo-mutants skips any function named `new`. So:

1. Each limit is a strict `<` or `>` in a named function that isn't
   `new`, returning `bool` (`fn over_cap(&self, ..) -> bool`). Then its
   off-by-one mutant (`>=`, `<=`) and its body mutants (`true`, `false`)
   exist; a limit written with `<=` or `>=` has only its inverse.
2. Each clamp that matters is an `if` with a strict operator, not a bare
   `min` or `max`.
3. Limit constants are expressions where natural (`1 << 12`,
   `4096 * 2160`), which gives them mutants; a bare literal has none, and
   its one-under, at and one-over test guards it.
4. Each probe asserts each counter's lower bound (4.3), so a count
   that stops being made is caught.
5. Runs are `mutants.sh`'s: with `--features checks`, and fux's tests
   beside fux-vt's, which kill some mutants fux-vt's tests can't reach.
6. Where the strict form moves a threshold by one, that is a change of
   the limit, stated in its row, not of behaviour.
7. Timeouts, unviable and unattempted mutants are not caught. A mutant
   no test can tell from the code is equivalent only with a written
   reason, in `fux-vt/compare/mutants-equivalent.txt`.

## 6 The invariants the checker asserts

`Parser::check` and `Screen::check` (under `checks`) check both screens,
and report which checks ran and each fault by its check's name. The fuzz
targets call them after every operation, the bounds probes at the end of
every row. "(Tests)" marks an invariant that compares two states, which
the tests check instead.

- **1** The OSC payload ≤ its limit (+ 1 inside an OSC); room ≤ 65,537 B.
- **2** 1 to 32 parameters.
- **3** In sixel exactly while a decoder exists, its picture in limits.
- **3a** An XTGETTCAP is answered only while its string is read.
- **4** A UTF-8 partial holds fewer bytes than its lead byte announces.
- **5** `FrameScan` resumes inside the bytes held, in a reachable state.
- **6** `refused()` never decreases. (Not checked yet.)
- **7** A grid's `order` is a permutation of its slots.
- **8** One record, text and link list per slot.
- **9** Every cell past a row's `used` is blank.
- **9a** Past a row's end, on a row not single width, every cell is empty.
- **10** Screen rows are `cols` wide; history rows fit their block.
- **11** Wide pairs are adjacent, on the screen and in history.
- **12** No glyph is wider than 2; a print advances by what it wrote.
  (Tests: B32.)
- **13** Spilled text lies inside its row's text; long text ≤ its limit,
  short ≤ its limit + 128, a cluster ≤ 128 B.
- **14** Every tabled style, on the screen and in history, is in its
  grid's table.
- **15** `places` matches `table` one to one; ≤ 64 styles share a place.
- **16** The table holds ≤ `MAX_STYLES`.
- **17** History's cell and text counts equal a recount.
- **18** Every block a history row names is held, and its cells fit it.
- **19** Text and link flags match the maps; the side store of rows not
  single width names rows kept, in order.
- **20** Row versions ≤ the grid's version; structural ≤ version.
- **20a** Row ids are unique across history and both screens, never
  reused, and `row_by_id` finds exactly the row with that id.
- **21** After a trim: bytes ≤ the budget, rows ≤ `MAX_ROWS`, cells ≤
  `MAX_CELLS`; the ring has room for ≤ two chunks more than it keeps,
  each map for ≤ 4 × its length + 64.
- **21a** History is charged the style table's room past the screen's
  share.
- **23** `held` covers the live links' cost, and `held` ≤ `LINK_BYTES`.
- **24** Each row's link runs lie inside its width, sorted and apart;
  every link with an id is in the id map; URIs ≤ 2,083 B, ids ≤ 250 B.
- **25** The cursor and saved cursors are on the grid, within their
  line's columns (half of them on a line not single width).
- **26** Margins lie inside the grid, each the whole of it or `first <
  last`; a row not single width has its flag set.
- **27** Pending wrap is set only by one of its setters. (Not checked:
  each setter's tests cover it.)
- **28** After any operation, every screen position whose row id, cells,
  wrap, links or prompt mark changed is reported by
  `dirty_rows_since` or `full_refresh_since`,
  and `changed_since` is true; `set_prompt` gives its row a new version,
  so prompt marks count. (Tests: the fuzz targets' `common.rs`.)
- **28a** `wrapped` on history's newest row continues into screen row 0,
  and walking `above()` from any row ends. (Tests.)
- **29** Whole and chunked feeding agree on modes, cursor shape,
  margins, palette, pictures and the inactive screen. (Tests:
  `tests/corpus/invariants.rs`.)
- **30** ≤ 64 placements on both screens together; ids increase.
- **31** Kept pictures + the decoder + its transient ≤ the budget.
- **32** A shown picture's cells hold `PLACEHOLDER`, inside its area, on
  its screen (but rows cropped off a top that keeps no history); its
  rows still exist.
- **35** The key decoder holds ≤ 258 B, a paste's room ≤ limit + 5;
  split and whole input decode alike (tests).
- **36** Kitty stacks ≤ 32 entries; tab stops kept for no more columns
  than a grid has had (≤ 65,536).
- **37** A rejected `resize` leaves every reader-visible field as it
  was, synchronized output included, and the cell printed last, which
  the next character can still join. (Tests: B37.)
