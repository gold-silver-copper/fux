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
let mut parser = fux_vt::Parser::new(size, 10_000)?; // and history lines
parser.process_with_replies(b"\x1b[1mhi\x1b[6n", |reply| send_to_program(reply))?;
let cell = parser.screen().cell(0, 0).unwrap();
assert_eq!((cell.contents(), cell.bold()), ("h", true));
```

This document is the contract: what each sequence does, the options, the
limits, and the departures from the references. The references are
ECMA-48, DEC STD 070, the VT520 manual, xterm's ctlseqs, Unicode 17 and the
specs of modern extensions
([`references/README.md`](https://github.com/gold-silver-copper/fux/blob/main/references/README.md)).
Where they are silent, or xterm departs from them, fux-vt does what xterm
does, since fux sets `TERM=xterm-256color`.

## Sequence matrix

CSI is ESC `[`. A missing or zero count is one unless noted. Sequences
count from one, the API from zero. "Margins" are the scroll region and,
with DECLRMM set, the left and right margins. Tests are in `tests/`
and `src/*/tests.rs`. Every mode SM, RM, DECSET and DECRST set is a
`Mode`, read with `Screen::mode(Mode::X)` as DECRQM reports it.

| Sequence | Behaviour | Tests |
| --- | --- | --- |
| Text, UTF-8 | Printed in ground state; widths from unicode-width 0.2; a character split across calls is completed. Invalid UTF-8 prints one U+FFFD, one column wide, per maximal subpart (Unicode 3.9), then rereads the byte that broke it off, and one per byte that starts no sequence (0xc0, 0xc1, 0xf5–0xff); overlong and surrogate forms are invalid. A lone continuation byte is Latin-1: 0x80–0x9f (C1) are ignored, 0xa0–0xbf print U+00A0–U+00BF (see Departures). UTF-8-encoded C1 characters are ignored. | `conformance::invalid_utf8_*` |
| Grapheme clusters | A character that continues the grapheme cluster of the cell just printed joins that cell (UAX #29, one character at a time, from tables `gen/` makes; nothing joins after a Prepend). A narrow cell whose cluster becomes two columns wide is widened over the cell under the cursor, except in the last column. Cursor moves and row edits end the cluster; SGR, modes and queries do not. A cluster keeps at most `CLUSTER_CAPACITY` (128) bytes and what its row's text budget allows; the rest is dropped, never split into another cell. `continues_cluster` tells a host what would join. | `unicode::tests`, `properties::printed_text_*`, `extended::*cluster*` |
| C0 controls | BS: back a column, stopping at the left margin unless already left of it (see CSI ? 45). HT: the next tab stop, else the last column (the right margin with DECLRMM). LF, VT, FF: down a line, scrolling at the bottom margin (between left and right margins only), CR too under LNM. CR: column 0, or the left margin if the cursor is at or right of it, or in origin mode. SO, SI: G1, G0 into GL. BEL and the rest: nothing visible. BS, LF, VT, FF and CR end a pending wrap; HT keeps it. | `semantics::text_controls_*` |
| ESC 7 / 8, CSI s / u | DECSC/SCOSC save and DECRC/SCORC restore the position with its pending wrap, origin mode, the pen, protection and the character sets. `CSI s` is DECSLRM while DECLRMM is set. In origin mode the restored column is at most the right margin. Each screen has its own saved position, clamped by a resize. | `extended::scosc_*` |
| ESC = / > | DECKPAM, DECKPNM: `Mode::ApplicationKeypad`, off; RIS and DECSTR reset it. State only. | `modes::decrqm_*` |
| ESC ( F, ESC ) F | SCS designates G0 or G1: `0` is DEC Special Graphics, any other set ASCII. With Special Graphics in GL, 0x5f–0x7e print a blank and `◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·`. G2, G3, the 96-character sets and other shifts are unhandled. RIS and DECSTR designate ASCII into both, G0 in GL. | `conformance::dec_special_graphics_*` |
| ESC D / E | IND: a line feed, LNM aside. NEL: a line feed, then to the column CR goes to. | `conformance::ind_and_nel_*` |
| ESC M | RI: up a line, scrolling down at the top margin (between left and right margins only). Ends a pending wrap. | `margins::index_scrolls_between_the_margins` |
| ESC c | RIS: both screens and history cleared; every mode, the pen, margins, tab stops, protection, links, keyboard stacks, cursor shape and palette back to their defaults. The dynamic and special colours and XTSAVE's saved modes stay. Row identities are never reused. | `semantics::regions_origin_and_reset_*` |
| CSI ! p | DECSTR, as xterm does it: shows the cursor; resets IRM, DECOM, DECCKM, DECKPAM, DECLRMM, reverse wraparound and synchronized output; turns DECAWM on (see Departures); resets both screens' margins, the pen, protection, the character sets, the saved cursor (home, default attributes) and the palette. Keeps the screen, cursor, pending wrap, tab stops, alternate screen, LNM, bracketed paste, focus reporting, mouse modes, cursor shape and keyboard state. | `conformance::a_soft_reset_*` |
| CSI 4 h / l | IRM, off: a printed glyph pushes what is at and after the cursor right, as ICH does, losing what passes the right margin; the row keeps its soft wrap. | `conformance::insert_mode_*` |
| CSI 20 h / l | LNM, off: LF, VT and FF return the carriage too; IND does not. RIS resets it, DECSTR keeps it. Other ANSI modes are unhandled. | `modes::lnm_*` |
| CSI A B C D E F G H d f \` a e | CUU and CPL stop at the top margin from at or below it, CUD and CNL at the bottom margin from at or above it, else at the screen's edge (xterm's `CursorUp`, `CursorDown`); CNL and CPL go to the column CR goes to. CUF and CUB stop at the right or left margin unless already past it. CUP, HVP, VPA and VPR count lines from the top margin and stay within the margins with DECOM set, and address the whole screen without (so VPR, unlike CUD, passes the bottom margin). CUP, HVP, CHA and HPA count columns from the left margin in origin mode, up to the right one. VPA and VPR keep the column (within the right margin in origin mode). HPR stops at the right margin in origin mode only. All end a pending wrap. | `conformance::line_and_column_addressing_*`, `margins::cursor_movements_*` |
| CSI b | REP: the last printed character, Pn more times, as if printed again. After any control, sequence or string (REP included) it does nothing until a character is printed. After a cluster it repeats the cluster's first character (see Departures). Copies past a screen and history's worth are skipped a line at a time, with the same result. | `conformance::rep_*`, `margins::rep_*` |
| CSI @ P X | ICH, DCH, ECH, within the row. ICH and DCH stop at the right margin; outside the left and right margins they do nothing and keep a pending wrap, otherwise they end it. A wide glyph an edit cuts is blanked whole. DCH ends the row's soft wrap; ICH keeps it. ECH leaves ISO-protected glyphs. Blanks take the pen's colours and no other attribute (`bce`). | `margins::edits_stop_*` |
| CSI L M S T | IL, DL, SU, SD within the margins, counts clamped to the region. With left and right margins only the cells between them move, rows keeping their identity, soft wrap and prompt mark, and a wide glyph across a margin is blanked first. IL and DL do nothing with the cursor outside the margins, else move it to the left margin. Rows scrolled off the top of the whole screen (no left and right margins) by SU, a line feed or a wrap go to history; other scrolls discard them. New lines take the pen's colours. | `margins::scrolling_is_bounded_*` |
| CSI J / K, CSI ? J / K | ED, EL, DECSED, DECSEL: 0 or none from the cursor, 1 to it, 2 all; others unhandled. Erased cells take the pen's colours (`bce`). Erasing through the last column ends the row's soft wrap; ED removes the prompt mark of each row it erases whole but the cursor's, whose mark stays even when ED erases all of it, as EL leaves it (a shell redrawing its prompt erases from it). DECSED and DECSEL leave protected glyphs, ED and EL only ISO-protected ones (see CSI " q). All end a pending wrap. | `conformance::blanks_brought_in_*` |
| CSI r | DECSTBM: top defaults to 1, bottom to the last line, and a bottom past the screen is the last line (see Departures). With top above bottom they are set and the cursor goes home, obeying DECOM; otherwise nothing changes. | `conformance::decstbm_*` |
| CSI ? 6 h / l | DECOM, off. Setting or resetting homes the cursor (to the top and left margins when set). | `conformance::line_and_column_addressing_obeys_origin_mode` |
| CSI ? 7 h / l | DECAWM, on. A glyph in the last column (the right margin's, unless the cursor is past it) leaves the cursor there with a wrap pending (`Screen::pending_wrap`). The next glyph first goes to the next line's start (the left margin) and marks the row soft-wrapped; so does a glyph too wide for the rest of the line, except where the line feed cannot move the cursor. Printing, BS, LF, VT, FF, CR, RI, cursor moves, ED, EL, ECH, ICH, DCH, IL, DL, DECSTBM, DECOM and RIS end the wrap; HT, SU, SD, SGR, modes and queries keep it. Off, a glyph overwrites the last cell and still sets the flag; resetting DECAWM keeps it (see Departures). | `conformance::a_pending_wrap_*`, `conformance::a_glyph_that_wraps_*` |
| CSI ? 1 / 25 / 2004 h / l | DECCKM (off), DECTCEM (on), bracketed paste (off). DECCKM is read by `Screen::encode_key`; bracketed paste is the host's to pass to `encode::paste`; DECTCEM is state only. | `semantics::alternate_mouse_modes_saved_cursor_and_replies` |
| CSI ? Pm s / r | XTSAVE saves each listed mode fux-vt keeps (1, 4, 5, 6, 7, 9, 25, 45, 47, 66, 67, 69, 1000, 1002, 1003, 1004, 1005, 1006, 1045, 1047, 1049, 2004, 2026; 2031 and 2048 with their options); XTRESTORE sets it as DECSET or DECRST would, reset if never saved. RIS and DECSTR keep what was saved. | `modes::xtsave_*` |
| CSI ? 45 / 1045 h / l | Reverse wraparound, off; RIS and DECSTR reset it. With DECAWM on, BS and CUB at the line's first column go on from the last column of the line above, which takes one of the count: for 45 a soft-wrapped row above, for 1045 any row above, or the bottom margin from the top margin (1045 wins). A pending wrap counts as one column past the last. The cursor stops at the first row. | `modes::reverse_wraparound_*` |
| CSI ? 69 h / l, CSI Pl ; Pr s | DECLRMM, off. Set, DECSLRM sets the left and right margins (Pl defaults to 1, Pr to the last column; a right margin past the screen is the last column, see Departures) if left is left of right, and homes the cursor, obeying DECOM; otherwise nothing changes. Resetting DECLRMM restores the screen's edges, and `CSI s` is SCOSC again. They bound what DEC STD 070 (5.4.3) lists, as each row here says. DECALN, DECSTR, RIS and a resize reset them. A wrap pending where the right margin was is carried out from there. Read with `Screen::left_right_margins`. | `margins::*` |
| CSI Pn ' } / ~ | DECIC, DECDC: Pn columns inserted or deleted at the cursor's column in each line of the scroll region, between the left and right margins, blanks in the pen's colours; nothing if the cursor is outside them. The cursor and a pending wrap stay; DECDC ends each line's soft wrap. | `margins::decic_and_decdc_*` |
| CSI Ps " q, ESC V / W | DECSCA 1 protects the glyphs printed after it, 0, 2 or none stops; SPA and EPA start and stop ISO protection. Protection is the glyph's: it moves with it through edits, scrolling and history, and REP repeats it; a glyph printed over it and every blank are unprotected. The last DECSCA or SPA decides which erases spare protected glyphs: DECSED and DECSEL (DEC), or also ED, EL and ECH (ISO). An ED or DECSED of the whole screen that finds none ends protection (see Departures). DECSC saves it; DECSTR and RIS end it. `CellRef` does not show it. | `protection::*` |
| CSI ? 4 / 5 / 8 / 66 / 67 h / l | Kept for DECRQM and XTSAVE, state only. DECSCLM (4), DECSCNM (5), DECBKM (67): off, reset by RIS, kept by DECSTR. DECBKM off means backarrow sends DEL, as `xterm-256color`'s `kbs` says. DECNKM (66) is the keypad mode of ESC = and ESC >. DECARM (8) is reported permanently reset. | `modes::decrqm_*` |
| CSI ? 1004 h / l | `Mode::FocusReporting`, off; RIS resets it. Read by `Screen::encode_focus`; DECRQM reports it. | `semantics::focus_and_cursor_shape`, `modes::decrqm_*` |
| CSI ? 2026 h / l | `Mode::SynchronizedOutput`, off: the program is drawing a frame. Holding the display is the host's: `Parser::process_until_frame` stops right after a sequence that sets the mode. RIS, DECSTR and any resize end it. | `opt_in::synchronized_output_*`, `opt_in::process_until_frame_*` |
| CSI ? 2048 h / l | With `Feature::InBandResize` only: `Mode::InBandResize`, off. Each set reports `CSI 48 ; rows ; cols ; 0 ; 0 t`; `Parser::resize_report` gives the report after a resize. RIS ends it. | `opt_in::in_band_resize_*` |
| CSI Ps SP q | DECSCUSR: `cursor_shape()` is Ps (0, the default, is the terminal's own); RIS resets it. State only. | `semantics::focus_and_cursor_shape` |
| CSI ? 47 h / l | Switches to or from the alternate screen without clearing it. The cursor, pending wrap, origin mode and margins carry across; each screen keeps its saved position. The alternate screen has no history, and no cells until first shown. | `conformance::switching_screens_*` |
| CSI ? 1049 h / l | Set: DECSC, switch, clear the alternate screen in the pen's colours (ending a pending wrap), cursor unmoved. Reset: switch back, DECRC. | `semantics::alternate_mouse_modes_saved_cursor_and_replies` |
| CSI ? 1047 / 1048 h / l | 1047: 47, clearing the alternate screen on leaving it. 1048: DECSC when set, DECRC when reset. | `conformance::modes_1047_and_1048` |
| CSI ? 9 / 1000 / 1002 / 1003 h / l | `mouse_protocol_mode()`; the latest set wins, and a reset clears only the mode in force. Read by `Screen::encode_mouse`. | `semantics::alternate_mouse_modes_saved_cursor_and_replies` |
| CSI ? 1005 / 1006 h / l | `mouse_protocol_encoding()`, by the same rules. Read by `Screen::encode_mouse`. | `semantics::alternate_mouse_modes_saved_cursor_and_replies` |
| CSI m | SGR. 0 reset; 1 bold, 2 dim, both possible (see Departures); 3 italic; 4 underline, 21 double, `4:0`–`4:5` none, single, double, curly, dotted, dashed (`4:` is `4:0`; larger numbers change nothing); 5 slow, 6 rapid blink, each replacing the other; 7 inverse; 8 hidden; 9 strikeout; 22–29 end them (22 bold and dim, 24 any underline). 30–37, 90–97, 40–47, 100–107 indexed colours; 39, 49, 59 defaults. 38, 48, 58 (foreground, background, underline colour): `5;n`, `2;r;g;b` (values the list lacks are 0; another kind takes only itself), `5:n`, `2:r:g:b`, `2:cs:r:g:b` (colour space ignored). An out-of-range colour is skipped; the rest applies. | `conformance::sgr_*`, `conformance::underline_styles_*` |
| CSI 5 n / 6 n / c | DSR 5 → `CSI 0 n`. DSR 6 → `CSI row ; col R`: the line counted from the top margin with DECOM set (the first line if above it), the column from the left margin in origin mode, a cursor waiting to wrap one past the last column (at it with `Options::identity`). DA (`CSI c`, `CSI 0 c`) → `CSI ? 1 ; 2 c` (`CSI ? 62 ; 22 c` with an identity). Other parameters and private forms are unhandled unless an option answers them. | `conformance::cursor_reports_*` |
| ESC # 8 | DECALN: the screen filled with `E` in default attributes; all four margins reset (DECLRMM stays), origin mode off, cursor home, no wrap pending; the pen keeps its colours and loses its attributes. Each row's soft wrap and prompt mark end; a row already all plain `E` keeps its version. Other `ESC #` are unhandled. | `modes::decaln_*` |
| ESC Z | DECID: answered as DA. | `modes::decid_*` |
| CSI Pi ; Pp ; Pt ; Pl ; Pb ; Pr * y | DECRQCRA, with `Feature::RectangleChecksums`; otherwise unhandled. | `opt_in::rectangle_checksums_*` |
| OSC, DCS, APC, PM, SOS | Consumed. OSC ends at BEL or ST (ESC `\`), the others at ST; 0x9c is a byte of a UTF-8 character, not ST. CAN and SUB cancel. Payloads are kept only for an option: OSC with `Events`, `Hyperlinks` or `Palette`; an OSC's first 16 bytes with `PromptMarks` alone; a DECRQSS request's first 4 bytes with `SettingReports`. | `conformance::a_control_string_*` |
| OSC 133 | With `Feature::PromptMarks` only. `A`, `N`: a fresh line (CR and IND unless in the first column), then the row is marked as a prompt's start (`Row::starts_prompt`). `P` with `k=i` or no `k`: the row marked, no fresh line. `L`: the fresh line alone. The rest change nothing. The mark moves with its row through scrolling, history and IL/DL, and through a reflow onto the row its first cell goes to; ED erasing the row whole, DECALN and RIS remove it; EL keeps it. Marks change no version. | `prompts::*` |
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
| `ExtendedReplies` | DECXCPR `CSI ? 6 n` → `CSI ? row ; col R`, as DSR 6; DA2 `CSI > c` → `CSI > 1 ; 10 ; 0 c`; DECRQM as `ModeReports`. | `opt_in::extended_replies_*` |
| `ModeReports` | DECRQM alone. `CSI ? Ps $ p` → `CSI ? Ps ; Pm $ y`: 1 set or 2 reset for 1, 4, 5, 6, 7, 9, 25, 45, 47, 66, 67, 69, 1000, 1002, 1003, 1004, 1005, 1006, 1045, 1047, 1049, 2004, 2026, 2031 (with `ColorSchemeUpdates`), 2048 (with `InBandResize`); 4 for 8; 0 otherwise. `CSI Ps $ p`: 1 or 2 for IRM (4) and LNM (20), else 0. | `opt_in::mode_reports_*` |
| `InBandResize` | Mode 2048 (see its row). Off: unrecognized, DECRQM 0. | `opt_in::in_band_resize_*` |
| `SizeReports` | `CSI 18 t` → `CSI 8 ; rows ; cols t`. `CSI 14 t` (pixels) stays unhandled. | `opt_in::size_reports_*` |
| `ColorSchemeUpdates` | Mode 2031 tracked: `Mode::ColorSchemeUpdates`, DECRQM; RIS ends it. The host sends the reports and answers `CSI ? 996 n`, which stays unhandled. Off: unrecognized, DECRQM 0. | `opt_in::colour_scheme_updates_*` |
| `KittyKeyboard` | A kitty keyboard flag stack per screen: `CSI > Ps u` pushes (a full stack of 32 drops its oldest), `CSI < Ps u` pops Ps (1), `CSI = Ps ; Pm u` sets (Pm 1 replace, 2 add, 3 remove); flags saturate at 255; `CSI ? u` → `CSI ? flags u`. `CSI > 4 ; Pv m` sets modifyOtherKeys (Pv 0: off); other `CSI > m` are unhandled. RIS clears it all. Read with `Screen::kitty_keyboard_flags`, `modify_other_keys`; the host encodes keys. Off: all unhandled. | `extended::kitty_*`, `extended::modify_other_keys_*` |
| `Reflow` | A resize re-wraps the primary screen and its history. Each logical line (soft-wrapped rows and the row ending it) loses its blank tail and is laid out at the new width without splitting wide glyphs: one that does not fit leaves a blank spacer, which later reflows and copies skip. The cursor and saved cursor stay on their characters. Surplus rows go in order: blank lines below the cursor (only as far as the screen's own lines are past the new height, so none is given up to bring a history row back above the cursor), the oldest lines into history, history past its limit; rows still below the screen once the cursor's row is at the top are dropped. A line's k-th row keeps the identity of its k-th row before; every row takes a new version. The margins reset. The alternate screen resizes without reflow. | `extended::*reflow*`, `extended::a_shrink_takes_no_history_back_onto_the_screen`, `properties::reflow_*` |
| `identity` | An `Identity { name, version }`. DA → `CSI ? 62 ; 22 c`; DA2 → `CSI > 1 ; Pv ; 0 c`, Pv = the version's parts, at most three, as digits in base 100: `1.2.3` is 10203, `1.2` is 102, `7` is 7 (a `-` or `+` suffix dropped); XTVERSION `CSI > q` → `DCS > \| name version ST`, unless name and version pass `Identity::MAX_LEN` (48) bytes; DSR 6 and DECXCPR report a cursor waiting to wrap at the last column. Without one, XTVERSION is unhandled. | `extended::*identity*` |
| `Hyperlinks` | OSC 8 `params ; URI`: glyphs printed while a link is open take it; an empty URI or RIS closes it. `Screen::hyperlink()` is the open link; `Row::link` and `Screen::link` give a cell's `Hyperlink`: `uri()`, `id()` (`id=` in params) and `key()`, equal for the cells of one OSC 8 without an id, or of one id and URI. Blank cells have none; a wide glyph's halves share one. Links follow their cells through scrolling, history, edits, resize and reflow. A URI over `URI_LIMIT` (2083 bytes), an id over `ID_LIMIT` (250), a byte outside printable ASCII, or an OSC over the payload limit opens nothing and closes the open link. Each screen holds at most 65,535 links and 4 MiB of URIs and ids (64 bytes more a link); when full, unused links are freed, then the oldest history rows lose theirs until half the bounds are free; the screen's rows keep theirs, and a link that still does not fit is dropped, with no retry for 256 links. A changed link changes the row's version. | `hyperlinks::*` |
| `SettingReports` | DECRQSS (`DCS $ q Pt ST`), as xterm 411 answers it: `DCS 1 $ r Pt ST` for `m` (the pen), ` q` (DECSCUSR) and `r` (DECSTBM); `DCS 0 $ r ST` for another request or one over 4 bytes. The pen is `0`, then 1, 4, 5, 7, 8, 2, 3, 9, 21, foreground, background (16 colours short, others `38:5:n` or `38:2::r:g:b`), with an underline style as `4:n` in 4's place, rapid blink 6 in 5's and the underline colour last (`58:…`): a curly underline alone is `0;4:3`, which neovim checks for. A cursor shape outside 1–6 is not answered, since vim reads an invalid answer as keys. | `opt_in::setting_reports_*` |
| `PromptMarks` | OSC 133 (see its row). | `prompts::*` |
| `Palette` | The colours a program sets, as xterm 411 keeps them. OSC 4 sets or queries palette entries (pairs of number and specification; 256–260 are the special colours, OSC 5's 0–4); OSC 10–19 the dynamic colours, each parameter the next; OSC 104, 105, 110–119 reset them. Specifications: `rgb:R/G/B` (1–4 hex digits a channel) and `#RGB` to `#RRRRGGGGBBBB`; names and other colour spaces are not read. Kept at 8 bits a channel; answered `OSC 4 ; n ; rgb:RRRR/GGGG/BBBB`, each byte twice, ended as asked. OSC 4 and 5 stop at the first bad number or specification; OSC 10–19 skip a bad one; OSC 104 and 105 stop at the first non-number; OSC 110–119 with a parameter do nothing; OSC 105 alone resets nothing (see Departures). Unset entries are answered with the host's colour for them, where it gave one (`Parser::set_host_palette`, entries 0 to 15, which no reset clears and which change nothing drawn), else with xterm's defaults (16 colours, 6×6×6 cube, grey ramp); unset dynamic colours become `ColorQuery` events with `Events`; unset special colours are not answered. RIS and DECSTR reset the palette only. Read with `Screen::palette_color`, `dynamic_color`, `colors_changed`; the host draws them. | `palette::*`, `palette::an_entry_not_set_is_answered_with_the_hosts_colour` |
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
| `Screen::encode_key(stroke, out)` | the key as `Screen::key_mode()` asks (DECCKM, the kitty flags, modifyOtherKeys): kitty protocol; xterm's `CSI 27 ; m ; k ~`; or legacy bytes, Ctrl mapping only what Xlib maps (`Ctrl-;` is `;`), Alt an ESC before them (Alt-Escape is ESC ESC). `keys::encode::key_bytes` takes any `KeyMode` |
| `Screen::encode_paste(text, out)` | the text, framed by `CSI 200 ~` and `CSI 201 ~` with bracketed paste (2004) set; inside the frame every end marker (`ESC [ 201 ~`, or with the C1 CSI U+009B) is removed, as often as removing one makes another. Otherwise as pasted. `keys::encode::paste` takes the mode given |
| `Screen::encode_focus(focused, out)` | `CSI I` or `CSI O` with focus reporting (1004) set, else nothing |
| `Screen::encode_mouse(event, out)` | a `keys::mouse::MouseEvent` as xterm reports it in the program's mode and encoding: X10 (9) presses of buttons 1 to 3 without modifiers; 1000 presses and releases (the wheel's turns as presses, never released); 1002 also motion with a button held; 1003 also motion with none; in the default encoding (nothing past position 223), UTF-8 (1005, Cb too, nothing past 2015) or SGR (1006, any position, a release naming its button). `keys::mouse::mouse_bytes` takes them given |

Each but the paste returns whether it sent anything. Motion within one
cell is the host's to drop.

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
  (`Screen::encode_*`, see [Keys](#keys)); nothing a program writes makes
  fux-vt send input.
- No graphics protocols: a host that draws images parses them itself.
- Only the primary screen has history, up to its limit. CSI 3 J (erase
  saved lines) is unhandled.
- Each compatibility choice is documented here and tested on its own,
  never hidden in a broad allowlist.

## Parser

A direct implementation of Paul Williams's DEC ANSI state machine
(<https://vt100.net/emu/dec_ansi_parser>), without a parser crate. UTF-8
replaces the 8-bit controls: raw C1 bytes are invalid UTF-8, and 0x9c in a
string is a byte of a character, not ST. Parameters saturate at
`u16::MAX`; at most 32 fields and two intermediates are kept, and more
make the sequence ignored. CAN and SUB cancel any sequence; ESC starts a
new one.

The common input skips the byte-at-a-time state machine, each fast path
doing exactly what it would:

- printable ASCII, found eight bytes at a time, is written into the row's
  cells a run at a time (a glyph at a time under IRM or Special Graphics);
- valid non-ASCII UTF-8 is decoded a run at a time;
- an escape sequence, or a CSI of parameters alone, is read in a loop of
  its own;
- an OSC payload is taken up to its terminator as one slice.

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
dropped, the cursor unmoved.

`Options`, `Error`, `Color`, `Blink`, `UnderlineStyle`,
`MouseProtocolMode`, `MouseProtocolEncoding`, `Event` and `Unhandled` are
`#[non_exhaustive]`: build options with `Options::new()` and its `const`
`with_*` methods, and give matches a fallback arm.

## Storage, identity and windows

Each screen is a grid: rows of cells in slots, a ring giving each screen
row's slot, and history.

- **Scrolling** the whole screen turns the ring and moves no cells. Each
  row records how far it may differ from blank, so erasing, recycling and
  taking a row into history touch only that far.
- **History** holds at most `history_lines` rows (0: none). A row scrolled
  off keeps its cells up to its last that is not a default blank, packed after the rows
  before it in blocks of 4,096 cells (`history.rs`), with 24 bytes of
  metadata, its text and links; past them it reads blank, at its original
  width. History grows as it fills; at its limit, scrolling reuses the
  blocks the oldest rows leave.
- **The alternate screen** has no history, and no cells until first shown;
  RIS and resizes keep an unmade one unmade.
- **Identity.** Every row has a `RowId`, kept through edits and scrolling
  while it is retained, and never reused; running out is
  `Error::IdentityExhausted`. A row's version changes with each edit that
  changes its cells, links or soft wrap, and nothing else: erasing blanks,
  or rewriting a glyph as it was, keeps it; ICH and DCH always change it.
- **Limits.** A `Size` has no zero to refuse: `Size::new` refuses one.
  A screen whose rows plus history limit pass 1,048,576 rows or 64 Mi cells
  is refused (`Error::Capacity`); text budgets and link bounds cover the rest.
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

A `Window` is an immutable view of at most the screen's size, any number of
rows up into history; reading it never changes where output lands. History
rows are padded or clipped to it unchanged, and a wide glyph cut by its
edge reads as blank. `Window::text` copies between inclusive endpoints:
wide halves become their first half, soft wraps join, padding at hard line
ends and past a history row's width is not copied, and cell and byte
limits are checked before anything grows.

Marks are not consumed, so readers share them. `changed_since` says
whether the screen may have changed (any processing counts);
`full_refresh_since` whether something structural did (scroll, resize,
reset, screen switch, history eviction); `dirty_rows_since` yields retained
rows newer than the mark, `dirty_live_rows_since` only the screen's, with
their places. A stale mark forces a refresh; it never hides an update.

## Verification

- **Tests** (`cargo test -p fux-vt`), named in each row above;
  `tests/properties.rs` and `tests/invariants.rs` check against small
  independent models and invariants over generated input;
  `tests/versions.rs` checks identities and versions. Expected values come
  from the references or xterm, never from fux-vt, and each test cites its
  source.
- **Fuzzing**: three targets in
  [`fuzz/`](https://github.com/gold-silver-copper/fux/blob/main/fux-vt/fuzz/README.md),
  their corpora replayed nightly in CI.
- **Other terminals**:
  [`compare/`](https://github.com/gold-silver-copper/fux/blob/main/fux-vt/compare/README.md)
  runs fux-vt beside Ghostty, alacritty, libvterm, avt, wezterm, vt100,
  xterm.js, tmux and xterm on random cases, recordings of real programs and
  esctest, failing where they outvote it; families that follow a recorded
  choice, these departures among them, are checked against xterm
  (`run.sh verdicts`). Results:
  [`SCOREBOARD.md`](https://github.com/gold-silver-copper/fux/blob/main/fux-vt/compare/scoreboard/SCOREBOARD.md).

## Departures from the references

Here xterm departs from the references, and fux-vt follows xterm, since
programs are tested against it. Each item ends with a case to replay
(`fux-vt/compare/run.sh replay --engines all --size RxC 'BYTES'`).

- **HT keeps a pending wrap,** though DEC STD 070 (Appendix D.6.1) lists HT
  among what clears it (`2x5 'abcde\tX'`).
- **Resetting DECAWM keeps a pending wrap,** though the appendix lists it
  too (`2x5 'abcde\e[?7l\e[?7hX'`).
- **With DECAWM off, a glyph in the last column still leaves a wrap
  pending,** carried out once DECAWM is set again; the appendix sets it only
  with autowrap on (`2x2 '\e[?7lca\e[?7hX'`).
- **Bold and faint can both be on,** where ECMA-48 (8.3.117) makes them one
  intensity (`1x3 '\e[1m\e[2mX'`).
- **DECSTR turns autowrap on,** where the VT520 manual (Table 5-6) and DEC
  STD 070 (p. 4-37) turn it off: `xterm-256color`'s `is2` and `rs2` (`tput
  init`, `tput reset`) send DECSTR and rely on `am` (`2x3
  '\e[?7l\e[!pabcd'`).
- **REP after a grapheme cluster** repeats the character that took the
  cell, where ECMA-48 (8.3.103) repeats the preceding graphic character,
  the mark (`1x8 'e\u{301}\e[2b'`).
- **REP after a control function** repeats nothing until a character is
  printed; ECMA-48 leaves it undefined (`1x8 '-\e[2b\e[2b'`).
- **A DECSTBM bottom margin past the screen** is the last line, where DEC
  STD 070 (5-25, note 3) ignores the sequence (`5x5 '\e[3;3H\e[2;99rX'`).
- **A DECSLRM right margin past the screen** is the last column, where DEC
  STD 070 (DECSLRM, note 3) ignores it
  (`3x12 '\e[?69h\e[3;99sabcdefghijklmn'`).
- **TBC 1, 2, 4 and 5 are ignored;** ECMA-48 (8.3.154) defines them for
  stops kept line by line, and there is one set
  (`1x20 '\e[3g\e[5G\eH\e[5g\r\tX'`).
- **CBT with a wrap pending** leaves the cursor and the wrap, so the next
  glyph lands where xterm puts it; ECMA-48 (8.3.7) moves back and DEC STD
  070 ends the wrap (`2x10 'abcdefghij\e[ZX'`).
- **HT and CHT with DECLRMM set stop at the right margin** wherever the
  cursor is, as esctest expects; DEC STD 070 (HT, note 1) goes on from
  right of the margin (`3x12 '\e[?69h\e[3;8s\e[2;10H\tX'`).
- **A lone UTF-8 continuation byte is Latin-1,** so Latin-1 text keeps its
  `£`, `°` and `©`; Unicode (3.9) recommends U+FFFD (`1x12 'price \xa35'`).
- **DECSED and DECSEL spare ISO-protected glyphs** too, "for backward
  compatibility"; DEC STD 070 (5.11.1.2) knows DECSCA's attribute alone
  (`2x10 'ab\eVcd\eWef\e[1;1H\e[?K'`).
- **An erase of the whole screen that finds no protected glyph ends
  protection** until the next DECSCA or SPA; DEC STD 070 keeps DECSCA's
  attribute (`2x10 '\e[1"q\e[?2Jab\e[1;1H\e[?2K'`).
- **OSC 105 with no parameter resets no special colour,** where ctlseqs
  says all; a listed one is reset. xterm 411 answers `OSC 5;0;?;1;?` with
  `3030` and `4040` after `OSC 5;0;#333;1;#444` and `OSC 105`.
