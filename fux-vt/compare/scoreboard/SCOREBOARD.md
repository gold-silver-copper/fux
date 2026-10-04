# fux scoreboard
For 9b97a7b, 2026-10-04, from `fux-vt/compare/run.sh scoreboard`; `history.jsonl` beside it has a line for each commit it was kept for.

| Axis | Measure | fux / fux-vt | Beside |
| --- | --- | --- | --- |
| Conformance | esctest2 pass rate, fux-vt | 48.6% (267/549) | in a fux pane: 41.2% (129/313) |
| Real programs | corpus recordings agreeing, field by field | 89.4% (101/113) | judged by xterm, ghostty, alacritty, libvterm, avt, wezterm |
| Multiplexer | recordings identical through it (transparency) | 69.0% (78/113) identical, 35 differ as recorded | tmux 49.1% (52/106); zellij 65.1% (69/106) |
| Speed | instructions per workload | 135 workloads against main: median -0.50%, 1 flagged, 17 too noisy to judge |  |
| Speed | MB/s on the corpus | 213 (fastest on 4 of 8 workloads) | Ghostty 211, alacritty 157 |
| Latency | keystroke p50 / p99, idle | 0.22 / 1.32 ms | tmux 0.19 / 60.51 ms; zellij 12.35 / 13.65 ms; direct 0.05 / 2.93 ms |
| Latency | keystroke p50 / p99, flood | 0.20 / 0.52 ms | tmux 0.30 / 0.55 ms; zellij 9.26 / 25.71 ms |
| Footprint | MiB per pane with full history | 25.7 MiB | tmux 6.6 MiB; zellij 24.2 MiB |
| Footprint | server MiB with 50 panes | 10.3 MiB | tmux 5.5 MiB; zellij 224.6 MiB |
| Footprint | idle CPU over 10 s | 0.0 ms | tmux 0.0 ms; zellij 10.0 ms |
| Footprint | bytes to the client per frame (median load) | 780 B | direct 1706 B |
| Robustness | fuzz time since the last crash | 24 min over 30 target runs | last crash: session at 2026-10-03T19:54Z |

| Check | Commit, when, how long, exit |
| --- | --- |
| corpus | 9b97a7b 2026-10-03T22:51Z 24s exit 0 |
| transparency | 9b97a7b 2026-10-03T22:51Z 4s exit 0 |
| random | 9b97a7b 2026-10-03T22:51Z 2s exit 0 |
| cases | 9b97a7b 2026-10-03T22:51Z 8s exit 0 |
| random-wide | 9b97a7b 2026-10-03T22:51Z 21s exit 0 |
| random-no-reflow | 9b97a7b 2026-10-03T22:51Z 21s exit 0 |
| esctest | 9b97a7b 2026-10-03T22:52Z 31s exit 0 |
| against | 9b97a7b 2026-10-03T22:52Z 36s exit 1 |
| verdicts-1 | 9b97a7b 2026-10-03T22:55Z 143s exit 0 |
| esctest-in-fux | 9b97a7b 2026-10-04T00:13Z 61s exit 0 |
| multiplexers | 9b97a7b 2026-10-04T00:17Z 270s exit 0 |
| feel | 9b97a7b 2026-10-04T00:32Z 852s exit 0 |
| info | 9b97a7b 2026-10-04T00:32Z 16s exit 0 |
