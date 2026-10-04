# References

The specifications fux-vt is checked against. Only this index, `fetch.sh`
and `.gitignore` are committed: the documents are free to download but not
obviously free to redistribute, so fetch them (about 170 MB):

```sh
references/fetch.sh            # fetches what is missing
references/fetch.sh --force    # fetches everything again
```

It needs `curl`, `wget` (for the vt100.net copies) and `git` (for
esctest2), and exits 1 listing anything it could not fetch.
`fux-vt-compare esctest` runs `xterm/esctest2` from here.

**Which source decides.** When the standards, xterm and the engines in
`fux-vt/compare` disagree:

1. **ECMA-48** decides the standard control functions.
2. **DEC STD 070** decides DEC behaviour, with the VT520 manual as its
   readable companion.
3. **ctlseqs** and xterm itself decide xterm's extensions; the harness
   runs real xterm (`fux-vt-compare replay --engines all`).

Where fux-vt follows xterm against the references, fux-vt's README lists
it under "Departures from the references". A fux-vt test cites the section
that sets its expected value.

## Contents

### `standards/`

| File | What | For |
| --- | --- | --- |
| `ECMA-48_5th_edition_1991.pdf` | Control Functions for Coded Character Sets (= ISO/IEC 6429:1992) | C0/C1 controls, sequence syntax, standard functions and modes, SPA/EPA |
| `ECMA-35_6th_edition_1994.pdf` | Character Code Structure and Extension Techniques (ISO/IEC 2022) | designating G0–G3, SO/SI, locking shifts |
| `ECMA-43_3rd_edition_1991.pdf` | 8-bit code structure (ISO 4873) | C1 in 8-bit form |
| `ECMA-6_6th_edition_1991.pdf` | 7-bit code (ISO 646 / ASCII) | the 7-bit table |
| `ITU-T_T.416_1993.pdf` | ITU-T T.416 (ISO/IEC 8613-6) | §13.1.8: SGR's colon form, `38:2:<colour space>:r:g:b` |

### `dec/`

