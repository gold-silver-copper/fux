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
| Printable ASCII / UTF-8 | Printable runs bypass state dispatch in ground state; unicode-width 0.2 supplies widths; incomplete UTF-8 survives calls. Invalid UTF-8 prints U+FFFD, one column wide, as xterm prints it: one for a sequence that breaks off (its maximal subpart, the Unicode Standard, 3.9), the byte that broke it off then read again, and one for a byte that starts no sequence (0xc0, 0xc1, 0xf5 to 0xff). A continuation byte alone is read as Latin-1, as xterm reads it: 0x80 to 0x9f are C1 controls, ignored, and 0xa0 to 0xbf print U+00A0 to U+00BF (see "Departures from the references"). xterm reads some overlong and surrogate forms otherwise, which no real output has; fux-vt does not follow it there. U+FFFD itself prints. UTF-8-encoded C1 characters are ignored | `text`, `chunking`, `conformance::invalid_utf8_*` |
| Grapheme clusters | A character that continues the extended grapheme cluster of the cell printed just before the cursor joins that cell: spacing marks, variation selectors, ZWJ sequences, flags, Indic conjuncts. Boundaries follow UAX #29 (Unicode 17) as a state machine fed one character at a time, from tables `gen/` generates, never from the text a cell stores; the one departure is that nothing joins after a Prepend character. A narrow cell whose cluster's string width becomes two is widened, the cell under the cursor becoming its second half; in the last column it stays narrow. Anything that moves the cursor or edits a row ends the cluster; SGR, modes and queries do not. A cluster is never split: past `Cell::CLUSTER_CAPACITY` (128) bytes, or past its row's text budget, the rest of it is dropped (a cell always holds a start of its cluster), and what follows lands where UAX #29 puts it. `continues_cluster` tells a host whether a character printed after a cluster would join it, by the same rules (fux places such a cell by a cursor move when it paints) | `unicode::tests` (all 766 conformance cases, every assigned code point against unicode-segmentation), `properties::printed_text_is_segmented_as_the_model_says`, `extended::grapheme_*`, `extended::long_clusters_*`, `extended::a_full_cluster_*`, fuzz operation `fd` |
| C0 | BS subtracts a column (and wraps back, with reverse wraparound: see CSI ? 45); HT advances to the next tab stop, or the last column if none is left (see ESC H and CSI g); LF/VT/FF advance/scroll without CR; CR goes to column zero; SO puts G1 in GL and SI G0 (see ESC ( / ESC )); BEL and other unhandled C0 have no visible effect. BS, LF, VT, FF and CR end a pending wrap; HT keeps it (see CSI ? 7) | `controls`, `conformance::a_pending_wrap_*` |
| ESC 7 / 8, CSI s / u | Save/restore position with its pending wrap (DEC STD 070, Appendix D.6.1), origin, drawing attributes, and the character sets with the shift between them, as xterm does; SCOSC/SCORC (`CSI s` / `CSI u`) share DECSC's slot; saved cursor is clamped after resize | `saved_cursor`, `extended::scosc_*`, `conformance::a_pending_wrap_is_saved_*` |
| ESC = / > | DECKPAM / DECKPNM set and clear `application_keypad()` (default off; reset clears it). State only: fux-vt encodes no keypad input | `opt_in::keypad_mode_is_tracked_and_reset` |
| ESC ( F / ESC ) F | SCS (VT520 manual, Tables 5-13 and 5-14; ECMA-35): designate G0 / G1, `0` DEC Special Graphics and any other set ASCII, as xterm reads them in UTF-8 (the national sets draw nothing different there). While GL holds Special Graphics, printable 0x5f to 0x7e print as xterm draws them: a blank, ◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·; other characters are unchanged. `TERM=xterm-256color`'s `smacs`/`rmacs` are `ESC ( 0` / `ESC ( B`. G2, G3 (`ESC *`, `ESC +`), the 96-character sets and single and locking shifts beyond SO/SI are unhandled. RIS and DECSTR designate ASCII into both, G0 in GL | `conformance::dec_special_graphics_draw_lines` |
| ESC D / E | IND (DEC STD 070; xterm's ctlseqs, as ECMA-48 withdrew it): a line feed, scrolling at the bottom margin; NEL (ECMA-48 8.3.86): the same, to the first column. Both end a pending wrap, as LF does | `conformance::ind_and_nel_*` |
| ESC H, CSI g, CSI I / Z | Tab stops: HTS (ECMA-48 8.3.62) sets one at the cursor's column; TBC (8.3.154) 0 or none clears the one there and 3 every one, the others ignored, as in xterm (see "Departures from the references"); CHT (8.3.10) and CBT (8.3.7) move Pn stops forward, stopping at the last column, or back, stopping at the first. One set for both screens, every eight columns at first and after RIS; DECSTR and resize keep them, as xterm does, the columns a resize adds having a reset's stops unless TBC 3 came since. CHT, like HT, keeps a pending wrap; CBT with one pending does nothing, as xterm (see "Departures from the references") | `conformance::tab_stops_*` |
| ESC M / c | Reverse index / full reset; reset clears both buffers and primary history, modes (IRM included), attributes and tab stops, but never restarts row identity allocation | `reset`, `scrolling` |
| CSI ! p | DECSTR, soft reset, as xterm does it: the VT520 manual's table (p. 5-150) and DEC STD 070's Soft Terminal Reset (p. 4-37) show the cursor and reset IRM, DECOM, DECCKM, DECKPAM, both screens' scroll regions, the pen, the character sets (ASCII, G0 in GL), and the saved cursor (home, normal attributes); DECAWM goes back to its default, on, as in xterm, where the VT520 table says no autowrap and DEC STD 070 off (see "Departures from the references"). The screen, the cursor and a pending wrap, the tab stops, the alternate screen, bracketed paste, focus reporting, mouse modes and kitty flags stay | `conformance::a_soft_reset_*` |
| CSI 4 h/l | IRM (ECMA-48 7.2.10; `xterm-256color`'s `smir`/`rmir`), off by default: with it set, a glyph printed moves what is at and after the cursor right, as ICH does, what passes the last column being lost, and the row keeps its soft wrap. Other ANSI modes are unhandled | `insert-mode`, `conformance::insert_mode_*` |
| CSI A B C D E F G H d f \` a e | Relative up/down/right/left (CUU CUD CUF CUB), next/previous line (CNL CPL), horizontal absolute (CHA, and HPA, CSI \`), cursor position (CUP, and HVP, CSI f), vertical absolute (VPA), vertical and horizontal relative (VPR, CSI e; HPR, CSI a). CUU, CUD, CNL and CPL stop at the margins when the cursor is between them, and at the screen's edges otherwise. CUP, HVP, VPA and VPR address lines as DEC STD 070's DECOM says: from the top margin and never past the margins with DECOM set, anywhere on the screen with it reset, so VPR passes the bottom margin there, unlike CUD (VT520 manual, 5-208; xterm). HPA and HPR, with no left or right margins, are CHA and CUF | `cursor`, `extended::hvp_*`, `conformance::line_and_column_addressing_*` |
| CSI b | REP (ECMA-48 8.3.103): the preceding graphic character printed Pn more times (0 or none: once), wrapping, scrolling and taking the character set as printing it again would. ECMA-48 leaves REP undefined after a control function; as in xterm, there is then nothing to repeat: after any control, sequence or string, REP's own included, REP does nothing until a character is printed. After a grapheme cluster it repeats the character that took the cell, without the marks joined to it, as xterm does (see "Departures from the references"). Copies past those that fill the screen and its retained history are skipped a whole row's worth at a time, which leaves everything as printing them would, so one REP prints at most that many | `conformance::rep_*` |
| CSI @ P X | Insert/delete/erase characters, bounded to the row; no orphan wide halves; the blanks inserted, brought in by a deletion or erased take the pen's foreground and background colours and no other attribute, as xterm's do (`bce`, background colour erase, which `xterm-256color` advertises) | `editing`, `wide_edits`, `conformance::blanks_brought_in_*` |
| CSI L M S T | Insert/delete lines; scroll up/down within margins; counts bounded to affected region; partial-region operations do not add history. IL and DL are ignored outside the margins, and leave the cursor in the first column (DEC STD 070, IL and DL, note 2; xterm). The lines brought in, by these and by LF and RI, take the pen's colours (`bce`, as for CSI @ P X) | `scrolling`, `conformance::blanks_brought_in_*` |
| CSI J / K, CSI ? J / K | Erase display/line: absent/0 forward, 1 backward, 2 all; erased cells take the pen's colours (`bce`, as for CSI @ P X); no protected cells | `erase`, `conformance::blanks_brought_in_*` |
| CSI r | DECSTBM (DEC STD 070, 5-25): top/bottom default to 1/height, a bottom past the screen being its last line, as xterm reads it (see "Departures from the references"); with the top above the bottom the margins are set and the cursor goes home, obeying DECOM (the first line with it reset, the top margin with it set); otherwise the sequence is ignored, the margins, the cursor and a pending wrap staying as they are | `margins`, `conformance::decstbm_*` |
| CSI ? 6 h/l | Origin mode per buffer; homes on change; defaults off | `margins` |
| CSI ? 7 h/l | Autowrap, defaults on. A glyph printed in the last column leaves the cursor on it with a wrap pending, DEC STD 070's Last Column Flag (Appendix D.6.1; `Screen::pending_wrap`); the next glyph first moves to the start of the next line, marking the row soft-wrapped. A glyph too wide for what is left of the line wraps the same way, and marks its row though the last column stays blank; only on the last row below the scroll region, where the glyph overwrites its own row instead, is nothing marked. `Screen::cursor_position` is therefore always on the screen. Printing, BS, LF, VT, FF, CR, RI, every cursor movement, CUP, ED, EL, ECH, ICH, DCH, IL, DL and DECSTBM (where carried out), DECOM and RIS end it, as the appendix lists; HT keeps it, as xterm and the engines in `compare/` do though the appendix lists HT; SU, SD, SGR, modes and queries keep it. With autowrap off a glyph overwrites the last available cell instead; the flag is still set, and resetting DECAWM keeps it, as in xterm, so a glyph printed after DECAWM is set again wraps | `autowrap`, `conformance::a_pending_wrap_*`, `conformance::a_glyph_that_wraps_*` |
| CSI ? 1 / 25 / 2004 h/l | Application cursor (off), cursor visibility (on), bracketed paste (off) | `modes` |
| CSI ? 45 / 1045 h/l | Reverse wraparound (ctlseqs: XTREVWRAP, 45, and XTREVWRAP2, 1045; off at first, and after DECSTR and RIS, as in xterm 411), with DECAWM: BS and CUB at the first column go on from the last column of the line before, which takes one of the count, as xterm 411 moves (`CursorBack`, asked under Xvfb). With 45 the line before is only a soft-wrapped row above, wherever the margins are, so a line's text is gone back over and no further (xterm since patch 383); with 1045 it is any row above, and from the top margin the bottom margin; 1045 wins when both are set. A cursor waiting to wrap counts as one past the last column, so the first column back is the one it waits in. Where xterm 411 crashes (45 past a soft-wrapped first row) or puts the cursor above the screen (1045 from above the top margin), the cursor stops at the first row, as Ghostty's does; Ghostty also stops 45 at the top margin, where xterm, and fux-vt, cross it. DECRQM reports both | `modes::reverse_wraparound_*` |
| CSI ? 4 / 5 / 8 / 66 / 67 h/l | Kept as xterm 411 keeps them, for DECRQM (and XTSAVE): DECSCLM (4, smooth scroll), DECSCNM (5, reverse video) and DECBKM (67, the backarrow key sends BS), off at first and after RIS, kept by DECSTR, as in xterm 411; DECNKM (66) is the keypad mode ESC = and ESC > set (`application_keypad()`), as in xterm (ctlseqs). State only: fux-vt scrolls at once, draws nothing and encodes no keys. DECBKM starts reset, the backarrow sending DEL, as fux sends it and `xterm-256color`'s `kbs` says (xterm 411 starts it set, from its `backarrowKey` resource). DECARM (8) is not kept: DECRQM reports it permanently reset (4), as xterm 411 does | `modes::decrqm_reports_the_modes_xterm_keeps` |
| CSI ? 1004 h/l | Focus reporting, `focus_reporting()` (off; reset clears it). State only: fux-vt sends no focus reports, and DECRQM does not report it | `focus_and_cursor_shape` |
| CSI ? 2026 h/l | Synchronized output (`references/modern/mode_2026_synchronized_output.md`), `synchronized_output()` (off): set while a program draws a frame it wants shown whole. Holding the display is the host's to do: `Parser::process_until_frame` stops right after a sequence that sets the mode and says how far it read, so a host can hold the frame's bytes from there (fux does; see its README). RIS, DECSTR and any resize end it. The spec is silent on both, and the engines in `compare/` that can tell split: a resize ends it in Ghostty and not xterm.js; DECSTR ends it in xterm.js and not Ghostty, and fux-vt ends it so that `tput init` and `tput reset` never leave a frame waiting. DECRQM reports it | `opt_in::synchronized_output_*`, `opt_in::process_until_frame_*`, `opt_in::mode_reports_*` |
| CSI ? 2048 h/l | In-band resize (`references/modern/mode_2048_in_band_resize.md`), with `Options::in_band_resize` (see "Opt-in outputs"): `in_band_resize()` (off). Setting it reports the size at once as `CSI 48 ; rows ; cols ; 0 ; 0 t`, every time it is set, as the spec requires; pixel sizes are 0, which the spec allows a terminal that does not know them. `Parser::resize_report` gives the host the report for the new size, to send once the program's terminal has it. RIS ends it. Without the option the mode is not recognized, and DECRQM says 0 | `opt_in::in_band_resize_reports_the_size` |
| CSI Ps SP q | DECSCUSR: `cursor_shape()` records Ps as given, absent or 0 being the terminal's default (reset clears it). State only: fux-vt draws no cursor | `focus_and_cursor_shape` |
| CSI ? 47 h/l | Switch to/from separate alternate buffer without clearing it; primary history retained; alternate has no history. As in xterm, the cursor, with its pending wrap, origin mode and the margins belong to the terminal, not to either screen: switching leaves them as they are. Each screen keeps its own saved cursor (DECSC), as each of xterm's does | `alternate`, `conformance::switching_screens_*` |
| CSI ? 1049 h/l | xterm's ctlseqs: save the cursor as DECSC does, switch to the alternate screen and clear it, the cursor staying where it is; leave it, then restore the cursor as DECRC does. The clear takes the pen's colours (`bce`) and ends a pending wrap, as ED's does | `alternate`, `conformance::switching_screens_*` |
| CSI ? 1047 / 1048 h/l | xterm's ctlseqs: 1047 enters the alternate screen as 47 does and, on leaving it, clears it first, as 1049 clears it; 1048 saves and restores the cursor as DECSC and DECRC do | `conformance::modes_1047_and_1048` |
| CSI ? 9 / 1000 / 1002 / 1003 h/l | X10 press / press-release / button-motion / any-motion; latest set wins; reset only clears matching active mode | `mouse` |
| CSI ? 1005 / 1006 h/l | UTF-8 / SGR encoding state, latest set wins and matching reset restores legacy; fux still emits legacy bytes for non-SGR, not a new UTF-8 encoder | `mouse` |
| CSI m | 0/reset; 1/bold and 2/dim, kept apart (both can be on, as in xterm; 22 ends both); 3/italic, 4/underline, 21/doubly underlined, 5/slow and 6/rapid blink (mutually exclusive), 7/inverse, 8/hidden, 9/strikeout; resets 22/23/24/25/27/28/29 (24 ends an underline of any style); 30–37/40–47, 90–97/100–107; 39/49/59 defaults; 38/48/58 (foreground, background, underline colour): `5;n` and `2;r;g;b` read as xterm reads them (values the list ends before are 0; another kind takes only itself), and ITU-T T.416's (13.1.8) colon forms `5:n`, `2:r:g:b` and `2:space:r:g:b`, the colour space ignored, as in xterm; a colour out of range is skipped and the rest of the SGR applies. `4:0` to `4:5` are kitty's underline styles (`references/modern/kitty_underlines.html`), none, single, double, curly, dotted and dashed, each replacing the one before, as 4 (single) and 21 (double) do: `Attributes::underline_style` and `CellRef::underline_style` read the style, `underline()` whether there is one; `4:` is `4:0`, and a number past 5 changes nothing. Ghostty, alacritty, libvterm, wezterm, xterm.js and tmux read them; xterm, avt and the vt100 crate ignore them, and fux-vt follows the spec | `sgr`, `extended::blink_*`, `conformance::sgr_*`, `conformance::an_invalid_sgr_*`, `conformance::underline_styles_*`, `conformance::sgr_21_*`, `cell::tests::underline_styles_*` |
| CSI 5n / 6n / 0c | Replies `ESC[0n`, a one-based cursor report, `ESC[?1;2c`. The report's line is counted from the screen's top with DECOM reset, and from the top margin with it set (DEC STD 070, CPR, p. 5-53; xterm), the first line for a cursor above the margin, which DECRC can leave; the column is the screen's, as fux-vt has no left margin; missing DA parameter is zero; preserve baseline reply coordinates, reporting a cursor waiting to wrap one past the last column; no replies for intermediates/private variants. `Options::identity` changes the DA answer and the column a cursor waiting to wrap is reported at (see "Opt-in outputs") | `replies`, `conformance::cursor_reports_*` |
| CSI Pi ; Pp ; Pt ; Pl ; Pb ; Pr * y | DECRQCRA, the checksum of a rectangle, answered with `Options::rectangle_checksums` (see "Opt-in outputs"); without it, unhandled | `opt_in::rectangle_checksums_sum_the_cells` |
| OSC / DCS / APC / PM / SOS | Consume without storing payload or drawing it (the colours, with `Options::palette`, are kept: see "Opt-in outputs"); OSC accepts BEL or ST, others ST, as ESC `\` (ECMA-48 8.3.143): the byte 0x9c, 8-bit ST, is part of a UTF-8 character in a payload (`\u{271c}` is e2 9c 9c) and ends nothing, as in xterm; cancellation/recovery follows parser state rules. With `Options::events`, `Options::hyperlinks` or `Options::palette` only, OSC payloads are buffered (see "Opt-in outputs"); with `Options::prompt_marks` alone, an OSC string's first 16 bytes, which tell a prompt mark (OSC 133); with none of them, nothing. A DCS string is never kept, but with `Options::setting_reports` a DECRQSS's request, at most 4 bytes, to answer it | `ignored_strings`, `opt_in::without_the_option_decrqss_is_ignored` |
| OSC 133 | Semantic prompts (`references/modern/osc133_semantic_prompts.md`, and FinalTerm's as `osc133_iterm2_escape_codes.html` documents them), with `Options::prompt_marks` (off by default: OSC 133 is then ignored): `A` does a fresh line (CR and IND, unless the cursor is in the first column, as the proposal says and Ghostty does) and marks the cursor's row as where a prompt starts, `Row::starts_prompt` / `Screen::starts_prompt`; `N` does the same (it may also end the previous command, of which fux-vt keeps no record); `P`, an explicit start of prompt, marks the cursor's row without a fresh line when its kind is primary (`k=i`, or no `k`), as Ghostty does (its bash integration sends it in `A`'s place under ble.sh), and right-side and continuation prompts (`k=r`, `k=c`, `k=s`) mark nothing; `L` is the fresh line alone; `B`, `C`, `D`, `I` and the other options (`aid`, `cl`) change nothing. A mark is the row's, one bit in its metadata's padding: it goes into history and through IL/DL with its row, and leaves with it; ED erasing the row whole (as Ghostty clears it) and RIS remove it, and EL, and ED's part of the cursor's row, leave it, as a shell erases from its prompt to redraw it. A resize keeps it on its row; a reflow puts it on the row the marked row's first cell goes to, so a mark on a line's second row goes, once a reflow has joined the rows, to the line's first. Marks are no change to a row's version | `prompts::without_the_option_prompt_marks_are_ignored`, `prompts::*` |
| Other sequences | Safely parse and ignore; recognized C0 inside CSI still executes; no leakage of ignored string payloads. A complete CSI or escape sequence fux-vt does not implement is given to `Sink::unhandled` (default: discarded): unknown final bytes, intermediates or private markers, ED/EL modes above 2, DSR other than 5/6. Unknown DEC private mode numbers are consumed quietly, and sequences cut short by parser bounds are not reported | `ignored_sequences`, `parser_bounds`, `extended::sequences_*` |

## Opt-in outputs

`Parser::new` uses `Options::default()`: everything below is off, and the
behaviour is exactly the table above. `Parser::with_options` enables each
independently; `Parser::process_with` delivers to a `Sink` whose
`reply`/`event`/`unhandled` methods default to discarding.

| Option | Contract | Tests |
| --- | --- | --- |
| `events` | OSC 0 → `IconName` then `Title`; OSC 1 → `IconName`; OSC 2 → `Title`; OSC 52 `Pc;Pd` → `Clipboard { selection, data }` unless `Pd` is `?` (a query); OSC 10 to 19 → a `ColorQuery { number, bel }` for each `?` among its parameters, each the next dynamic colour from the one named (`OSC 10 ; ? ; ?` asks 10 and 11; ctlseqs), `bel` saying whether BEL rather than ST ended it, for the host's answer; other OSC numbers, and setting a colour, produce nothing. BEL executed in ground, escape or CSI state → `Bell` (BEL terminating an OSC is not a bell). Payloads are raw bytes. An OSC string is terminated by BEL or by ESC (the start of ST); CAN/SUB cancel it without an event. At most `OSC_PAYLOAD_LIMIT` (64 KiB) payload bytes are buffered per string; a longer string is consumed with no event, nothing past the limit buffered (the allocation, at most that limit, is kept for the next string). Events are identical under any chunking | `opt_in::events_*`, `opt_in::colour_queries_are_events`, `opt_in::osc_payloads_are_bounded_and_cancellable`, fuzz header bit `0x10` |
| `extended_replies` | DECXCPR `CSI ? 6 n` → `CSI ? row ; col R` (same coordinates as DSR 6n, including a cursor waiting to wrap); secondary DA `CSI > c` / `CSI > 0 c` → `CSI > 1 ; 10 ; 0 c`; DECRQM `CSI ? Ps $ p` → `CSI ? Ps ; Pm $ y` with `Pm` 1 set / 2 reset for modes 1, 4, 5, 6, 7, 25, 45, 47, 66, 67, 1045, 1047, 1049, 9, 1000, 1002, 1003, 1005, 1006, 2004, 2026, 2031 (with `color_scheme_updates`), 2048 (with `in_band_resize`), 4 (permanently reset) for DECARM (8), as xterm 411 answers, and 0 (not recognized) otherwise; ANSI DECRQM `CSI Ps $ p` → `CSI Ps ; Pm $ y`, `Pm` 1 or 2 for IRM (4) and 0 otherwise. Primary DA and DSR 5n/6n are unchanged | `opt_in::extended_replies_*`, fuzz header bit `0x20` |
| `mode_reports` | DECRQM alone, answered as with `extended_replies` (`CSI ? Ps $ p` and `CSI Ps $ p`), without DA2 or DECXCPR: how programs learn which modes the terminal knows, synchronized output among them, without what the other replies say about the terminal | `opt_in::mode_reports_answer_decrqm_alone` |
| `in_band_resize` | Mode 2048 recognized and tracked; setting it replies `CSI 48 ; rows ; cols ; 0 ; 0 t`; `Parser::resize_report` gives the same for the size after a resize while it is set. Off: not recognized, DECRQM 0, no reports | `opt_in::in_band_resize_reports_the_size` |
| `size_reports` | xterm's text-area size query (ctlseqs, window manipulation): `CSI 18 t` → `CSI 8 ; rows ; cols t`, the screen's size in characters. `CSI 14 t` (pixels) stays unhandled: fux-vt knows no pixels. Off: `CSI 18 t` is unhandled | `opt_in::size_reports_answer_the_text_area_in_characters` |
| `color_scheme_updates` | Mode 2031, colour-scheme change reports (`references/modern/mode_2031_color_scheme_updates.md`), recognized and tracked: `Screen::color_scheme_updates`, DECRQM 1 or 2; RIS ends it. State only: the host knows the colours, so it sends the reports (`CSI ? 997 ; 1 n` dark, `; 2 n` light) and answers `CSI ? 996 n`, which stays unhandled. Off: not recognized, DECRQM 0 | `opt_in::colour_scheme_updates_are_tracked` |
| `kitty_keyboard` | Tracks one kitty keyboard flag stack per screen (primary, alternate): `CSI > Ps u` pushes (a full stack of 32 drops its oldest), `CSI < Ps u` pops Ps (default 1), `CSI = Ps ; Pm u` sets (Pm 1 replace, 2 add, 3 remove); flags saturate at 255; `CSI ? u` answers `CSI ? flags u`. `CSI > 4 ; Pv m` sets modifyOtherKeys (Pv 0 or absent: off); other `CSI > m` resources are unhandled. Full reset clears all of it. Read with `Screen::kitty_keyboard_flags` / `modify_other_keys`. State only: the host encodes keys. Off, every one of these sequences is unhandled | `extended::kitty_*`, `extended::modify_other_keys_*`, `extended::without_the_option_keyboard_*`, `properties::kitty_keyboard_flags_agree_with_two_plain_stacks`, fuzz rows-byte bit `0x20` |
| `reflow` | Resize re-wraps the primary screen and its history at the new width: each logical line (soft-wrapped rows and the row ending it) loses its blank tail and is laid out again without splitting wide glyphs (a glyph that does not fit leaves a blank spacer and starts the next row; a later reflow, and a copy, skip that spacer, so reflowing narrower and back gives the text it started with). The cursor stays on its character, or as far past the text as it was, at most waiting to wrap, and so does the saved cursor (DECSC's, and 1049's), so DECRC finds its character too. Surplus rows go blank lines below the cursor first, then the oldest lines into history, then history past its limit; rows below the screen once the cursor's row reaches its top are dropped. A reflowed line's k-th row keeps the identity of its k-th row before, if it had one; every row takes a new version and the resize forces a full refresh. The scroll region is reset. The alternate screen resizes without reflow. Two passes over the rows, no line gathered in memory; each line is laid out a run of cells at a time, as many as fit before a row ends, a wide glyph moves or a spacer is skipped, so a resize costs the rows, not their cells; transactional like every resize | `extended::*reflow*`, `properties::reflow_narrower_and_back_keeps_every_line`, `grid::tests::laying_out_runs_is_laying_out_cells`, fuzz rows-byte bit `0x10` |
| `identity` | An `Identity { name, version }`. Primary DA answers `CSI ? 62 ; 22 c`; secondary DA answers `CSI > 1 ; Pv ; 0 c`, Pv the version as `major*10000 + minor*100 + patch`, with or without `extended_replies`; XTVERSION (`CSI > q`, `CSI > 0 q`) answers `DCS > \| name version ST`, unless name and version exceed `Identity::MAX_LEN` (48) bytes, when it is unhandled; DSR 6n and DECXCPR report a cursor waiting to wrap at the last column, as xterm does. Without one, XTVERSION is unhandled | `extended::an_identity_*`, `extended::identity_*`, fuzz rows-byte bit `0x40` |
| `hyperlinks` | OSC 8 (`references/modern/osc8_hyperlinks.md`): `OSC 8 ; params ; URI ST` opens a link, which every glyph printed until the next OSC 8 takes; an empty URI closes it, as RIS does (SGR 0, DECSTR and DECSC/DECRC leave it). `Screen::hyperlink()` is the link open. Read a cell's with `Row::link(col)` or `Screen::link(row, col)`: a `Hyperlink` with `uri()`, the `id` the program gave in `params` (`id=` among `:`-separated pairs; empty is none) and `key()`, which tells links apart: the cells of one OSC 8 without an `id` are one link, and cells of the same `id` and URI one link wherever and whenever printed, as the spec's "Hover underlining" groups them (VTE's way for links without an id). A blank cell (erased, inserted or scrolled in) has no link; a wide glyph's second half has its first half's; a mark joined to a blank cell gives it a space and the open link. Links go with their cells through scrolling into history, IL/DL, ICH/DCH and IRM, resize and reflow, and go with the rows that are dropped. Limits, from the spec's "Length limits": a URI over `URI_LIMIT` (2083) bytes, an `id` over `ID_LIMIT` (250), or either with a byte outside printable ASCII (undefined in the spec, and unsafe to pass on to another terminal), or an OSC string past `OSC_PAYLOAD_LIMIT`, opens nothing and closes the link open. Each screen holds at most 65,535 links and 4 MiB of URIs and ids (64 bytes more each); when a new link does not fit, the links no row has are freed, then the oldest history rows lose theirs until the links rows have are at most half the bounds; the screen's rows never do, and a link that still does not fit is not kept (its cells have none), with no further try for the next 256 links. Storage: a row a link was printed in has a `u16` a cell beside its cells (`link.rs`), and the screen counts the cells of each link, so freeing reads no cell (an erased cell keeps its link's count until written over, or its row is erased whole or leaves); other rows cost nothing more, their flag sitting in the row metadata's padding, and output in which no link was ever opened costs one test a run of text. A changed link is a changed cell for the row's version. Off, OSC 8 is ignored | `hyperlinks::*`, `link::tests`, fuzz history-byte bit `0x40` |
| `setting_reports` | DECRQSS (`DCS $ q Pt ST`; xterm's ctlseqs, "Device-Control functions"; DECRPSS in the VT510 manual) answered as xterm answers it (`do_dcs`, xterm 411 misc.c): `DCS 1 $ r Pt ST`, `Pt` the control function that sets the setting now, for `m` (the pen), ` q` (DECSCUSR) and `r` (DECSTBM, one-based); `DCS 0 $ r ST` for any other request, or one of more than 4 bytes. The pen is in xterm's form and order (`xtermFormatSGR`): `0`, then 1, 4, 5, 7, 8, 2, 3, 9 and 21 as set, then the foreground and background, 16 colours in their short forms (30–37, 90–97, 40–47, 100–107) and the rest as `38:5:n` / `38:2::r:g:b`; what xterm has not, in xterm's forms: an underline style as `4:n` in the underline's place (double is 21, single 4), rapid blink as 6 in blink's, the underline colour last as `58:5:n` / `58:2::r:g:b`. A curly underline alone is `0;4:3`, which neovim asks after to learn the terminal keeps styles (neovim 0.12.5, `tui_query_extended_underline` and `handle_term_response`). A cursor shape left to the terminal (DECSCUSR 0, the default) is not answered: fux-vt does not know it, and vim, which asks, reads an invalid answer as keys. The request is told from the string's parameters (none), intermediate (`$`) and final (`q`), and its first 4 bytes kept; CAN and SUB cancel it unanswered. Off: DECRQSS is consumed like any DCS, unanswered | `opt_in::setting_reports_*`, `opt_in::without_the_option_decrqss_is_ignored`, fuzz columns-byte bit `0x80` |
| `prompt_marks` | OSC 133 (semantic prompts; see its row above): `A` marks the row a prompt starts on (`Row::starts_prompt`) after a fresh line, `L` is the fresh line alone, the rest change nothing. Each OSC string's first 16 bytes are kept to tell one, with nothing else retained unless `events` or `hyperlinks` is on. Off: OSC 133 ignored | `prompts::*`, fuzz header bit `0x80` of the history byte |
| `palette` | The colours a program sets, as xterm keeps them (ctlseqs, "Operating System Commands"; `palette.rs`): OSC 4 sets and queries the 256-colour palette, pairs of a number and a specification; OSC 5, and OSC 4 at 256 to 260, the special colours (bold, underline, blink, reverse, italic); OSC 10 to 19 the dynamic colours, each parameter the next colour from the one named; OSC 104, 105 and 110 to 119 reset them. A specification is read as XParseColor reads `rgb:R/G/B` (1 to 4 hex digits a channel, scaled) and `#RGB` to `#RRRRGGGGBBBB` (the digits the high bits), case aside; a colour is kept at 8 bits a channel, the high byte, and answered `OSC 4 ; n ; rgb:RRRR/GGGG/BBBB` with the byte twice, in the form and with the terminator of the query, as xterm 411 keeps and answers them (`#fff` is `f0f0`). Colour names and `rgbi:`, `CIEXYZ:` and the other device-independent spaces are not read. Lists are read as xterm reads them: OSC 4 and 5 stop at the first pair whose number (read by `atoi`) is out of range or whose specification cannot be read; OSC 10 to 19 skip such a specification and go on; OSC 104 and 105 stop at the first item that is no number; OSC 110 to 119 with a parameter do nothing. Answers to colours not set: a palette entry gets xterm's default (its 16 compiled-in colours, the 6×6×6 cube and the grey ramp, which Ghostty shares; its 16 are its own theme's), which `xterm-256color` names; a dynamic colour is asked of the host, an `Event::ColorQuery` with `events`, as without the option (fux answers 10 and 11 with its client terminal's); a special colour is not answered (xterm's default is its foreground, which fux-vt does not know; Ghostty keeps no special colours). OSC 105 alone resets nothing, as in xterm 411, where ctlseqs says every special colour (see "Departures from the references"). RIS and DECSTR reset the palette and leave the dynamic and special colours, as xterm 411 does (Ghostty keeps all three). Read with `Screen::palette_color`, `Screen::dynamic_color` and `Screen::colors_changed`: state only, the host draws a cell of a changed entry in its colour. Off: ignored, and OSC 10 to 19 queries are events as before | `palette::*`, `palette::tests` |
| `rectangle_checksums` | DECRQCRA (`CSI Pi ; Pp ; Pt ; Pl ; Pb ; Pr * y`, VT520 manual 5-104, DEC STD 070 5-180) → DECCKSR `DCS Pi ! ~ xxxx ST`, four upper-case hex digits. DEC leaves the sum to the terminal; this is xterm's default, which is the VT520's (`xtermCheckRect`; ctlseqs, XTCHECKSUM): each cell's character, plus 0x08 hidden, 0x10 underlined, 0x20 inverse, 0x40 blinking and 0x80 bold, summed in 16 bits and negated. A character past Latin-1 or below a space, and a wide glyph's second half, count as ESC (0x1b), a DEC Special Graphics glyph as the code it was drawn with (0x60 to 0x7e), and a cluster's combining marks are added as they are, as xterm counts them. An empty cell counts as a space, as on DEC's terminals, where an erased cell holds one; xterm skips cells nothing was written to (its `checksumExtension` 8 counts them too), which esctest accepts either way. The page `Pp` is ignored, as xterm ignores it (DEC has 0 mean all pages; fux-vt has one). The rectangle is one-based, relative to the top margin in origin mode, clamped to the screen (to the margins in origin mode), absent or 0 meaning its whole extent; a top below the bottom, or a left past the right, sums nothing (`0000`). It is how esctest reads the screen back (`compare/`'s `esctest`). With it a program can read what its screen shows, which is why xterm refuses it by default (`disallowedWindowOps`); no program in the corpus asks it, and fux's panes leave it off. Off: unhandled | `opt_in::rectangle_checksums_sum_the_cells`, fuzz columns-byte bit `0x80` |

Cells are read as `CellRef`s (`Screen::cell`, `Window::cell`, `Row::cell`,
`Row::cells`), which find a long cluster's text in its row. A consumer that
stores or transports screen contents keeps them in `Cells`, which holds long
clusters the same way (`row.cells().collect::<Cells>()`, `Cells::set`,
`Cells::set_text`; `properties::cells_agree_with_a_plain_list_of_cells`); `Cell::new` (inline, at most `Cell::INLINE_CAPACITY`, 17,
bytes), `Cell::wide_continuation` and `Attributes::new`/`with_*` build the
rest. Parser output never goes through them.

A host that copies cells copies their links itself: `Cells` holds none.

Every public item is documented (`missing_docs` is forbidden). `Options`,
`Error`, `Color`, `Blink`, `MouseProtocolMode`, `MouseProtocolEncoding`,
`Event` and `Unhandled` are `#[non_exhaustive]`, so later releases can add
to them: options are built with `Options::new()` and its `const` `with_*`
methods, and a match on one of the enums needs a fallback arm. Rows and
windows are read through accessors: `Row::id`, `Row::version`,
`Row::wrapped`, `Window::rows`, `Window::cols`, `Window::offset`.

### Deliberate boundary

Upstream does **not** dispatch DECAWM 7; implementing it is a required,
standards-backed correction, tested independently as well as tracked in the
differential inventory. IL/DL outside scrolling margins are ignored, unlike
upstream's accidental row edits there. Both match XTerm(411)
(`fux-vt/compare`'s xterm engine replays them). Upstream left 1047/1048,
ESC D/E/H, programmable tab stops (CSI g, I, Z), IRM, REP and HPA/HPR/VPR
unimplemented; fux-vt implements them, as `xterm-256color` advertises them
(`ind`, `hts`, `tbc`, `cbt`, `smir`, `hpa`) or xterm does. CSI 3J (erase
saved lines) remains ignored (and reported unhandled); CSI f/s/u are
implemented as HVP and SCOSC/SCORC.
ESC =/> set the `application_keypad()` getter, as upstream's did, but fux
never acts on it: there is no keypad encoding. By default there are no
window-title, bell, clipboard, or window-resize side effects from child output,
and no OSC payload is retained. In particular OSC 52 from a child cannot bypass
fux's clipboard policy: fux enables `Options::events` for titles alone, and its
sink drops clipboard events. Window resize from
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
print U+FFFD, and UTF-8-encoded C1 characters are ignored; in control
strings the 8-bit ST (0x9c) is a byte of a character, not a terminator. Seven-bit ESC
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
- **DECSTBM with a bottom margin past the screen** (DEC STD 070, 5-25,
  note 3, ignores it). xterm takes the last line instead, and so does
  fux-vt, as every engine in `compare/` but vt100 does
  (`fux-vt-compare replay --engines all --size 5x5 '\e[3;3H\e[2;99rX'`).
- **TBC 1, 2, 4 and 5** (ECMA-48 8.3.154). ECMA-48 defines them for tab
  stops kept line by line as well (2 clears the active line's, 5 every
  one); xterm keeps one set and ignores them, and so does fux-vt, as
  every engine in `compare/` but libvterm does (`fux-vt-compare replay
  --engines all --size 1x20 '\e[3g\e[5G\eH\e[5g\r\tX'`).
- **CBT with a wrap pending** (ECMA-48 8.3.7 moves back to the previous
  tab stop; DEC STD 070, Appendix D.6.1, ends the wrap on a cursor
  movement). xterm moves its cursor but keeps the wrap pending, so the
  next glyph still goes to the start of the next line; xterm.js leaves
  the cursor where it is. fux-vt, whose pending wrap is always in the
  last column, leaves the cursor there, so text lands where xterm puts
  it; Ghostty, alacritty, libvterm, avt, wezterm and tmux move back and
  end the wrap (`fux-vt-compare replay --engines all --size 2x10
  'abcdefghij\e[ZX'`).
- **A UTF-8 continuation byte alone** (the Unicode Standard, 3.9,
  recommends U+FFFD). xterm reads it as Latin-1, so Latin-1 text keeps
  its `£`, `°` and `©` and a raw C1 control is ignored; so does fux-vt.
  Ghostty, libvterm, avt and tmux print U+FFFD; alacritty, wezterm and
  xterm.js drop it (`fux-vt-compare replay --engines all --size 1x12
  'price \xa35'`).
- **OSC 105 with no parameter resets no special colour.** ctlseqs says
  "If no parameters are given, all special colors will be reset"; xterm
  411 resets none (`OSC 5;0;#333;1;#444`, `OSC 105`, then `OSC 5;0;?;1;?`
  answers `3030` and `4040`), and neither does Ghostty, which keeps no
  special colours. A listed one is reset, as ctlseqs says and as esctest
  expects (its ResetSpecialColor_Single passes in xterm 411).
- **REP after a control function** (ECMA-48 8.3.103 leaves it undefined, so
  this is no departure but a choice where the standard is silent). xterm
  repeats nothing until a character is printed again, and so does fux-vt,
  REP after REP included; xterm.js and tmux agree, and Ghostty, alacritty,
  libvterm, avt and wezterm repeat the last character again
  (`fux-vt-compare replay --engines all --size 1x8 '-\e[2b\e[2b'`).
