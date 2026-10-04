# Golden screens

Each `.snap` file is the expected screen after every operation of one
fixture in `../corpus/fixtures.rs`. `../golden.rs` replays each fixture on
a 4×12 parser with 3 history rows, in pieces of 1, 2, 3 and 7 bytes and
whole, and compares the text exactly.

| Fixture | Covers |
| --- | --- |
| `text` | printing, CR LF, HT, BS, VT, FF |
| `cursor` | CUP, CUU, CUD, CUF, CUB, CNL, CPL, CHA, VPA, clamping |
| `editing` | ICH, DCH, ECH, EL, ED, erasing in the pen's background |
| `scrolling` | history, SU, SD, IL, DL, RI |
| `margins` | DECSTBM, DECOM, scrolling inside the region |
| `sgr` | attributes and colours, both parameter forms |
| `modes` | DECCKM, DECTCEM, bracketed paste, mouse modes and encodings |
| `alternate` | 1049 and 47 |
| `save-reply-reset` | DECSC, DECRC, DSR, DA, DECXCPR (unanswered by default), RIS |
| `unicode` | wide glyphs, combining marks, overwriting wide halves |
| `strings-invalid` | OSC, DCS, APC, PM and SOS consumed; invalid UTF-8; CAN and SUB |

A snapshot records, after each operation, the replies so far, the size,
cursor, modes, mouse state and pen, then every retained row, history first
and oldest first, with its soft wrap and its cells. A cell left out is
blank in the default attributes. Row identities and versions are not
recorded; `../versions.rs` checks them.

The snapshots were first captured from the vt100 crate (0.16.2), which
fux-vt replaced, never from fux-vt. Where vt100 differs from xterm and the
references, they were corrected by hand:

- `sgr`: SGR 2 after SGR 1 keeps bold beside dim (flags 31, not 30);
- `margins`: DECSTBM with origin mode reset homes the cursor to the first
  line, not the top margin (cursor (0, 0) after operation 1; DEC STD 070,
  DECSTBM);
- `alternate`: 1049 and 47 leave the cursor where it was (`ALT` at row 3,
  column 4), and 1049 clears the alternate screen in the pen's colours
  (red blanks, `bce`);
- `strings-invalid`: invalid UTF-8 prints U+FFFD, for `c0` and for
  `f0 9f` cut off by ESC, where vt100 dropped it; the lone `80` is a C1
  control read as Latin-1 and ignored, as in xterm.
