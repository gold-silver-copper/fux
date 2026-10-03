# References

The specifications fux-vt is checked against. Only this index and
`fetch.sh` are committed. The documents are free to download, but not
obviously free to redistribute, so run the script to get them (about 176
MB):

```sh
references/fetch.sh            # fetches what is missing
references/fetch.sh --force    # fetches everything again
```

**Which source decides.** When the standards, xterm and the engines in
`fux-vt/compare` disagree, use this order:

1. **ECMA-48** decides the standard control functions.
2. **DEC STD 070** decides DEC behaviour, with the VT520 manual as its
   readable companion.
3. **ctlseqs** and xterm itself decide xterm extensions. The comparison
   harness runs real xterm: `replay --engines all`.

A fux-vt test cites the section that sets its expected value.

## Contents

### `standards/`: formal standards

| File | What | Use it for |
| --- | --- | --- |
| `ECMA-48_5th_edition_1991.pdf` | Control Functions for Coded Character Sets; the same text as ISO/IEC 6429:1992 | C0/C1 controls, control-sequence syntax, every standard control function and mode |
| `ECMA-35_6th_edition_1994.pdf` | Character Code Structure and Extension Techniques (ISO/IEC 2022) | designating G0–G3 (`ESC ( 0`), SO/SI, locking shifts |
| `ECMA-43_3rd_edition_1991.pdf` | 8-bit coded character set structure (ISO 4873) | the 8-bit code table, C1 in 8-bit form |
| `ECMA-6_6th_edition_1991.pdf` | 7-bit coded character set (ISO 646 / ASCII) | the 7-bit code table |
| `ITU-T_T.416_1993.pdf` | ITU-T T.416 (ISO/IEC 8613-6), Open Document Architecture | §13.1.8 SGR: where `38:2:<colour space>:r:g:b` and the colon subparameters come from |

### `dec/`: DEC terminals, which everyone emulates

