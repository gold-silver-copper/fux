# Minimal design restoration

Implemented against `minimal-bevy-fux` base `870807c` (including PRs 16–19), using original fux main `8b41296` as the visual reference. Verified on 2026-09-20, macOS arm64, Rust 1.98.1 (`48a229cea`), pinned Bevy 0.19.1. No dependency/lockfile changes.

## Restored contract

| Requirement | Implementation and direct evidence |
| --- | --- |
| Borderless panes; one bottom bar | Default leaf Nodes have no borders/insets; pane content starts at `(0,0)`. ANSI decoded into vt100 asserts colored content at the first/last cells, cursor at both extremes, and the full last-row background. `borderless_surface…`, `cursor_uses_first…` |
| Shared split separators and focus | Native one-cell Node gaps; separator cells derived only from computed visible Split siblings. Nested split test checks `│`, `─`, `├`, muted/bold focus, picking, zoom, and custom wider gaps remaining blank. No layout solver or pane hit-test replacement. |
| Correct content sizing and pointer coordinates | PTYs use full content rectangles, not dimensions minus two. Existing real `stty size` checks now assert 11×40 and 23×80. Raw child-byte tests assert `(1,1)` and the last content cell, releases, bottom-row exclusion, and no forwarding into a larger viewer's blank margins. |
| Bottom-right vertical help | Prefix shows the column immediately; explicit help scrolls. Content-sized surface, normal binding order, one-cell horizontal padding, contrasting background, bold heading, hidden-row indicators. Unit tests prove every binding remains reachable at all heights 2–39. Hot reload covers shorter/empty lists, changed prefix, Unicode/custom actions, and clamped scroll. |
| Prefix/input policy | Literal doubled prefix, unknown prefix keys remain modal, Escape cancellation, paste isolation, no mouse click-through even before the first panel paint. Prefix Right actually changes native flex growth; explicit help arrows/page keys and panel wheel scroll instead. |
| Unified prompts and status | Rename/paste/edit/cancel/accept and scene-path prompts share the corner surface; editable text uses reversal. Yellow notices, red errors, focused exit status and unfocused dim reversed exit marker are asserted at actual cells. |
| Clean repaint/cursor/style | Overlay closes restore content/cursor; cursor hidden during help/prompts. Bar/panel reset application styles; application red/blue output is preserved. Resizing while open, narrow ellipsis, zero dimensions and a bar-only one-row viewport are tested. |
| Native architecture and existing behavior | Existing scene remapping/Visibility, cache invalidation/arbitrary components, process identity/lifecycle, stock Viewer removal, clipboard effects, blocked frontend drain, and independent viewer focus/zoom/history tests all pass. The old pane-header ordering assertion now checks real terminal content. |
| Actual attached frontend | A real `fux attach` in a 47×13 outer PTY receives actual prefix/help/escape/rename/bracketed-paste/detach bytes; completed paints are decoded and checked. Detach emits the terminal reset/alternate-screen restoration sequence. |

## Tiny backing-size safety

Removing the old viewer clamp exposed a pinned vt100 0.16.2 underflow in `grid::col_wrap` during one-row wrapping; one-column wide glyphs have the same subtraction hazard. The PTY/emulator backing minimum is therefore **2×2**, while visible rectangles still permit 0/1 cells. Snapshot extraction clips columns without splitting wide glyphs; painting clips rows and cursor. A real live-output regression shrinks to a 1-column, 1-content-row view, feeds wide glyphs, checks the backing dimensions, and verifies the bar is not overwritten. This is a documented tiny-size limitation, not an invented content row or a claim of exact one-cell PTY emulation.

## Gates and artifacts

All final commands passed:

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
git diff --check
```

**22 tests: 7 unit + 15 integration**, including a real attached frontend. The final ordinary parallel suite passed **10 consecutive complete runs**, then another capture run. [Final test output](design/tests.log), [Clippy](design/clippy.log), [release build](design/build.log).

Earlier repeated runs exposed intermittent startup connection failures also noted in the historical refinement evidence. A disposable instrumented copy of stock Bevy HTTP reported `AddrInUse`; a sampled failed process was parked with no TCP listener. The fixture now uses distinct non-ephemeral ports and serializes listener reservation-to-ready with outer-PTY forks, which can temporarily inherit reservations. Scenarios themselves remain parallel. Readiness completes a real `rpc.discover` exchange. No retry/replacement transport or production HTTP change was made. Diagnostic dependency overrides and sampling code were removed, and the original lockfile restored before the final gates/repetitions.

Source manifest SHA-256: `a55f896053ae68f1cb88fa38d1e3a41ec5e5a0dc2370467a22637f304ea1053a`:

```sh
shasum -a 256 Cargo.lock Cargo.toml rust-toolchain.toml src/*.rs src/chrome/*.rs tests/*.rs tests/design/*.rs | shasum -a 256
```

Release executable SHA-256: `ace5d92f5ba6aede2d2c221b55d554455b4b8554fa7922c379ed609cf01591ed`. Tests/captures used the normal debug test build; release was built, not separately performance-benchmarked. Historical throughput/CPU figures are not new measurements of this change.

### Render review

[Visually inspected contact sheet](design/renders.png): single pane (80×24), nested splits (61×15), full help (60×40), rename prompt (60×12), narrow help (18×7), and bar-only tiny view (20×1). The single-pane fixture intentionally retains colored application output at opposite corners. Review confirmed no outer frames/title strips, internal separators only, a persistent bottom bar, and bottom-right overlays growing upward. It also led to reducing wasted key-alignment space in narrow help.

The PNG is a rasterization of decoded ANSI cells with a representative ANSI palette and system fonts, **not a desktop-terminal screenshot**. Font-specific junction appearance and ANSI colors may vary in the user's terminal. Raw `*.ansi` and decoded, right-trimmed `*.txt` snapshots accompany it. `frontend.ansi` separately records the **real attached frontend's** outer-PTY stream, including restoration; `frontend-help.txt` is its decoded help frame. Do not blindly print the transcript to a working terminal: it contains alternate-screen/mode controls.

Reproduce captures (including optional per-cell JSON for external rasterization):

```sh
FUX_DESIGN_CAPTURE="$PWD/verification/design" cargo test --locked --test remote_lifecycle design::
```

No screenshot framework, Python dependency or image renderer is part of fux. The one-off contact-sheet conversion ran outside the project. Tests' temporary servers/processes/directories are cleaned up by their fixture owners.

## Intentional differences from original main

- Keep Ctrl-B and this rewrite's configured actions. Prefix arrows execute bindings; explicit help owns keyboard navigation. No timeout.
- Keep configuration order under one compact bold Commands heading, rather than importing the old command-group/availability registry. Unknown/custom actions remain visible. Entries are not clickable; the wheel uses the actual painted panel bounds.
- No synthetic tabs, tab choosers, selection engine, confirmation workflow, configurable theme engine or historical scene migration. This rewrite's workspace/pane model stays intact.
- Loaded native Nodes are not normalized or stripped. The default is borderless; arbitrary registered scene components/native flex/grid/visibility remain supported.
- Backing PTY minimum 2×2 as documented above; actual zero/one-cell viewer geometry remains supported.
- This restores the visual language, not every feature or pixel of the old ratatui client. No Ratatui, GPU renderer, difference shader, protocol redesign or recurring UI timer was introduced.
