# What the corpus's programs send

Made by `fux-vt-compare inventory` from the recordings in this directory (see the harness README, "The corpus"). Each sequence is normalized: numbers that only place the cursor or pick a colour are `n`, a mode or an SGR attribute is a row of its own, and an XTGETTCAP request shows the capabilities it asks for. "Count" counts every time it was sent, in all the recordings. "fux-vt" is what a fux pane's parser does with it, as fux sets it up (`fux::pane::OPTIONS`: events, DECRQM, in-band resize, the size query, colour-scheme reports, the kitty keyboard protocol, hyperlinks, prompt marks and fux's identity), with what fux itself answers.

Recordings:

- `bash`: bash, GNU bash, version 5.3.3(1)-release (aarch64-apple-darwin24.4.0) (17 steps, 877 bytes)
- `claude-ghostty`: claude-ghostty, Claude Code 2.1.288 (Claude Code) (3 steps, 3110 bytes)
- `claude-main`: claude-main, Claude Code 2.1.288 (Claude Code) (3 steps, 3025 bytes)
- `claude`: claude, Claude Code 2.1.288 (Claude Code) (3 steps, 10793 bytes)
- `delta-diff`: delta-diff, delta 0.19.2, git version 2.51.0 (5 steps, 45384 bytes)
- `delta-log`: delta-log, delta 0.19.2, git version 2.51.0 (8 steps, 34377 bytes)
- `fzf-height`: fzf-height, fzf 0.65.2 (brew) (7 steps, 15185 bytes)
- `fzf`: fzf, fzf 0.65.2 (brew) (9 steps, 29408 bytes)
- `gls`: gls, ls (GNU coreutils) 9.12 (1 steps, 1900 bytes)
- `helix`: helix, helix 25.07.1 (a05c151b) (16 steps, 63873 bytes)
- `less`: less, less 668 (POSIX regular expressions) (12 steps, 26267 bytes)
- `man`: man, man (macOS), mandoc (8 steps, 8431 bytes)
- `tmux`: tmux, tmux 3.7c (21 steps, 16112 bytes)
- `vim`: vim, VIM - Vi IMproved 9.1 (2024 Jan 02, compiled Aug  8 2026 16:52:02) (24 steps, 25352 bytes)
- `zsh`: zsh, zsh 5.9 (arm64-apple-darwin26.0) (17 steps, 1549 bytes)

## Asked after

| Feature | Sent as | Programs |
| --- | --- | --- |
| synchronized output (CSI ? 2026 h/l, and its DECRQM) | `CSI ? 2026 $ p` ×3, `CSI ? 2026 h` ×16, `CSI ? 2026 l` ×16 | claude, claude-ghostty, claude-main |
| hyperlinks (OSC 8) | `OSC 8 (close)` ×24, `OSC 8 (open)` ×24 | claude-ghostty, gls |
| underline styles (SGR 4:n, 21) | not sent by any | |
| underline colour (SGR 58, 59) | `SGR 59` ×66 | helix |
| semantic prompts (OSC 133) | not sent by any | |
| in-band resize (CSI ? 2048 h/l) | not sent by any | |
| XTGETTCAP (DCS + q) | `DCS + q (#2)` ×1, `DCS + q (#4)` ×1, `DCS + q (%i)` ×1, `DCS + q (*7)` ×1, `DCS + q (Co)` ×1, `DCS + q (k1)` ×1, `DCS + q (kd)` ×1, `DCS + q (kl)` ×1, `DCS + q (kr)` ×1, `DCS + q (ku)` ×1 | vim |
| DECRQSS (DCS $ q) | `DCS $ q  q` ×1 | vim |
| DECRQM (CSI ? n $ p, CSI n $ p) | `CSI ? 1016 $ p` ×3, `CSI ? 12 $ p` ×1, `CSI ? 2026 $ p` ×3 | claude, claude-ghostty, claude-main, vim |
| kitty keyboard (CSI ? u, CSI > n u, CSI < u, CSI = n u) | `CSI < 1 u` ×1, `CSI < u` ×9, `CSI > 5 u` ×7, `CSI ? u` ×4 | claude, claude-ghostty, claude-main, helix |
| modifyOtherKeys (CSI > 4 ; n m, CSI ? 4 m) | `CSI > 4 m` ×3, `CSI > 4; m` ×2, `CSI > 4;2 m` ×7, `CSI ? 4 m` ×1 | claude, claude-ghostty, claude-main, vim |
| colour queries (OSC 4/10/11/12 ?) | `OSC 10 ?` ×5, `OSC 11 ?` ×5 | delta-diff, delta-log, tmux, vim |
| XTVERSION, DA2 (CSI > q, CSI > c) | `CSI > 0 q` ×3, `CSI > c` ×2, `CSI > q` ×1 | claude, claude-ghostty, claude-main, tmux, vim |
| current directory (OSC 7) | not sent by any | |
| clipboard (OSC 52) | not sent by any | |
| kitty graphics (APC G) | `APC G` ×3 | claude, claude-ghostty, claude-main |