| File | What | For |
| --- | --- | --- |
| `DEC_STD_070_Video_Systems_Reference_Manual_1991.pdf` | DEC STD 070, with pseudocode for every function | DEC behaviour. Ch. 5: the Last Column Flag (pending wrap, p. 5-139 on), autowrap, margins (5.4.3), origin mode, selective erase (5.11.1.2). Ch. 4: DECSTR (p. 4-37) |
| `VT520_VT525_Programmer_Information_1994.pdf` | VT520/VT525 Programmer Information (EK-VT520-RM) | DECSTR's table (p. 5-150), every DEC private mode |
| `VT510_520_Spec.pdf` | VT510/VT520 product specification | what the VT500 series implements |
| `VT330_VT340_Text_Programming_1988.pdf` | VT330/VT340 Text Programming | VT300-era text functions, character sets |
| `VT220_Programmer_Pocket_Guide_1984.pdf`, `VT220_Technical_Manual_1984.pdf` | VT220 | the VT220 class that DA1's `62` claims |
| `VT100_User_Guide_1979.pdf`, `VT100_Technical_Manual_1982.pdf`, `VT100_Programming_Reference_Card_1982.pdf` | VT100 | the original behaviour |
| `vt100.net/vt510-rm/`, `vt220-rm/`, `vt100-ug/` | vt100.net's transcriptions, one page per function (open `contents.html`) | searchable function pages: `DECSTR.html`, `DECSLRM.html`, `DECSCA.html`… |
| `vt100.net/dec_ansi_parser.html` | Paul Williams's DEC ANSI parser state machine | fux-vt's parser (fux-vt's README, "Parser") |

### `xterm/`

| File | What | For |
| --- | --- | --- |
| `ctlseqs.html`, `ctlseqs.pdf` | XTerm Control Sequences | every xterm sequence: private modes, OSC (the palette: 4, 5, 10–19, 104, 105, 110–119), DECRQM, XTVERSION, XTSAVE/XTRESTORE, mouse |
| `terminfo.src` (and `.gz`) | ncurses's terminfo source | what `TERM=xterm-256color` promises programs (`bce`, `rep`, `smir`…) |
| `vttest/` | vttest source | the classic interactive conformance test |
| `esctest2/` | Dickey's esctest2, from George Nachman's suite | automated tests with xterm's expected values, one per function |

### `unicode/`: Unicode 17.0.0, the version of fux-vt's tables

| File | What |
| --- | --- |
| `UAX29_Text_Segmentation.html` | UAX #29, revision 47: grapheme clusters |
| `UAX11_East_Asian_Width.html` | UAX #11, revision 44: widths |
| `UTS51_Emoji.html` | UTS #51, revision 29: emoji presentation, ZWJ sequences, modifiers, flags |
| `ucd/` | `EastAsianWidth.txt`, `DerivedCoreProperties.txt` (InCB), `auxiliary/GraphemeBreakProperty.txt`, `auxiliary/GraphemeBreakTest.txt`, `emoji/emoji-data.txt`. `fux-vt/gen` builds its tables from four of them, given in one directory (`fux-vt/gen/README.md`) |

### `modern/`: extensions with no formal standard

| File | What |
| --- | --- |
| `kitty_keyboard_protocol.html` | the kitty keyboard protocol (`CSI > u`, `CSI < u`, `CSI = u`, `CSI ? u`) |
| `kitty_underlines.html` | underline styles (`4:0` to `4:5`) and colour (SGR 58, 59) |
| `mode_2026_synchronized_output.md` | synchronized output |
| `mode_2027_grapheme_clusters.tex` | grapheme cluster processing |
| `mode_2031_color_scheme_updates.md` | colour-scheme reports (`CSI ? 996 n`, `CSI ? 997 ; 1/2 n`) |
| `mode_2048_in_band_resize.md` | in-band resize reports |
| `osc8_hyperlinks.md` | hyperlinks, OSC 8 |
| `osc133_iterm2_escape_codes.html` | iTerm2's escape codes; "Shell Integration/FinalTerm" has OSC 133 `A`–`D` |
| `osc133_semantic_prompts.md` | Per Bothner's semantic prompts proposal, extending OSC 133 (`L`, `N`, `P`, `I`, options) |

## Which source settles what

| Topic | Source |
| --- | --- |
| pending wrap: what sets, clears and saves it | DEC STD 070 ch. 5, the Last Column Flag |
| SGR, the colon form and colour space | ECMA-48 §8.3.117; ITU-T T.416 §13.1.8 |
| character sets, G0/G1, SO/SI | ECMA-35; DEC STD 070 ch. 3; VT520 manual, SCS |
| REP | ECMA-48 §8.3.103 |
| DECSTBM, origin mode, VPA, VPR, HPR, IL, DL | DEC STD 070 ch. 5; VT520 manual; ECMA-48 §8.3.158, .160, .59, .67, .32 |
| left and right margins, DECIC, DECDC | DEC STD 070 5.4.3; VT510 manual; ctlseqs |
| protected glyphs, selective erase | DEC STD 070 5.11.1.2 (DECSCA, DECSED, DECSEL); ECMA-48 (SPA, EPA) |
| NEL, IND, tab stops (HTS, TBC, CHT, CBT) | ECMA-48 §8.3.86, .62, .154, .10, .7; IND from DEC STD 070 and ctlseqs (ECMA-48 withdrew it) |
| IRM | ECMA-48 §7.2.10, §7.3.3 |
| BCE | DEC STD 070 ch. 5; ctlseqs; terminfo `bce` |
| DECSTR | VT520 manual p. 5-150; DEC STD 070 ch. 4 |
| alternate screen (47, 1047, 1048, 1049), XTSAVE/XTRESTORE, the palette | ctlseqs |
| control strings, ST inside a DCS, U+FFFD | ECMA-48 §5.6, §8.3.27, §8.3.143; ctlseqs on UTF-8 and C1 |
| grapheme clusters and widths | UAX #29, UAX #11, UTS #51; mode 2027 |
| kitty keyboard flags | `modern/kitty_keyboard_protocol.html` |
