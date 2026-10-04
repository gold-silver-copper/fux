# fux scoreboard
For 288ff60, 2026-10-04, from `fux-vt/compare/run.sh scoreboard`; `history.jsonl` beside it has a line for each commit it was kept for.

| Axis | Measure | fux / fux-vt | Beside |
| --- | --- | --- | --- |
| Conformance | esctest2 pass rate, fux-vt | 69.0% (379/549) | in a fux pane: 56.3% (175/311) |
| Real programs | corpus recordings agreeing, field by field | 89.4% (101/113) | judged by xterm, ghostty, alacritty, libvterm, avt, wezterm |
| Multiplexer | recordings identical through it (transparency) | 69.0% (78/113) identical, 35 differ as recorded | tmux 49.1% (52/106); zellij 65.1% (69/106) |
| Speed | instructions per workload | 135 workloads against main: median -29.51%, 0 flagged, 18 too noisy to judge |  |
| Speed | MB/s on the corpus | 324 (fastest on 7 of 8 workloads) | Ghostty 181, alacritty 145 |
| Latency | keystroke p50 / p99, idle | 0.61 / 1.27 ms | tmux 0.23 / 2.04 ms; zellij 13.29 / 14.62 ms; direct 0.06 / 0.17 ms |
| Latency | keystroke p50 / p99, flood | 0.15 / 0.31 ms | tmux 0.28 / 3.52 ms; zellij 9.92 / 13.04 ms |
| Footprint | MiB per pane with full history | 5.0 MiB | tmux 6.5 MiB; zellij 24.1 MiB |
| Footprint | server MiB with 50 panes | 5.0 MiB | tmux 5.5 MiB; zellij 223.1 MiB |
| Footprint | idle CPU over 10 s | 0.0 ms | tmux 0.0 ms; zellij 0.0 ms |
| Footprint | bytes to the client per frame (median load) | 909 B | direct 1706 B |
| Robustness | fuzz time since the last crash | 34 min over 40 target runs | last crash: session at 2026-10-03T19:54Z |

## fux-vt beside Ghostty's core (libghostty-vt)

Ahead or level on 33 of 33.

| Axis | fux-vt | Ghostty | |
| --- | --- | --- | --- |
| esctest2 pass rate (379/549 and 328/546) | 69.0% | 60.1% | ahead |
| corpus recordings agreeing with xterm | 101 of 113 | 74 of 113 | ahead |
| corpus points agreeing with xterm | 939 of 992 | 753 of 992 | ahead |
| instructions per byte: ascii | 15.8 | 18.3 | ahead |
| instructions per byte: dense-cells | 50.7 | 113.7 | ahead |
| instructions per byte: medium-cells | 67.1 | 90.1 | ahead |
| instructions per byte: cursor-motion | 63.5 | 77.4 | ahead |
| instructions per byte: scrolling | 213.4 | 215.3 | level |
| instructions per byte: scroll-region | 45.3 | 49.2 | ahead |
| instructions per byte: unicode | 227.0 | 298.4 | ahead |
| instructions per byte: corpus | 52.6 | 70.7 | ahead |
| memory, bytes per cell, a screen: dense, 80 columns | 20 B | 123 B | ahead |
| memory, bytes per cell, a screen: empty, 80 columns | 20 B | 45 B | ahead |
| memory, bytes per history row: ascii, 80 columns | 655 B | 698 B | ahead |
| memory, bytes per history row: corpus, 80 columns | 120 B | 426 B | ahead |
| memory, bytes per history row: cursor-motion, 80 columns | 683 B | 695 B | ahead |
| memory, bytes per history row: dense-cells, 80 columns | 685 B | 4219 B | ahead |
| memory, bytes per history row: medium-cells, 80 columns | 685 B | 692 B | level |
| memory, bytes per history row: scroll-region, 80 columns | 95 B | 693 B | ahead |
| memory, bytes per history row: scrolling, 80 columns | 44 B | 693 B | ahead |
| memory, bytes per history row: unicode, 80 columns | 791 B | 1206 B | ahead |
| memory, bytes per cell, a screen: medium, 80 columns | 20 B | 61 B | ahead |
| memory, bytes per cell, a screen: dense, 200 columns | 13 B | 77 B | ahead |
| memory, bytes per cell, a screen: empty, 200 columns | 13 B | 18 B | ahead |
| memory, bytes per history row: ascii, 200 columns | 1307 B | 1733 B | ahead |
| memory, bytes per history row: corpus, 200 columns | 118 B | 803 B | ahead |
| memory, bytes per history row: cursor-motion, 200 columns | 1499 B | 1725 B | ahead |
| memory, bytes per history row: dense-cells, 200 columns | 1681 B | 10343 B | ahead |
| memory, bytes per history row: medium-cells, 200 columns | 1414 B | 1724 B | ahead |
| memory, bytes per history row: scroll-region, 200 columns | 97 B | 1280 B | ahead |
| memory, bytes per history row: scrolling, 200 columns | 43 B | 1732 B | ahead |
| memory, bytes per history row: unicode, 200 columns | 1322 B | 2844 B | ahead |
| memory, bytes per cell, a screen: medium, 200 columns | 13 B | 28 B | ahead |

| Check | Commit, when, how long, exit |
| --- | --- |
| corpus | 288ff60 2026-10-04T11:48Z 26s exit 0 |
| transparency | 288ff60 2026-10-04T11:47Z 7s exit 0 |
| random | 288ff60 2026-10-04T11:47Z 5s exit 0 |
| cases | 288ff60 2026-10-04T11:47Z 12s exit 0 |
| oracle | 288ff60 2026-10-04T11:47Z 7s exit 0 |
| random-wide | 288ff60 2026-10-04T11:48Z 22s exit 0 |
| random-no-reflow | 288ff60 2026-10-04T11:48Z 22s exit 0 |
| esctest | 288ff60 2026-10-04T11:48Z 30s exit 0 |
| against | 288ff60 2026-10-04T11:49Z 25s exit 0 |
| verdicts-1 | 288ff60 2026-10-04T11:53Z 209s exit 0 |
| esctest-in-fux | 288ff60 2026-10-04T12:35Z 72s exit 0 |
| multiplexers | 288ff60 2026-10-04T12:39Z 268s exit 0 |
| feel | 288ff60 2026-10-04T12:54Z 856s exit 0 |
| info | 288ff60 2026-10-04T12:54Z 17s exit 0 |