| File | What | Use it for |
| --- | --- | --- |
| `DEC_STD_070_Video_Systems_Reference_Manual_1991.pdf` | DEC STD 070, DEC's internal standard for its video terminals, with pseudocode for every function | the most rigorous source on DEC behaviour. Chapter 5, "Character Cell Display": the Last Column Flag (pending wrap) in "Insert or Replace Graphic Character" (page 5-139 on), autowrap, margins, origin mode. Chapter 4: Soft Terminal Reset (DECSTR, page 4-37) |
| `VT520_VT525_Programmer_Information_1994.pdf` | VT520/VT525 Programmer Information (EK-VT520-RM) | the latest DEC programmer manual: DECSTR's table of what a soft reset restores (§5, page 5-150), every DEC private mode |
| `VT510_520_Spec.pdf` | VT510/VT520 product specification | what the VT500 series implements |
| `VT330_VT340_Text_Programming_1988.pdf` | VT330/VT340 Text Programming | VT300-era text functions, character sets |
| `VT220_Programmer_Pocket_Guide_1984.pdf`, `VT220_Technical_Manual_1984.pdf` | VT220 | the VT220 baseline that DA1 `62` claims |
| `VT100_User_Guide_1979.pdf`, `VT100_Technical_Manual_1982.pdf`, `VT100_Programming_Reference_Card_1982.pdf` | VT100 | the original behaviour, quirks included |
| `vt100.net/vt510-rm/` | vt100.net's transcription of the VT510 Reference Manual, one page per function (open `contents.html`) | searchable DEC function pages: `DECSTR.html`, `DECAWM.html`, `DECSTBM.html`, `DECOM.html`, and so on |
| `vt100.net/vt220-rm/`, `vt100.net/vt100-ug/` | the same, for the VT220 manual and the VT100 user guide | |
| `vt100.net/dec_ansi_parser.html` | Paul Williams's DEC ANSI parser state machine, with its diagram inline | fux-vt's parser (fux-vt's README, "Parser") |

### `xterm/`: the de facto standard

| File | What | Use it for |
| --- | --- | --- |
| `ctlseqs.html`, `ctlseqs.pdf` | XTerm Control Sequences, by Thomas Dickey | every sequence xterm implements: DEC private modes, OSC, DECRQM, XTVERSION, mouse, bracketed paste, focus |
| `terminfo.src` (and `.gz`) | ncurses's terminfo source | what `TERM=xterm-256color` promises programs (`bce`, `smacs`, `smir`, `rep`, `hts`, `cbt`...); the rule in the audit prompt |
| `vttest/` | vttest source | the classic interactive conformance test; its menus show what correct output looks like |
| `esctest2/` | esctest2, Dickey's fork of George Nachman's automated test suite | automated conformance tests with expected values for xterm, one per function, readable as executable specifications |

### `unicode/`: Unicode 17.0.0, the version fux-vt's tables follow

| File | What | Use it for |
| --- | --- | --- |
| `UAX29_Text_Segmentation.html` | UAX #29, revision 47 | grapheme cluster boundaries |
| `UAX11_East_Asian_Width.html` | UAX #11, revision 44 | narrow and wide |
| `UTS51_Emoji.html` | UTS #51, revision 29 | emoji presentation, ZWJ sequences, modifiers, flags |
| `ucd/EastAsianWidth.txt`, `ucd/DerivedCoreProperties.txt` (InCB), `ucd/auxiliary/GraphemeBreakProperty.txt`, `ucd/auxiliary/GraphemeBreakTest.txt`, `ucd/emoji/emoji-data.txt` | the data files | the inputs `fux-vt/gen` generates its tables from, and the conformance cases |

### `modern/`: extensions with no formal standard

| File | What |
| --- | --- |
| `kitty_keyboard_protocol.html` | the kitty keyboard protocol (`CSI > u`, `CSI < u`, `CSI = u`, `CSI ? u`) |
| `mode_2026_synchronized_output.md` | synchronized output, mode 2026 (contour's vt-extensions) |
| `mode_2027_grapheme_clusters.tex` | grapheme cluster processing, mode 2027 (contour's terminal-unicode-core) |
| `mode_2048_in_band_resize.md` | in-band resize reports, mode 2048 |
| `osc8_hyperlinks.md` | hyperlinks, OSC 8 |
| `kitty_underlines.html` | kitty's underline styles (`4:0` to `4:5`) and underline colour (SGR 58, 59) |
| `osc133_iterm2_escape_codes.html` | iTerm2's escape codes; its "Shell Integration/FinalTerm" section documents FinalTerm's semantic prompt marks, OSC 133 `A` (prompt), `B` (command), `C` (output) and `D` (finished) |
| `osc133_semantic_prompts.md` | Per Bothner's semantic prompts proposal: OSC 133 extended (`L` fresh line, `A` a fresh line then a prompt, `N`, `P`, `I`, options such as `aid`, `k` and `cl`) |

## Where each audit finding is settled

The findings are listed in `~/Desktop/code/fux-audit-fixes-prompt.md`.

| Finding | Source |
| --- | --- |
| F1 pending wrap: what clears it, what saves it | DEC STD 070 ch. 5, the Last Column Flag ("Insert or Replace Graphic Character", and each function's pseudocode that clears it) |
| F2 SGR colon form and colour space | ITU-T T.416 §13.1.8; ECMA-48 §8.3.117 (SGR) |
| F3 DEC special graphics, G0/G1, SO/SI | ECMA-35; DEC STD 070 ch. 3 (code extension); VT520 manual, SCS |
| F7 REP | ECMA-48 §8.3.103 |
| F8 DECSTBM, VPA in origin mode, IL/DL to column 0, HPR/VPR | DEC STD 070 ch. 5; VT520 manual; ECMA-48 §8.3.158 (VPA), §8.3.160 (VPR), §8.3.59 (HPR), §8.3.67 (IL), §8.3.32 (DL) |
| IND/NEL, tab stops (HTS, TBC, CHT, CBT) | ECMA-48 §8.3.86 (NEL), §8.3.62 (HTS), §8.3.154 (TBC), §8.3.10 (CHT), §8.3.7 (CBT); IND is in DEC STD 070 and ctlseqs, as ECMA-48 withdrew it |
| IRM | ECMA-48 §7.2.10, §7.3.3 |
| BCE: blanks a scroll or insert brings in | DEC STD 070 ch. 5 (erase and scroll with the Current Rendition); ctlseqs and terminfo `bce` |
| DECSTR | VT520 manual p. 5-150 (its table); DEC STD 070 ch. 4, Soft Terminal Reset |
| 47, 1047, 1048, 1049 | ctlseqs (private modes) |
| F6 0x9c inside a DCS string; F9 U+FFFD | ECMA-48 §5.6 (control strings), §8.3.27 (DCS), §8.3.143 (ST); ctlseqs on UTF-8 and C1 |
| Grapheme clusters and widths | UAX #29, UAX #11, UTS #51; mode 2027 |
| Kitty keyboard flags | `modern/kitty_keyboard_protocol.html` |