## Not implemented, or in part

| Sequence | What | Count | Programs | fux-vt |
| --- | --- | ---: | --- | --- |
| `CSI ? 12 l` | steady cursor (att610) | 32 | helix, tmux, vim | not implemented: a mode fux-vt does not keep, consumed quietly |
| `APC G` | kitty graphics | 3 | claude, claude-ghostty, claude-main | not implemented: consumed, dropped |
| `CSI 16 t` | XTWINOPS: cell size in pixels? | 3 | claude, claude-ghostty, claude-main | not implemented: reported as unhandled |
| `CSI ? 1016 l` | reset: mouse: SGR pixel encoding | 3 | claude, claude-ghostty, claude-main | not implemented: a mode fux-vt does not keep, consumed quietly |
| `CSI ? 7727 h` | application escape key (mintty) | 3 | tmux | not implemented: a mode fux-vt does not keep, consumed quietly |
| `CSI 0 % m` |  | 1 | vim | not implemented: reported as unhandled |
| `CSI 14 t` | XTWINOPS: window size in pixels? | 1 | tmux | not implemented: reported as unhandled |
| `CSI ? 1015 h` | mouse: urxvt encoding | 1 | helix | not implemented: a mode fux-vt does not keep, consumed quietly |
| `CSI ? 1015 l` | reset: mouse: urxvt encoding | 1 | helix | not implemented: a mode fux-vt does not keep, consumed quietly |
| `CSI ? 12 h` | blinking cursor (att610) | 1 | vim | not implemented: a mode fux-vt does not keep, consumed quietly |
| `CSI ? 4 m` | XTQMODKEYS, modifyOtherKeys? | 1 | vim | not implemented: reported as unhandled |
| `CSI ? 7727 l` | reset: application escape key (mintty) | 1 | tmux | not implemented: a mode fux-vt does not keep, consumed quietly |
| `DCS $ q  q` | DECRQSS | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (#2)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (#4)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (%i)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (*7)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (Co)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (k1)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (kd)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (kl)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (kr)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS + q (ku)` | XTGETTCAP | 1 | vim | not implemented: a query, consumed unanswered |
| `DCS z` |  | 1 | vim | not implemented: consumed, dropped |

## Implemented

| Sequence | What | Count | Programs | fux-vt |
| --- | --- | ---: | --- | --- |
| `SGR 0` |  | 5179 | delta-diff, delta-log, fzf, fzf-height, gls, helix, man, tmux, vim, zsh | implemented |
| `SGR 38;2;n;n;n` |  | 3188 | claude, claude-ghostty, claude-main, delta-diff, delta-log, helix | implemented |
| `CSI n;n H` | CUP | 1709 | delta-diff, delta-log, helix, less, man, tmux, vim | implemented |
| `SGR 48;2;n;n;n` |  | 1401 | claude, claude-ghostty, claude-main, delta-diff, delta-log, helix | implemented |
| `CSI K` | EL | 1071 | bash, claude-ghostty, claude-main, delta-diff, delta-log, fzf, fzf-height, less, man, tmux, vim, zsh | implemented |
| `SGR 30-37` |  | 809 | delta-diff, delta-log, gls, tmux, vim | implemented |
| `SGR 90-97` |  | 752 | vim | implemented |
| `SGR 38;5;n` |  | 688 | delta-diff, fzf, fzf-height, vim | implemented |
| `CSI n C` |  | 552 | claude, claude-ghostty, claude-main, fzf, fzf-height, tmux, vim, zsh | implemented |
| `CSI n G` |  | 429 | claude, claude-ghostty, claude-main | implemented |
| `SGR 1` |  | 380 | claude, claude-ghostty, claude-main, fzf, fzf-height, gls, man, vim, zsh | implemented |
| `SGR 39` |  | 322 | claude, claude-ghostty, claude-main, helix, tmux | implemented |
| `CSI n B` |  | 239 | claude, claude-ghostty, claude-main, fzf, fzf-height, tmux, zsh | implemented |
| `SGR 48;5;n` |  | 231 | fzf, fzf-height, vim | implemented |
| `CSI H` | CUP, home | 203 | claude, delta-diff, less, man, tmux, vim | implemented |
| `CSI n A` |  | 188 | claude, claude-ghostty, claude-main, fzf, fzf-height, tmux | implemented |
| `ESC M` | RI | 181 | delta-diff, less, man | implemented |
| `CSI ? 25 l` | DECTCEM, hide the cursor | 137 | claude, claude-ghostty, claude-main, fzf, fzf-height, helix, tmux, vim | implemented |
| `CSI 1 K` |  | 116 | tmux | implemented |
| `CSI ? 25 h` | DECTCEM, show the cursor | 105 | claude, claude-ghostty, claude-main, fzf, fzf-height, helix, tmux, vim | implemented |
| `CSI 0 K` |  | 90 | delta-diff, delta-log | implemented |
| `CSI 2 K` |  | 82 | claude, claude-ghostty, claude-main | implemented |
| `SGR 49` |  | 76 | claude, claude-ghostty, claude-main, helix | implemented |
| `SGR 24` |  | 75 | helix, man, zsh | implemented |
| `CSI n X` | ECH | 74 | tmux | implemented |
| `SGR 7` |  | 74 | bash, claude-ghostty, claude-main, delta-log, helix, less, man, vim, zsh | implemented |
| `SGR 4` |  | 69 | helix, man, zsh | implemented |
| `SGR 59` |  | 66 | helix | implemented |
| `SGR 27` |  | 51 | bash, claude-ghostty, claude-main, delta-log, helix, less, man, vim, zsh | implemented |
| `ESC ( B` | G0: ASCII | 41 | claude, claude-ghostty, claude-main, tmux | implemented |
| `CSI ? 7 h` | DECAWM, autowrap | 31 | fzf, fzf-height | implemented |
| `CSI ? 7 l` | reset: DECAWM, autowrap | 31 | fzf, fzf-height | implemented |
| `CSI n D` |  | 27 | claude, claude-ghostty, claude-main, zsh | implemented |
| `OSC 8 (close)` | hyperlink: end | 24 | claude-ghostty, gls | implemented: ends the open hyperlink |
| `OSC 8 (open)` | hyperlink: start | 24 | claude-ghostty, gls | implemented: opens a hyperlink, which the cells printed keep (Row::link) |
| `CSI n;n r` | DECSTBM, scrolling region | 21 | tmux, vim | implemented |
| `SGR 22` |  | 21 | claude, claude-ghostty, claude-main, helix | implemented |
| `SGR 40-47` |  | 20 | tmux, vim | implemented |
| `CSI ? 2004 h` | bracketed paste | 19 | bash, claude, claude-ghostty, claude-main, fzf, fzf-height, helix, tmux, vim, zsh | implemented |
| `CSI G` |  | 19 | claude-ghostty, claude-main, fzf, fzf-height | implemented |
| `CSI ? 2004 l` | reset: bracketed paste | 17 | bash, claude, claude-ghostty, claude-main, fzf, fzf-height, helix, tmux, vim, zsh | implemented |
| `CSI ? 1000 l` | reset: mouse: press and release | 16 | claude, claude-ghostty, claude-main, fzf, fzf-height, helix, tmux | implemented |
| `CSI ? 1002 l` | reset: mouse: button motion | 16 | claude, claude-ghostty, claude-main, fzf, fzf-height, helix, tmux | implemented |
| `CSI ? 1006 l` | reset: mouse: SGR encoding | 16 | claude, claude-ghostty, claude-main, fzf, fzf-height, helix, tmux | implemented |
| `CSI ? 2026 h` | synchronized output: begin | 16 | claude, claude-ghostty, claude-main | implemented |
| `CSI ? 2026 l` | synchronized output: end | 16 | claude, claude-ghostty, claude-main | implemented |
| `CSI C` |  | 16 | bash | implemented |
| `CSI ? 1003 l` | reset: mouse: any motion | 14 | claude, claude-ghostty, claude-main, helix, tmux | implemented |
| `CSI n SP q` | DECSCUSR, cursor style | 14 | helix | implemented: the cursor style is kept |
| `CSI A` |  | 10 | zsh | implemented |
| `CSI c` | DA1, primary device attributes | 10 | claude, claude-ghostty, claude-main, delta-diff, delta-log, helix, tmux | answers (first with `\e[?62;22c`) |
| `CSI < u` | kitty keyboard: pop flags | 9 | claude, claude-ghostty, claude-main | implemented: tracked; fux encodes keys as it asks (src/encode.rs) |
| `CSI J` | ED | 9 | fzf-height, zsh | implemented |
| `SGR 100-107` |  | 8 | vim | implemented |
| `SGR 2` |  | 8 | claude, helix | implemented |
| `CSI 2 J` |  | 7 | helix, less, tmux, vim | implemented |
| `CSI > 4;2 m` | XTMODKEYS: modifyOtherKeys 2 | 7 | claude, claude-ghostty, claude-main, vim | implemented: tracked; fux encodes keys as it asks (src/encode.rs) |
| `CSI > 5 u` | kitty keyboard: push flags 5 | 7 | claude, claude-ghostty, claude-main, helix | implemented: tracked; fux encodes keys as it asks (src/encode.rs) |
| `CSI ? 1 h` | DECCKM, application cursor keys | 6 | delta-diff, delta-log, less, man, tmux, vim | implemented |
| `CSI ? 1 l` | reset: DECCKM, application cursor keys | 6 | delta-diff, delta-log, less, man, tmux, vim | implemented |
| `CSI ? 1004 l` | reset: focus reporting | 6 | claude, claude-ghostty, claude-main, helix, tmux, vim | implemented |
| `CSI ? 1049 h` | alternate screen, cursor saved | 6 | fzf, helix, less, man, tmux, vim | implemented |
| `CSI ? 1049 l` | reset: alternate screen, cursor saved | 6 | fzf, helix, less, man, tmux, vim | implemented |
| `CSI n @` | ICH | 6 | bash | implemented |
| `CSI r` |  | 6 | claude, claude-ghostty, claude-main | implemented |
| `ESC 7` | DECSC | 6 | claude, claude-ghostty, claude-main | implemented |
| `ESC 8` | DECRC | 6 | claude, claude-ghostty, claude-main | implemented |
| `ESC =` | DECKPAM | 6 | delta-diff, delta-log, less, man, tmux, vim | implemented |
| `ESC >` | DECKPNM | 6 | delta-diff, delta-log, less, man, tmux, vim | implemented |
| `CSI ? 1004 h` | focus reporting | 5 | claude, claude-ghostty, claude-main, helix, vim | implemented |
| `OSC 0` | title and icon name | 5 | claude, claude-ghostty, claude-main | implemented: an event; fux sets the pane title |
| `OSC 10 ?` | foreground colour query | 5 | delta-diff, delta-log, tmux, vim | implemented: a ColorQuery event; fux answers with its client terminal's colour (src/outer.rs) |
| `OSC 11 ?` | background colour query | 5 | delta-diff, delta-log, tmux, vim | implemented: a ColorQuery event; fux answers with its client terminal's colour (src/outer.rs) |
| `CSI 6 n` | DSR, cursor position report | 4 | fzf-height, vim | answers (first with `\e[1;1R`) |
| `CSI ? 2031 h` | colour-scheme change reports | 4 | claude, claude-ghostty, claude-main, tmux | implemented |
| `CSI ? 2031 l` | reset: colour-scheme change reports | 4 | claude, claude-ghostty, claude-main, tmux | implemented |
| `CSI ? u` | kitty keyboard: query flags | 4 | claude, claude-ghostty, claude-main, helix | answers (first with `\e[?5u`) |
| `CSI n M` | DL | 4 | vim | implemented |
| `CSI > 0 q` | XTVERSION | 3 | claude, claude-ghostty, claude-main | answers (first with `\eP>\|fux 0.17.0\e\\`) |
| `CSI > 4 m` | XTMODKEYS: modifyOtherKeys reset | 3 | claude, claude-ghostty, claude-main | implemented: tracked; fux encodes keys as it asks (src/encode.rs) |
| `CSI ? 1000 h` | mouse: press and release | 3 | fzf, fzf-height, helix | implemented |
| `CSI ? 1002 h` | mouse: button motion | 3 | fzf, fzf-height, helix | implemented |
| `CSI ? 1006 h` | mouse: SGR encoding | 3 | fzf, fzf-height, helix | implemented |
| `CSI ? 1016 $ p` |  | 3 | claude, claude-ghostty, claude-main | answers (first with `\e[?1016;0$y`) |
| `CSI ? 2026 $ p` | DECRQM: synchronized output? | 3 | claude, claude-ghostty, claude-main | answers (first with `\e[?2026;2$y`) |
| `CSI n d` |  | 3 | tmux | implemented |
| `CSI 22;n t` | XTWINOPS: push title | 2 | vim | implemented: reported as unhandled; fux's pane pushes its title (src/pane.rs) |
| `CSI 23;n t` | XTWINOPS: pop title | 2 | vim | implemented: reported as unhandled; fux's pane pops its title (src/pane.rs) |
| `CSI > 4; m` | XTMODKEYS: modifyOtherKeys reset | 2 | vim | implemented: tracked; fux encodes keys as it asks (src/encode.rs) |
| `CSI > c` | DA2, secondary device attributes | 2 | tmux, vim | answers (first with `\e[>1;1700;0c`) |
| `CSI ? 1005 l` | reset: mouse: UTF-8 encoding | 2 | tmux | implemented |
| `SGR 23` |  | 2 | vim | implemented |
| `SGR 29` |  | 2 | vim | implemented |
| `CSI 18 t` | XTWINOPS: size in characters? | 1 | tmux | answers (first with `\e[8;40;120t`) |
| `CSI 22;n;n t` | XTWINOPS: push title | 1 | tmux | implemented: reported as unhandled; fux's pane pushes its title (src/pane.rs) |
| `CSI 23;n;n t` | XTWINOPS: pop title | 1 | tmux | implemented: reported as unhandled; fux's pane pops its title (src/pane.rs) |
| `CSI < 1 u` | kitty keyboard: pop flags | 1 | helix | implemented: tracked; fux encodes keys as it asks (src/encode.rs) |
| `CSI > q` | XTVERSION | 1 | tmux | answers (first with `\eP>\|fux 0.17.0\e\\`) |
| `CSI ? 1003 h` | mouse: any motion | 1 | helix | implemented |
| `CSI ? 12 $ p` |  | 1 | vim | answers (first with `\e[?12;0$y`) |
| `CSI ? 996 n` | colour-scheme query | 1 | tmux | implemented: reported as unhandled; fux answers with its client terminal's scheme (src/outer.rs) |
| `CSI n L` | IL | 1 | vim | implemented |
