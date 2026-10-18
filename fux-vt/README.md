# fux-vt

The terminal emulator inside [fux](https://github.com/gold-silver-copper/fux):
a byte parser and a bounded screen with history, stable row identities and
change marks. Every allocation is bounded, every resize is transactional,
and nothing a program writes reaches the host except through options the
host turns on.

The fixed-size cell and much of the inherited behaviour come from Jesse
Luehrs's MIT-licensed [vt100](https://crates.io/crates/vt100) crate, whose
license is kept in `LICENSE`; the parser and grid are fux-vt's own.

```rust
let size = fux_vt::Size::new(24, 80)?; // rows and columns, neither zero
let mut parser = fux_vt::Parser::new(size, 8 << 20)?; // and bytes of history
parser.process_with_replies(b"\x1b[1mhi\x1b[6n", |reply| send_to_program(reply))?;
let cell = parser.screen().cell(0, 0).unwrap();
assert_eq!((cell.contents(), cell.bold()), ("h", true));
```

This document is the contract: what each sequence does, the options, the
limits, and where the standards leave the choice. The references are
ECMA-48, DEC STD 070, the VT520 manual, xterm's ctlseqs, Unicode 17 and the
specs of modern extensions
([`references/README.md`](https://github.com/gold-silver-copper/fux/blob/master/references/README.md)).
Where a standard specifies a behaviour, fux-vt does what it says: ECMA-48
for its control functions, DEC STD 070 for DEC's (over the DEC product
manuals), ECMA-35 for character sets, Unicode for text, the kitty
keyboard, OSC 8 and OSC 133 specs for their protocols, and ctlseqs for
xterm's own functions (over xterm 411 where xterm departs from it). Where
the standards are silent or call a behaviour undefined, it does what xterm
411 does, since fux sets `TERM=xterm-256color`. A row that differs from
xterm says so.

## Sequence matrix

CSI is ESC `[`. A missing or zero count is one unless noted. Sequences
count from one, the API from zero. "Margins" are the scroll region and,
with DECLRMM set, the left and right margins. Tests are in `tests/`
and `src/*/tests.rs`. Every mode SM, RM, DECSET and DECRST set is a
`Mode`, read with `Screen::mode(Mode::X)` as DECRQM reports it.

| Sequence | Behaviour | Tests |
| --- | --- | --- |
| Text, UTF-8 | Printed in ground state; widths from unicode-width 0.2; a character split across calls is completed. Invalid UTF-8 prints one U+FFFD, one column wide, per maximal subpart (Unicode 3.9), then rereads the byte that broke it off, and one per byte that starts no sequence (0xc0, 0xc1, 0xf5–0xff, and a continuation byte alone, 0x80–0xbf, which xterm reads as Latin-1); overlong and surrogate forms are invalid. UTF-8-encoded C1 characters are ignored. | `conformance::invalid_utf8_*` |
| Grapheme clusters | A character that continues the grapheme cluster of the cell just printed joins that cell (UAX #29 as written, GB9b's Prepend included, one character at a time, from tables `gen/` makes). A narrow cell whose cluster becomes two columns wide is widened over the cell under the cursor, except in the last column. Cursor moves and row edits end the cluster; SGR, modes and queries do not. A cluster keeps at most `CLUSTER_CAPACITY` (128) bytes and what its row's text budget allows; the rest is dropped, never split into another cell. `continues_cluster` tells a host what would join. | `unicode::tests`, `properties::printed_text_*`, `extended::*cluster*`, `conformance::a_prepend_*` |
| C0 controls | BS: back a column, stopping at the left margin unless already left of it (see CSI ? 45). HT: the next tab stop, else the right margin (the last column without DECLRMM); from right of the margin, or on a line outside the scrolling region, the next stop or the last column (DEC STD 070, HT, note 1); CHT as that many HTs. LF, VT, FF: down a line, scrolling at the bottom margin (between left and right margins only), CR too under LNM. CR: column 0, or the left margin if the cursor is at or right of it, or in origin mode. SO, SI: G1, G0 into GL. BEL and the rest: nothing visible. BS, HT, LF, VT, FF and CR end a pending wrap (see CSI ? 7). | `semantics::text_controls_*`, `conformance::a_tab_from_*` |
| ESC 7 / 8, CSI s / u | DECSC/SCOSC save and DECRC/SCORC restore the position with its pending wrap, origin mode, the pen, protection and the character sets. `CSI s` is DECSLRM while DECLRMM is set. In origin mode the restored column is at most the right margin. Each screen has its own saved position, pen, protection and character sets, as each of xterm's screens has; the position is clamped by a resize. | `extended::scosc_*`, `conformance::each_screen_saves_*` |
| ESC = / > | DECKPAM, DECKPNM: `Mode::ApplicationKeypad`, off; RIS and DECSTR reset it. State only. | `modes::decrqm_*` |
| ESC ( F, ESC ) F, ESC * F, ESC + F, ESC n / o, ESC N / O | SCS designates G0, G1, G2 or G3: `0` is DEC Special Graphics, any other set ASCII. SO, SI, LS2 and LS3 put G1, G0, G2 or G3 in GL; SS2 and SS3 invoke G2 or G3 for the next graphic character alone (ECMA-35, 6.3 and 9). With Special Graphics in effect, 0x5f–0x7e print a blank and `◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·`. The 96-character sets and the shifts into GR are unhandled. RIS and DECSTR designate ASCII into all four, G0 in GL. | `conformance::dec_special_graphics_*`, `conformance::g2_and_g3_*` |
| ESC 6 / 9 | DECBI, DECFI: the cursor a column back or forward; from the left margin back, or the right margin forward, the columns between the margins of each line of the scrolling region shift one the other way, a blank column coming in at that margin, the cursor staying. Outside the margins the cursor moves, ignored at the page's edge. Both end a pending wrap (DEC STD 070, Back Index, Forward Index, whose algorithms shift the columns whatever the cursor's line, where xterm shifts them only with the cursor in the scrolling region). | `margins::decbi_and_decfi_*` |
| ESC D / E | IND: a line feed, LNM aside. NEL: a line feed, then to the column CR goes to. | `conformance::ind_and_nel_*` |
| ESC M | RI: up a line, scrolling down at the top margin (between left and right margins only). Ends a pending wrap. | `margins::index_scrolls_between_the_margins` |
| ESC c | RIS: both screens and history cleared; every mode, the pen, margins, tab stops, protection, links, keyboard stacks, cursor shape and palette back to their defaults. The dynamic and special colours and XTSAVE's saved modes stay. Row identities are never reused. | `semantics::regions_origin_and_reset_*` |
| CSI ! p | DECSTR, as xterm does it: shows the cursor; resets IRM, DECOM, DECCKM, DECKPAM, DECLRMM, reverse wraparound and synchronized output; sets DECAWM to the terminal's setting, on (DEC STD 070, DECSTR: "NVM if present", and revision note 29, recalling it from NVR; the VT520 manual's table says off, and `xterm-256color`'s `is2` and `rs2` rely on it on); resets both screens' margins, the pen, protection, the character sets, the shown screen's saved cursor (home, default attributes and character sets) and the palette. Keeps the screen, cursor, pending wrap, tab stops, alternate screen, LNM, bracketed paste, focus reporting, mouse modes, cursor shape and keyboard state. | `conformance::a_soft_reset_*` |
| CSI 4 h / l | IRM, off: a printed glyph pushes what is at and after the cursor right, as ICH does, losing what passes the right margin; the row keeps its soft wrap. | `conformance::insert_mode_*` |
| CSI 20 h / l | LNM, off: LF, VT and FF return the carriage too; IND does not. RIS resets it, DECSTR keeps it. Other ANSI modes are unhandled. | `modes::lnm_*` |
| CSI A B C D E F G H d f \` a e | CUU and CPL stop at the top margin from at or below it, CUD and CNL at the bottom margin from at or above it, else at the screen's edge (xterm's `CursorUp`, `CursorDown`); CNL and CPL go to the column CR goes to. CUF and CUB stop at the right or left margin unless already past it. CUP, HVP, VPA and VPR count lines from the top margin and stay within the margins with DECOM set, and address the whole screen without (so VPR, unlike CUD, passes the bottom margin). CUP, HVP, CHA and HPA count columns from the left margin in origin mode, up to the right one. VPA and VPR keep the column (within the right margin in origin mode). HPR stops at the right margin in origin mode only. All end a pending wrap. | `conformance::line_and_column_addressing_*`, `margins::cursor_movements_*` |
| CSI b | REP: the last printed character, Pn more times, as if printed again; a wide glyph a one-column screen drops is still the last printed. After any control, sequence or string (REP included) it does nothing until a character is printed. After a cluster it repeats the cluster's first character (see "Where the standards leave the choice"); after a single shift, the character the shift invoked (ECMA-48 8.3.103: a graphic character "represented by one or more bit combinations"), where xterm prints the byte again in GL. Copies past a screen and history's worth are skipped a line at a time, with the same result. | `conformance::rep_*`, `margins::rep_*` |
| ESC H, CSI g, CSI I, CSI Z | Tab stops, one set for both screens: every eight columns at first and after RIS, kept by DECSTR and a resize. HTS sets one at the cursor's column. TBC 0 or none clears that one, 3 every one; the others do nothing, as DEC STD 070 (TBC) defines only these. With several parameters TBC acts on each in turn, as DEC STD 070's algorithm (TBC, p. 5-66) does (xterm and Ghostty read the first). CHT is HT n times; CBT goes back n stops, or to the first column (ECMA-48 8.3.7). Both end a pending wrap (see CSI ? 7), CBT moving back from the last column (xterm keeps the wrap). | `conformance::tab_stops_*`, `conformance::tbc_acts_on_each_of_its_parameters` |
| CSI @ P X | ICH, DCH, ECH, within the row. ICH and DCH stop at the right margin; outside the left and right margins they do nothing and keep a pending wrap, otherwise they end it. A wide glyph an edit cuts is blanked whole. DCH ends the row's soft wrap; ICH keeps it. ECH leaves guarded glyphs (SPA's). Blanks take the pen's colours and no other attribute (`bce`). | `margins::edits_stop_*` |
| CSI L M S T | IL, DL, SU, SD within the margins, counts clamped to the region. With left and right margins only the cells between them move, rows keeping their identity, soft wrap and prompt mark, and a wide glyph across a margin is blanked first. IL and DL do nothing with the cursor outside the margins, else move it to the left margin. A soft wrap stays with its row, and the row above the region keeps its own, as xterm keeps them. Rows scrolled off the top of the whole screen (no left and right margins) by SU, a line feed or a wrap go to history; other scrolls discard them. New lines take the pen's colours. | `margins::scrolling_is_bounded_*`, `extended::deleting_or_inserting_lines_*` |
| CSI J / K, CSI ? J / K | ED, EL, DECSED, DECSEL: 0 or none from the cursor, 1 to it, 2 all, each parameter in turn as DEC STD 070's algorithms do (`1;0` is 2; xterm reads the first alone); unhandled if none is known. ED and EL's erased cells take the pen's colours (`bce`); DECSED and DECSEL make each glyph a space in its own rendition and attributes, empty cells staying empty (DEC STD 070, DECSEL and DECSED, as DECSERA): the Space character, which copy, search, reflow and history read as they read a space printed, where xterm gives them the pen's colours (`2x4 '\e[41mab\e[mc\e[42m\e[1;2H\e[?1K'`). Erasing through the last column ends the row's soft wrap; ED removes the prompt mark of each row it erases whole but the cursor's, whose mark stays even when ED erases all of it, as EL leaves it (a shell redrawing its prompt erases from it). DECSED and DECSEL leave the glyphs DECSCA protects, ED and EL the guarded ones (see CSI " q). All end a pending wrap. | `conformance::blanks_brought_in_*`, `conformance::ed_and_el_carry_out_each_parameter`, `protection::selective_erase_leaves_each_position_its_rendition` |
| CSI r | DECSTBM: top defaults to 1, bottom to the last line. With top above bottom and the bottom on the screen they are set and the cursor goes home, obeying DECOM; otherwise nothing changes (DEC STD 070, 5-25, notes 2 and 3; xterm takes a bottom past the screen as the last line). | `conformance::decstbm_*` |
| CSI ? 6 h / l | DECOM, off. Setting or resetting homes the cursor (to the top and left margins when set). | `conformance::line_and_column_addressing_obeys_origin_mode` |
| CSI ? 7 h / l | DECAWM, on. A glyph in the last column (the right margin's, unless the cursor is past it) leaves the cursor there with a wrap pending (`Screen::pending_wrap`). The next glyph first goes to the next line's start (the left margin) and marks the row soft-wrapped; so does a glyph too wide for the rest of the line, except where the line feed cannot move the cursor. Printing, BS, HT, CHT, LF, VT, FF, CR, RI, cursor moves, ED, EL, ECH, ICH, DCH, IL, DL, DECSTBM, DECOM, resetting DECAWM and RIS end the wrap (DEC STD 070, Appendix D.6.1; xterm keeps it through HT and CHT); SU, SD, SGR, modes and queries keep it. Off, a glyph overwrites the last cell and sets no flag (DEC STD 070, Appendix D.6.1, which lists resetting DECAWM too; xterm keeps the flag in both), and a mark still joins it; a flag DECRC restores is the Last Column Flag all the same, which setting DECAWM again (or DECSTR) keeps, so the next glyph wraps, as the appendix has it; a wide glyph that no longer fits on the row is not printed, and the glyph before it stays, as xterm does. | `conformance::a_pending_wrap_*`, `conformance::the_last_column_flag_*`, `conformance::a_glyph_that_wraps_*`, `conformance::a_wide_glyph_with_no_room_*` |
| CSI ? 1 / 25 / 2004 h / l | DECCKM (off), DECTCEM (on), bracketed paste (off). DECCKM is read by `Screen::key_mode`; bracketed paste is the host's to pass to `encode::paste`; DECTCEM is state only. | `semantics::alternate_mouse_modes_saved_cursor_and_replies` |
| CSI ? Pm s / r | XTSAVE saves each listed mode fux-vt keeps (1, 4, 5, 6, 7, 9, 25, 45, 47, 66, 67, 69, 1000, 1002, 1003, 1004, 1005, 1006, 1045, 1047, 1049, 2004, 2026; 2031 and 2048 with their options); XTRESTORE sets it as DECSET or DECRST would, reset if never saved. RIS and DECSTR keep what was saved. | `modes::xtsave_*` |
| CSI ? 45 / 1045 h / l | Reverse wraparound, off; RIS and DECSTR reset it. With DECAWM on, BS and CUB at the line's first column go on from the last column of the line above, which takes one of the count: for 45 a soft-wrapped row above, for 1045 any row above, or the bottom margin from the top margin (1045 wins). A pending wrap counts as one column past the last. The cursor stops at the first row. | `modes::reverse_wraparound_*` |
| CSI ? 69 h / l, CSI Pl ; Pr s | DECLRMM, off. Set, DECSLRM sets the left and right margins (Pl defaults to 1, Pr to the last column) if left is left of right and right is on the screen (DEC STD 070, DECSLRM, notes 2 and 3; xterm takes a right margin past the screen as the last column), and homes the cursor, obeying DECOM; otherwise nothing changes. Resetting DECLRMM restores the screen's edges, and `CSI s` is SCOSC again. They bound what DEC STD 070 (5.4.3) lists, as each row here says. DECALN, DECSTR, RIS and a resize reset them. A wrap pending where the right margin was is carried out from there. Read with `Screen::left_right_margins`. | `margins::*` |
| CSI Pn SP @ / A | SL, SR: the columns between the left and right margins of each line of the scrolling region move Pn left or right (ECMA-48 8.3.121, 8.3.135), blanks in the pen's colours coming in; nothing if the cursor is outside the margins, as xterm bounds them. The cursor and a pending wrap stay. | `margins::sl_and_sr_*` |
| CSI Pn ' } / ~ | DECIC, DECDC: Pn columns inserted or deleted at the cursor's column in each line of the scroll region, between the left and right margins, blanks in the pen's colours; nothing if the cursor is outside them. The cursor and a pending wrap stay; DECDC ends each line's soft wrap. | `margins::decic_and_decdc_*` |
| CSI Ps " q, ESC V / W | DECSCA 1 protects the glyphs printed after it from DECSED and DECSEL (DEC STD 070, 5.11.1.2), 0, 2 or none stops; SPA and EPA start and end a guarded area, whose glyphs ED, EL and ECH leave (ECMA-48 8.3.140, 8.3.49, with ERM reset). Each glyph keeps the protection it was printed with, either, both or none, and each erase spares its own kind only (xterm keeps one kind for the whole terminal, the last DECSCA's or SPA's). Protection is the glyph's: it moves with it through edits, scrolling and history, and REP repeats it; a glyph printed over it and every blank are unprotected. An erase that finds none leaves the protection as it is (DEC STD 070, 5.11.1.2; ECMA-48 8.3.140), where xterm ends it after an ED of the whole screen. DECSC saves DECSCA's attribute; DECSTR ends both for the glyphs printed next, and RIS. `CellRef` does not show it. | `protection::*` |
| CSI ? 4 / 5 / 8 / 66 / 67 h / l | Kept for DECRQM and XTSAVE. DECSCLM (4), DECSCNM (5), DECBKM (67): off, reset by RIS, kept by DECSTR. DECSCLM is state only; DECSCNM (reverse video) is the host's to draw, read with `Screen::mode` (fux turns each cell's inverse over). DECBKM is read by `Screen::key_mode`: off, Backspace sends DEL, as `xterm-256color`'s `kbs` says, and BS with Ctrl; set, BS, and DEL with Ctrl (ctlseqs; xterm's backarrow toggle). Under the kitty protocol an unmodified Backspace keeps these legacy bytes (its spec, "Disambiguate escape codes"), and its codes stay kitty's. DECNKM (66) is the keypad mode of ESC = and ESC >. DECARM (8) is reported permanently reset. | `modes::decrqm_*` |
| CSI ? 1004 h / l | `Mode::FocusReporting`, off; RIS resets it. Read by `Screen::focus_report`; DECRQM reports it. | `semantics::focus_and_cursor_shape`, `modes::decrqm_*` |
| CSI ? 2026 h / l | `Mode::SynchronizedOutput`, off: the program is drawing a frame. Holding the display is the host's: `Parser::process_until_stop` stops right after a sequence that sets the mode (`Stop::Begun`, while set too) or resets it while set (`Stop::Ended`: DECRST naming it among other modes, XTRESTORE, RIS, DECSTR), so a host that reads output as it comes knows when a frame is whole; one that holds a frame's bytes unread finds where the parser will end it with `Parser::frame_scan`, carrying nothing out (`FrameScan::end`). Any resize ends it too. | `opt_in::synchronized_output_*`, `opt_in::process_until_stop_*`, `parser::tests::a_frame_scan_*` |
| CSI ? 2048 h / l | With `Feature::InBandResize` only: `Mode::InBandResize`, off. Each set reports `CSI 48 ; rows ; cols ; 0 ; 0 t`; `Parser::resize_report` gives the report after a resize. RIS ends it. | `opt_in::in_band_resize_*` |
| CSI Ps SP q | DECSCUSR: `cursor_shape()` is the `CursorShape` Ps names in ctlseqs: 0 (the default) and 1 a blinking block, 0 kept apart as `Zero`, which many terminals take for their own shape, so a host can pass it on as 0 (DECRQSS answers it 1, as xterm), 2 to 6 the steady block, the underlines and the bars, 7 `Initial`, xterm's initial resources, the terminal's own shape as before any is set. Another Ps is ignored. RIS resets it to `Initial`. State only. | `semantics::focus_and_cursor_shape`, `conformance::decscusr_*` |
| CSI ? 47 h / l | Switches to or from the alternate screen without clearing it. The cursor, pending wrap, origin mode and margins carry across; each screen keeps what DECSC saved on it. The alternate screen has no history, and no cells until first shown. | `conformance::switching_screens_*` |
| CSI ? 1049 h / l | Set: DECSC, switch, clear the alternate screen in the pen's colours (ending a pending wrap), cursor unmoved. Reset: switch back, DECRC. | `semantics::alternate_mouse_modes_saved_cursor_and_replies` |
| CSI ? 1047 / 1048 h / l | 1047: 47, clearing the alternate screen on leaving it. 1048: DECSC when set, DECRC when reset. | `conformance::modes_1047_and_1048` |
| CSI ? 9 / 1000 / 1002 / 1003 h / l | `mouse_protocol_mode()`; the latest set wins, and a reset clears only the mode in force. Read by `Screen::encode_mouse`. | `semantics::alternate_mouse_modes_saved_cursor_and_replies` |
| CSI ? 1005 / 1006 h / l | `mouse_protocol_encoding()`, by the same rules. Read by `Screen::encode_mouse`. | `semantics::alternate_mouse_modes_saved_cursor_and_replies` |
| CSI m | SGR. 0 reset; 1 bold, 2 dim, both possible (see "Where the standards leave the choice"); 3 italic; 4 underline, 21 double, `4:0`–`4:5` none, single, double, curly, dotted, dashed (`4:` is `4:0`; larger numbers change nothing); 5 slow, 6 rapid blink, each replacing the other; 7 inverse; 8 hidden; 9 strikeout; 22–29 end them (22 bold and dim, 24 any underline). 30–37, 90–97, 40–47, 100–107 indexed colours; 39, 49, 59 defaults. 38, 48, 58 (foreground, background, underline colour): `5;n`, `2;r;g;b` (values the list lacks are 0; another kind takes only itself), `5:n`, `2:r:g:b`, `2:cs:r:g:b` (colour space ignored). An out-of-range colour is skipped; the rest applies. | `conformance::sgr_*`, `conformance::underline_styles_*` |
| CSI 5 n / 6 n / c | DSR 5 → `CSI 0 n`. DSR 6 → `CSI row ; col R`: the line counted from the top margin with DECOM set (the first line if above it), the column from the left margin in origin mode, a cursor waiting to wrap one past the last column (at it with `Options::identity`). DA (`CSI c`, `CSI 0 c`) → `CSI ? 1 ; 2 c` (`CSI ? 62 ; 6 ; 21 ; 22 ; 28 c` with an identity: level 2, with selective erase, horizontal scrolling, colour text and rectangular editing, DEC STD 070, DA, Table 1). Other parameters and private forms are unhandled unless an option answers them. | `conformance::cursor_reports_*` |
| ESC # 8 | DECALN: the screen filled with `E` in default attributes; all four margins reset (DECLRMM stays), origin mode off, cursor home, no wrap pending; the pen keeps its colours and loses its attributes. Each row's soft wrap and prompt mark end; a row already all plain `E` keeps its version. Other `ESC #` are unhandled. | `modes::decaln_*` |
| ESC Z | DECID: answered as DA. | `modes::decid_*` |
| CSI … $ x / z / { / v | The rectangular area operations (DEC STD 070, 5.12.1): DECFRA fills with Pch (32–126 through GL's set, so Special Graphics too; 160–255 Latin-1; others ignored, where xterm takes any character) in the pen, as printing writes it; DECERA makes each character a space in the pen's colours (`bce`), its attributes cleared, unoccupied positions staying so; DECSERA makes each glyph DECSCA does not protect a space in its own attributes, SPA's guarded ones too, empty cells staying empty; DECCRA copies text and attributes, read whole first, clipped at the screen. DECCARA and DECRARA (`$ r`, `$ t`) and DECSACE (`* x`) are not implemented: unhandled. Rectangles are `Pt ; Pl ; Pb ; Pr` from 1, defaults the whole screen, clamped to it, or in origin mode from and to the margins (as DECRQCRA's); one inverted is ignored; pages are ignored. The cursor, a pending wrap and each row's soft wrap stay; a wide glyph DECFRA, DECERA, DECSERA or DECCRA cuts (DECCRA's at the source's edges too) is blanked whole; filled and copied cells lose their links. DA names them, rectangular editing (28), with an identity (see CSI c). | `rectangles::*` |
| CSI Pi ; Pp ; Pt ; Pl ; Pb ; Pr * y | DECRQCRA, with `Feature::RectangleChecksums`; otherwise unhandled. | `opt_in::rectangle_checksums_*` |
| OSC, DCS, APC, PM, SOS | Consumed. OSC ends at BEL or ST (ESC `\`), the others at ST; 0x9c is a byte of a UTF-8 character, not ST. CAN and SUB cancel. An OSC's payload leaves out the controls a command string cannot hold (ECMA-48 8.3.89 and 5.6 allow BS to CR) and DEL, as Williams's DEC parser model does (xterm 411 shows each as `?`: `1x4 '\e]2;a\x01b\x07'`). Payloads are kept only for an option: OSC with `Events`, `Hyperlinks` or `Palette`; an OSC's first 64 bytes with `PromptMarks` alone; a DECRQSS request's first 4 bytes with `SettingReports`. | `conformance::a_control_string_*` |
| DCS P1 ; P2 ; P3 q … ST | With `Feature::Pictures` only; otherwise ignored, as every DCS fux-vt does not answer. Sixel (DEC STD 070, the VT340's; ctlseqs, "Sixel Graphics"): decoded as it comes into a `sixel::Picture` of RGBA pixels, one kept for each position of the sixel grid and shown as square pixels: a position as tall, to its width, as P1's macro says (2:1 by default; DEC STD 070, 7.1) or the raster attributes' `" Pan ; Pad` (before any sixel, 8.1.2) shows as that many rows, or columns. Sixels past what a picture may keep (4096 positions a side, 4096 by 2160 in all, and the pictures' budget) are not painted, and the picture keeps what fits (11.2.2: sixels past the right margin "are not printed"); it shows at most 4096 pixels a side. Pixels no sixel sets stay clear for P2 1, and take colour register 0, the background, for P2 0 and 2 (7.2), over the picture and the raster attributes' extent, which sixels may draw past; an extent past what a picture may hold is cut to it, as it limits nothing (DEC STD 070, 8.1.2). P3, the grid size, is the screen's one grid, its pixel (7.3). The picture is placed at the cursor, a row of cells at a time down from it, scrolling at the bottom and clipped at the right edge, each cell filled with `PLACEHOLDER`, not printed: no pen, insert mode, cluster or `Prints` reaches it (DEC STD 070-9, 11.1); the cursor ends below it, in its first column. Text or an erase over a picture, or its rows leaving history, lets it go, as does a picture placed over it; scrolling carries it; a screen keeps 64 at most, and the pictures of both screens and the one being drawn, as it grows, hold `PICTURE_BYTES` (96 MiB, `Parser::set_picture_bytes`) of pixels at most, those no longer shown let go first, then the oldest; RIS keeps none. The cell size is the host's (`Parser::set_host_graphics`, answering `CSI 14 t` and `CSI 16 t`), at least 2 by 6, else 10 by 20; DA1 says `4` and XTSMGRAPHICS answers when the host shows sixel. Read with `Screen::pictures`; `sixel::encode` writes one back. | `extended::*sixel*`, `sixel::tests` |
| OSC 133 | With `Feature::PromptMarks` only. `A`, `N`: a fresh line (CR and IND unless in the first column), then the row is marked as a prompt's start (`Row::starts_prompt`); `N` first ends the command running (`Screen::commands`), as a `D` with no status. `P` with `k=i` or no `k`: the row marked, no fresh line. `L`: the fresh line alone. `C` marks where a command's output starts and `D ; status` where it ends and how (`Screen::commands`; `C ; late` places the start after the line the host last submitted, `Parser::note_submit`; `Commands::full_screen`, the start was on the alternate screen; `Commands::started` counts the commands begun, a `C` while one runs beginning none). With `Parser::require_mark_nonce`, a mark counts only with `nonce=NONCE` among its options, or with `nonce=NESTED` while a command the shell marked runs: a program it ran marks commands within it until the shell marks again. `S ; activity=STATE` is a state report, a relay's word for what the program it runs does (STATE `reading`, `polling`, `waiting`, `busy`, `idle` or `unknown`; another is no report): under the same nonce rule (the nested nonce counting while a command the shell marked runs), it is kept as `Commands::report`, and changes no command; `Commands::marks` counts every mark that counts, a report among them, and `Commands::reported` gives the last report's state while no mark came after it. The rest change nothing. The mark moves with its row through scrolling, history and IL/DL, and through a reflow onto the row its first cell goes to; ED erasing the row whole, DECALN and RIS remove it; EL keeps it. Marks change no version. | `prompts::*` |
| Anything else | Ignored; C0 controls inside a CSI still execute. A complete CSI or escape sequence fux-vt does not implement goes to `Sink::unhandled`. Unknown DEC private modes, and sequences cut short by the parser's bounds, are dropped unreported. | `semantics::ignored_sequences_*`, `extended::sequences_*` |

## Opt-in outputs

`Parser::new` uses `Options::default()`: everything below is off, and the
behaviour is the table above. `Parser::with_options` turns each on
independently: a `Feature` each (`Options::new().with(Feature::Reflow)`),
and `identity` by `Options::with_identity`. `Parser::process_with`
delivers to a `Sink`, whose `reply`, `event` and `unhandled` methods
discard by default.

| Feature | Behaviour | Tests |
| --- | --- | --- |
| `Events` | OSC 0 → `IconName`, `Title`; OSC 1 → `IconName`; OSC 2 → `Title`; OSC 52 `Pc;Pd` → `Clipboard { selection, data }` unless `Pd` is `?`; OSC 10–19 → `ColorQuery { number, bel }` for each `?`, each parameter being the next colour; BEL in ground, escape or CSI state → `Bell`. Payloads are raw bytes. An OSC over `OSC_PAYLOAD_LIMIT` (64 KiB) gives no event, and nothing past the limit is buffered. | `opt_in::events_*`, `opt_in::osc_payloads_*` |
| `ExtendedReplies` | DECXCPR `CSI ? 6 n` → `CSI ? row ; col R`, as DSR 6; DA2 `CSI > c` → `CSI > 1 ; 10 ; 0 c`; DECRQM as `ModeReports`. The DSRs of DEC's other devices, which fux-vt has none of (DEC STD 070, DSR, and its extensions): `?15` → `?13` (no printer), `?25` → `?23` (no UDK extension, where xterm says unlocked), `?26` → `?27;0` (keyboard language unknown, as a VT220 reports it), `?55` → `?53` (no locator), `?56` → `?57;0`, `?62` → `CSI 0 * {` (no macro space), `?63;Pid` → `DCS Pid ! ~ 0000 ST` (no macros), `?75` → `?73` (power-up) until reported once after the screen is made or RIS, then `?70` (xterm answers 70 at once), `?85` → `?83` (no sessions). | `opt_in::extended_replies_*` |
| `ModeReports` | DECRQM alone. `CSI ? Ps $ p` → `CSI ? Ps ; Pm $ y`: 1 set or 2 reset for 1, 4, 5, 6, 7, 9, 25, 45, 47, 66, 67, 69, 1000, 1002, 1003, 1004, 1005, 1006, 1045, 1047, 1049, 2004, 2026, 2031 (with `ColorSchemeUpdates`), 2048 (with `InBandResize`); 4 for 8; 3 for 2027 (`Mode::GraphemeClustering`, permanently set); 0 otherwise, each a `ModeStatus`. `CSI Ps $ p`: 1 or 2 for IRM (4) and LNM (20); 4 (permanently reset) for GATM, KAM, CRM, SRTM, ERM, VEM, BDSM, DCSM, HEM, PUM, FEAM, FETM, MATM, TTM, SATM and TSM (1 to 3, 5 to 11, 13 to 18), which have the reset state only (ECMA-48 7.1): the keyboard is never locked, controls are performed, ED, EL and ECH spare a guarded area, there is no implicit bidirectional processing and no data component; 3 (permanently set) for SRM (12), GRCM (21) and ZDM (22), which have the set state only: SIMULTANEOUS (ECMA-48 7.2.15; DEC STD 070's SRM, local echo off, the default), as fux-vt never images what is typed; CUMULATIVE (7.2.8), as an SGR changes only the renditions it names; DEFAULT (F.4.2), as a parameter of 0 is its default; else 0, as for 19, the 4th edition's EBM, which the 5th removed (F.5.1). | `opt_in::mode_reports_*`, `modes::grapheme_clustering_*`, `modes::decrqm_*` |
| `InBandResize` | Mode 2048 (see its row). Off: unrecognized, DECRQM 0. | `opt_in::in_band_resize_*` |
| `SizeReports` | `CSI 18 t` → `CSI 8 ; rows ; cols t`. `CSI 14 t` (pixels) stays unhandled. | `opt_in::size_reports_*` |
| `ColorSchemeUpdates` | Mode 2031 tracked: `Mode::ColorSchemeUpdates`, DECRQM; RIS ends it. The host sends the reports and answers `CSI ? 996 n`, which stays unhandled. Off: unrecognized, DECRQM 0. | `opt_in::colour_scheme_updates_*` |
| `KittyKeyboard` | A kitty keyboard flag stack per screen: `CSI > Ps u` pushes (a full stack of 32 drops its oldest), `CSI < Ps u` pops Ps (1), `CSI = Ps ; Pm u` sets (Pm 1 replace, 2 add, 3 remove); of a flags number only the spec's five bits (1 to 16) are kept; `CSI ? u` → `CSI ? flags u`. `CSI > 4 ; Pv m` sets modifyOtherKeys (Pv 0 or none: off; 1 to 3 its levels, 3 sending unmodified keys as `CSI 27 ; 1 ; code ~` too; past 3 no key changes), and `CSI > m` with no parameters turns it off, as it resets every resource (ctlseqs, XTMODKEYS), and so does `CSI > 4 n` (disable); other resources are unhandled, `CSI > 0 m` (modifyKeyboard's reset) among them. RIS clears it all. Read with `Screen::kitty_keyboard_flags`, `modify_other_keys` (a `keys::encode::OtherKeys`, `None` when off); the host encodes keys. Off: all unhandled. | `extended::kitty_*`, `extended::modify_other_keys_*` |
| `Reflow` | A resize re-wraps the primary screen and its history. Each logical line (soft-wrapped rows and the row ending it) loses its blank tail and is laid out at the new width without splitting wide glyphs: one that does not fit leaves a blank spacer, which later reflows and copies skip. The cursor and saved cursor stay on their characters. Surplus rows go in order: blank lines below the cursor (only as far as the screen's own lines are past the new height, so none is given up to bring a history row back above the cursor), the oldest lines into history, history past its limit; rows still below the screen once the cursor's row is at the top are dropped. A line's k-th row keeps the identity of its k-th row before; every row takes a new version. The margins reset. The alternate screen resizes without reflow. | `extended::*reflow*`, `extended::a_shrink_takes_no_history_back_onto_the_screen`, `properties::reflow_*` |
| `identity` | An `Identity { name, version }`. DA → `CSI ? 62 ; 6 ; 21 ; 22 ; 28 c`; DA2 → `CSI > 1 ; Pv ; 0 c`, Pv = the version's parts, at most three, as digits in base 100: `1.2.3` is 10203, `1.2` is 102, `7` is 7 (a `-` or `+` suffix dropped); XTVERSION `CSI > q` → `DCS > \| name version ST`, unless name and version pass `Identity::MAX_LEN` (48) bytes; DSR 6 and DECXCPR report a cursor waiting to wrap at the last column. Without one, XTVERSION is unhandled. | `extended::*identity*` |
| `Hyperlinks` | OSC 8 `params ; URI`: glyphs printed while a link is open take it; an empty URI or RIS closes it. `Screen::hyperlink()` is the open link; `Row::link` and `Screen::link` give a cell's `Hyperlink`: `uri()`, `id()` (`id=` in params) and `key()`, equal for the cells of one OSC 8 without an id, or of one id and URI. Blank cells have none; a wide glyph's halves share one. Links follow their cells through scrolling, history, edits, resize and reflow. A URI over `URI_LIMIT` (2083 bytes), an id over `ID_LIMIT` (250), a byte outside printable ASCII, or an OSC over the payload limit opens nothing and closes the open link. Each screen holds at most 4 MiB of URIs and ids (64 bytes more a link); when full, the links its rows no longer have are let go, then the oldest history rows lose theirs until half is free; the screen's rows keep theirs, and a link that still does not fit is dropped, with no retry for 256 links. A changed link changes the row's version. | `hyperlinks::*` |
| `SettingReports` | DECRQSS (`DCS $ q Pt ST`), as xterm 411 answers it: `DCS 1 $ r Pt ST` for `m` (the pen), ` q` (DECSCUSR), `r` (DECSTBM), `s` (DECSLRM, the screen's edges without DECLRMM) and `"q` (DECSCA: 1 while DECSCA 1 protects, else 0, SPA being no DECSCA); `DCS 0 $ r ST` for another request or one over 4 bytes. The pen is `0`, then 1, 4, 5, 7, 8, 2, 3, 9, 21, foreground, background (16 colours short, others `38:5:n` or `38:2::r:g:b`), with an underline style as `4:n` in 4's place, rapid blink 6 in 5's and the underline colour last (`58:…`): a curly underline alone is `0;4:3`, which neovim checks for. `Attributes::write_sgr` writes it (`SgrForm::Report`), and the same attributes as a host paints them on a terminal (`SgrForm::Paint`: semicolon colours, the underline colour first, an underline style only if the terminal draws them). The terminal's own cursor shape (`Initial`) is not answered, since vim reads an invalid answer as keys. | `opt_in::setting_reports_*` |
| `PromptMarks` | OSC 133 (see its row). | `prompts::*` |
| `Palette` | The colours a program sets, as xterm 411 keeps them. OSC 4 sets or queries palette entries (pairs of number and specification; 256–260 are the special colours, OSC 5's 0–4); OSC 10–19 the dynamic colours, each parameter the next; OSC 104, 105, 110–119 reset them. Specifications: `rgb:R/G/B` (1–4 hex digits a channel) and `#RGB` to `#RRRRGGGGBBBB`; names and other colour spaces are not read. Kept at 8 bits a channel; answered `OSC 4 ; n ; rgb:RRRR/GGGG/BBBB`, each byte twice, ended as asked. OSC 4 and 5 stop at the first bad number or specification; OSC 10–19 skip a bad one; OSC 104 and 105 stop at the first non-number; OSC 110–119 with a parameter do nothing; OSC 105 alone resets every special colour (ctlseqs; xterm 411 resets none). Unset entries are answered with the host's colour for them, where it gave one (`Parser::set_host_palette`, entries 0 to 15, which no reset clears and which change nothing drawn), else with xterm's defaults (16 colours, 6×6×6 cube, grey ramp); unset dynamic colours become `ColorQuery` events with `Events`; unset special colours are not answered. RIS and DECSTR reset the palette only. Read with `Screen::palette_color`, `dynamic_color`, `colors_changed`; the host draws them. | `palette::*`, `palette::an_entry_not_set_is_answered_with_the_hosts_colour` |
| `Pictures` | Sixel (see its row) for a host that draws pictures. Off, a sixel string is read and dropped: nothing is decoded or allocated, its cells hold what the program wrote around it (no `PLACEHOLDER`), the cursor stays, and DA1 and XTSMGRAPHICS say no sixel whatever `Parser::set_host_graphics` says. | `extended::*sixel*`, `screen::tests::pictures_*` |
| `RectangleChecksums` | DECRQCRA → `DCS Pi ! ~ xxxx ST`, xterm's default sum, the VT520's: each cell's character plus 0x08 hidden, 0x10 underline, 0x20 inverse, 0x40 blink, 0x80 bold, summed in 16 bits and negated. A character past Latin-1 or below a space, and a wide glyph's second half, count as ESC; a Special Graphics glyph as its code; combining marks are added; an empty cell is a space. `Pp` is ignored. The rectangle is one-based, relative to and clamped by the margins in origin mode, the screen otherwise; 0 is its whole extent; an inverted one sums to `0000`. It lets a program read the screen, so fux leaves it off; esctest needs it. | `opt_in::rectangle_checksums_*` |

## Keys

`fux_vt::keys` is the host's side of input: what a user's terminal sends,
decoded, and what a program asked for, encoded. It needs no option.

- **Decoding:** `keys::decode::Decoder` turns a terminal's raw bytes,
  however split, into `Input`s: keys (legacy and xterm's modifiers, and
  the kitty keyboard protocol with disambiguate and alternate keys),
  bracketed pastes (whole, at most `PASTE_LIMIT`, 64 KiB, or what
  `Decoder::with_paste_limit` sets), focus changes,
  mouse reports (SGR and the default encoding) and answers to the host's
  own questions (DA1, the kitty flags, DECRQM, OSC 10 and 11, OSC 4
  palette entries, the colour scheme, DECRQSS for underline styles). A
  lone Escape is a key once
  `ESCAPE_DELAY` (35 ms) passes without more, which the host learns from
  `Decoder::deadline` and tells with `Decoder::timeout`. Every buffer is
  bounded.

**Encoding** reads what the program asked for from its screen:

| Encoder | Sends |
| --- | --- |
| `keys::encode::key_bytes(stroke, screen.key_mode(), out)` | the key as `Screen::key_mode()` asks (DECCKM, DECBKM, the kitty flags, modifyOtherKeys): kitty protocol; xterm's `CSI 27 ; m ; k ~`; or legacy bytes, Ctrl mapping only what Xlib maps (`Ctrl-;` is `;`), Alt an ESC before them (Alt-Escape is ESC ESC). `Key::F` holds a `Function`, F1 to F35: F13 to F35 are kitty's 57376 to 57398, and send nothing in the legacy modes, where xterm has no bytes for them. It takes any `KeyMode` |
| `keys::encode::paste(text, screen.mode(Mode::BracketedPaste), out)` | the text, framed by `CSI 200 ~` and `CSI 201 ~` with bracketed paste (2004) set; inside the frame every end marker (`ESC [ 201 ~`, or with the C1 CSI U+009B) is removed, as often as removing one makes another. Otherwise as pasted |
| `Screen::focus_report(focused)` | `CSI I` or `CSI O` with focus reporting (1004) set, else nothing (`None`) |
| `Screen::encode_mouse(event, out)` | a `keys::mouse::MouseEvent`, whose `MouseAction` carries its button (a press always names one), as xterm reports it in the program's mode and encoding: X10 (9) presses of buttons 1 to 3 without modifiers; 1000 presses and releases (the wheel's turns as presses, never released); 1002 also motion with a button held; 1003 also motion with none; in the default encoding (nothing past position 223), UTF-8 (1005, Cb too, nothing past 2015) or SGR (1006, any position, a release naming its button). `keys::mouse::mouse_bytes` takes them given |

`encode_mouse` returns whether it sent anything, and `focus_report` the
report or `None`. Motion within one cell is the host's to drop.

- **Colours:** `keys::colour` holds `Rgb` (read from and answered as
  `rgb:RRRR/GGGG/BBBB`), `Scheme` (dark or light, as mode 2031 reports
  it) and `Colours`, a terminal's foreground, background and scheme.
- `fux_vt::bytes::ByteQueue` is the byte queue the decoder reads
  from, for hosts that need one.

Tests are in `src/keys/decode.rs`, `src/keys/encode.rs` and
`src/keys/mouse/tests.rs` (each encoder against ctlseqs, every mouse report
decoding back to its event, no paste ending its bracket); `fuzz/`'s `keys`
target feeds the decoder arbitrary bytes and splits, re-encodes each mouse
event and paste it decodes, and checks both. `compare/`'s `encoders` holds
every encoder beside libghostty-vt's, with its recorded verdicts.

## Deliberate boundary

- Programs cannot operate the window: resizing, moving or iconifying it,
  132 columns and the title stack are refused in every mode (`CSI Ps t`
  other than `CSI 18 t` is unhandled; DECCOLM is ignored).
- By default, output causes no title, bell or clipboard side effects, and
  no OSC payload is kept. OSC 52 is only ever an event, which fux drops.
- Keypad, mouse and focus modes are state only. Keys, pastes, focus
  changes and mouse events are encoded only when the host asks
  (`keys::encode`, `Screen::focus_report`, `Screen::encode_mouse`, see
  [Keys](#keys)); nothing a program writes makes fux-vt send input.
- Sixel is the one graphics protocol (see its row), with
  `Feature::Pictures`: pictures are kept as pixels for the host to draw
  (`Screen::pictures`), within their budget (see [Bounds](#bounds)),
  however large their data says they are; none reaches the host's
  terminal unless the host sends it.
- Only the primary screen has history, up to its limit. CSI 3 J (ED 3,
  erase saved lines) and `Parser::clear_history` drop it, from either
  screen.
- Each compatibility choice is documented here and tested on its own,
  never hidden in a broad allowlist.

## Parser

A direct implementation of Paul Williams's DEC ANSI state machine
(<https://vt100.net/emu/dec_ansi_parser>), without a parser crate. UTF-8
replaces the 8-bit controls: raw C1 bytes are invalid UTF-8, and 0x9c in a
string is a byte of a character, not ST. Parameters saturate at
`u16::MAX`. At most 32 fields are kept: the rest are ignored and the
sequence runs with the 32, as DEC STD 070 (3.5.3.1) lets parameters
"beyond the maximum number supported" be (xterm 411 differs: `1x4
'\e[0;…;0;1;4;7mX'` with 30 zeros is plain there). More than two
intermediates make the sequence ignored. CAN and SUB cancel any
sequence; ESC starts a new one.

The common input skips the byte-at-a-time state machine, each fast path
doing exactly what it would:

- printable ASCII, found eight bytes at a time, is written into the row's
  cells a run at a time (a glyph at a time under IRM or Special Graphics);
- valid non-ASCII UTF-8 is decoded a run at a time;
- an escape sequence, or a CSI of parameters alone, is read in a loop of
  its own;
- an OSC payload is taken up to its terminator in one pass.

`parser::tests::fast_paths_equal_the_general_path*` and
`ascii_run_path_equals_scalar_dispatch_*` check them against the general
path, and the fuzz target `terminal` compares whole, chunked and
byte-at-a-time processing.

## Cells

A grid stores a cell in 8 bytes (`compact.rs`):

- 4 bytes of text, one character. A cluster of 5 to 17 bytes goes in the
  row's short text, a longer one (up to 128) in its long text, which holds
  at most 32 bytes a cell and 128 more. Overwritten text stays until the row
  runs out of room and is compacted; a cluster that still does not fit is
  cut to 17 bytes of whole characters.
- A 28-bit style number. Common styles are their own number: default or
  indexed colours with no underline colour and any rendition but rapid
  blink and underlines other than single, or a direct foreground on the default
  background, bold or italic (`style.rs`). Other styles are kept once in a
  table per screen. The table is swept, not counted: once it holds twice
  the styles in use at the last sweep, and at least max(4096, cells / 8),
  unused styles go and cells are renumbered. Printing in the pen looks
  nothing up.
- Bits for the halves of a wide glyph, text kept in the row, and
  protection.

Readers never see a stored cell: `Screen::cell`, `Window::cell`,
`Row::cell` and `Row::cells` return `CellRef`s, which carry the cluster's
text and the style's attributes. A host that stores screen contents uses
`Cells` (`row.cells().collect::<Cells>()`, `Cells::set`), which stores
cells as rows do, with each cell's attributes beside it in place of a
style, so it keeps and cuts text as a row does. A host makes a cell to
store with `CellRef::new` or `CellRef::wide_continuation`. `Cells` holds
no links.

A zero-width character after an empty cell joins it as a space and the
character; in the first column it joins the row above only if that row is
soft-wrapped, and is otherwise dropped. A glyph wider than the grid is
dropped, the cursor unmoved but for a wrap pending before it, which is
carried out, as xterm does.

## Storage, identity and windows

Each screen is a grid: rows of cells in slots, a ring giving each screen
row's slot, and history.

- **Scrolling** the whole screen turns the ring and moves no cells. Each
  row records how far it may differ from blank, so erasing, recycling and
  taking a row into history touch only that far.
- **History** holds at most `history_bytes` (0: none): 24 bytes a row, 8 a
  cell, and its text. A row scrolled off keeps its cells up to its last that
  is not a default blank, packed after the rows before it in blocks of 4,096
  cells (`history.rs`), with its text and links; past them it reads blank, at
  its original width. At its budget, the oldest rows go, and scrolling
  reuses the blocks they leave.
- **The alternate screen** has no history, and no cells until first shown;
  RIS and resizes keep an unmade one unmade.
- **Identity.** Every row has a `RowId`, kept through edits and scrolling
  while it is retained, and never reused: 64 bits counted by one do not
  run out. A row's version changes with each edit that
  changes its cells, links or soft wrap, and nothing else: erasing blanks,
  or rewriting a glyph as it was, keeps it; ICH and DCH always change it.
- **Limits.** A `Size` has no zero to refuse: `Size::new` refuses one.
  A screen of more than 64 Mi cells is refused (`Error::Capacity`); history
  keeps at most 1,048,576 rows and 64 Mi cells, whatever its budget; text
  budgets and link bounds cover the rest.
  Arithmetic is checked. After an error the terminal stays usable: input
  already applied may stay, and a partial scroll still forces a refresh.
- **Resize** builds the new grids first, so a failure changes nothing and
  peak memory can hold both. Tab stops stay; the scroll region and the
  left and right margins reset, as xterm resets them.

### Without `Feature::Reflow`

Resize is not paragraph reflow: rows keep their first cells and lose the
columns past the new width, and the screen's rows lose their soft wraps
(history keeps them). Rows move around the cursor so its line stays
visible: a shrink drops rows below the cursor first, then moves rows above
it into history (or drops them, on the alternate screen); a grow pulls rows
back from history, then adds blank rows below. Both cursors move with
their rows and are clamped, and keep a pending wrap, at any width, as xterm
keeps it.

With `reflow`, each line is laid out a run of cells at a time (as many as
fit before a row ends, a wide glyph moves or a spacer is skipped), in two
passes over the rows with no line gathered in memory, so a resize costs
its rows rather than their cells. A cell-at-a-time layout, used below two
columns, is the oracle (`grid::tests::laying_out_runs_is_laying_out_cells`).

### Windows and change marks

A `Row` is a retained row and its place among the rows retained, history
first: it borrows the screen, so a place can't be held past output that
moves it, and it steps to the rows around it (`above`, `below`, `up`,
`down`) without leaving them; a `RowId` finds it again later. A `Window`
is an immutable view of the screen's size: the screen (`Screen::window`),
or the rows from one up in history (`Row::window`), never short of rows;
reading it never changes where output lands. History rows are padded or
clipped to it unchanged, and a wide glyph cut by its edge reads as blank.
`Window::text` copies between inclusive endpoints: wide halves become
their first half, soft wraps join, a picture's cells read as blanks
(`CellRef::shown`), padding at hard line ends and past a history row's
width is not copied, and cell and byte limits are checked before anything
grows. `Row::copied` gives one row's glyphs as a copy takes them, each with
its column, for a copy of another shape, and `Row::ends_in_spacer` whether
a soft-wrapped row ends in the blank a wide glyph did not fit in, which a
copy leaves out (a blank erased in a colour is a cell, and stays).

Marks are not consumed, so readers share them. `changed_since` says
whether the screen may have changed (any processing counts);
`full_refresh_since` whether something structural did (scroll, resize,
reset, screen switch, history eviction); `dirty_rows_since` yields retained
rows newer than the mark, `dirty_live_rows_since` only the screen's, with
their places. A stale mark forces a refresh; it never hides an update.

## Bounds

What a program's bytes can make fux-vt hold or do, whatever they are and
however they are split, under what the host promises. Each line is a row
of the bounds suite (`tests/bounds.rs`), which feeds its reproducer below,
at and past the limit, at 1-byte, 16-byte and 4 KiB pieces, and checks
fux-vt's work counters and a counting allocator against it.

- **Pictures** (B01) keep at most 96 MiB of pixels per terminal
  (`PICTURE_BYTES`, `Parser::set_picture_bytes`), on both screens, the
  picture being drawn and its growth included. Past it, pictures no longer
  shown go first (covered, scrolled away, or on the other screen), then the
  oldest.
- **A sixel picture** (B03, B04) keeps at most 4096 by 2160 positions of
  its grid, 4096 a side, and is shown at its aspect, at most 4096 pixels a
  side; what is past that is not painted. Its room grows by doubling each
  side, so decoding paints and copies at most 4,096 pixels a byte.
- **A picture's declared size** (B05) costs nothing until it is drawn:
  the raster attributes' extent and the background are kept as numbers,
  and read as the picture is shown. Placing a picture fills its cells, at
  most about 2,300 cells a byte at a 10 by 20 cell and 7,600 at the
  smallest cell a host may give, 2 by 6.
- **Styles** (B12): a program can't choose colours that collide in the
  style table: its hash takes both of a style's words, keyed once a
  process from std's random keys.
- **The style table** (B11): its bytes past what the screen alone may
  need (twice its cells' styles) count in `history_bytes`, so the oldest
  history rows go as it grows, and when they all have, the styles no cell
  shows go.
- **`Prints`** (B10) keeps at most 4 bytes of text a screen cell (16 KiB
  at least) and 4,096 runs between takes. Past that, or after a resize,
  it records nothing more and says so (`Prints::lost`).
- **Finding a frame's end** (B19): `FrameScan::end` reads each byte once,
  however the frame comes split, a sequence left unfinished included.
- **A glyph** (B32) takes at most 2 columns: U+17D8, the one character
  unicode-width makes 3 wide, takes 1, as in xterm 411 and glibc
  (`char_width`, `str_width`, which fux's painter uses too).
- **An OSC string** (B20) keeps at most `OSC_PAYLOAD_LIMIT` (65,536)
  bytes, read to its end however long; its buffer holds at most 65,537,
  and 81,921 while it grows.
- **The work of one call** (P24, B02): `Parser::process_until_stop`
  stops after a sequence, as `Stop::Budget`, once 25 M weighted units of
  work (threats.md 4.4) are spent since its last such stop, over as many
  calls as that took. What is spent is the work one sequence can multiply:
  ED, rectangle fills, erases and copies, column edits, scrolls of more
  than a line, REP, the alignment pattern, checksums, history's searches
  and sweeps. With what the sequence that crosses it does, a call stays
  under the 50 M units a 4 KiB call may do at any size the host promises
  (4095 by 4096 in fux), as long as no one sequence does half of that; the
  host gives it the rest later. `process_with` doesn't stop: its work is
  bounded by its bytes alone.
- **A host's picture** (B07): `sixel::Picture::new` checks the size
  against the pixels, each side at most 4096, so `sixel::encode` works on
  the pixels a picture has.

## Verification

- **Tests** (`cargo test -p fux-vt`), named in each row above;
  `tests/properties.rs` and `tests/invariants.rs` check against small
  independent models and invariants over generated input;
  `tests/versions.rs` checks identities and versions. Expected values come
  from the references or xterm, never from fux-vt, and each test cites its
  source.
- **The `checks` feature**, off by default and for tests, fuzzing and
  mutation runs only: `Work`, deterministic counters of the work fux-vt
  does (cells written, rows walked, bytes scanned and the rest), which the
  bounds suite (`tests/bounds.rs`) holds to each bound's constant per
  input byte; and `Parser::check`, one invariant checker over the parser,
  both screens, their history, styles, links, pictures and prints, which
  says which checks ran and what each found. The invariant tests, the
  oracle (`diff/`) and the fuzz targets call it after every operation.
  Without the feature none of it is built.
- **Fuzzing**: three targets in
  [`fuzz/`](https://github.com/gold-silver-copper/fux/blob/master/fux-vt/fuzz/README.md),
  their corpora replayed nightly in CI.
- **Other terminals**:
  [`compare/`](https://github.com/gold-silver-copper/fux/blob/master/fux-vt/compare/README.md)
  runs fux-vt beside Ghostty, alacritty, libvterm, avt, wezterm, vt100,
  xterm.js, tmux and xterm on random cases, recordings of real programs and
  esctest, failing where they outvote it; a family a standard or xterm
  decides is checked by the standard's test or beside xterm
  (`run.sh verdicts`). Results:
  [`SCOREBOARD.md`](https://github.com/gold-silver-copper/fux/blob/master/fux-vt/compare/scoreboard/SCOREBOARD.md).

## Where the standards leave the choice

Here the standards are silent or leave the behaviour to the
implementation, and fux-vt does what xterm 411 does. Each item ends with a
case to replay (`fux-vt/compare/run.sh replay --engines all --size RxC
'BYTES'`).

- **Bold and faint can both be on.** ECMA-48 (8.3.117) names 1 "bold or
  increased intensity" and 2 "faint, decreased intensity or second
  colour", and leaves their combination to the implementation; 22 ends
  both (`1x3 '\e[1m\e[2mX'`).
- **REP after a grapheme cluster** repeats the character that took the
  cell. ECMA-48 (8.3.103) repeats the preceding graphic character and
  knows no cluster (`1x8 'e\u{301}\e[2b'`).
- **REP after a control function** repeats nothing until a character is
  printed; ECMA-48 leaves it undefined (`1x8 '-\e[2b\e[2b'`).

One departure from the standards remains, from mode 2027's spec
(`references/modern/mode_2027_grapheme_clusters.tex`, "Grapheme
Cluster"), whose emoji rules its join rule cannot meet:

- **A cluster takes its string width** (unicode-width) as characters join
  it, where the spec moves the cursor on a join only for VS16. Its
  "Emoji" section wants a flag (two regional indicators, each one column
  alone) and an emoji modifier sequence two columns, which no VS16 makes;
  and a conjunct, or a Prepend character with what follows, is drawn in
  its string width by a host terminal that measures so, as Ghostty draws
  conjuncts and flags (`1x6 '\u{600}1x'`: two columns, where the join
  rule would leave one).
