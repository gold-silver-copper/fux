# Replace the bottom command band with a bottom-right column

Execute this in the fux repository (0.3.2 on `main`). It changes only the viewer's popup
presentation; the server, protocols and bindings stay as they are.

## Problem

The command popup is a full-width band at the bottom: a title row, at most ten body rows and a
footer row, paged. With 25 entries it always needs three pages, and each 80-cell row holds about
20 characters of text, so most of the band is padding and the user pages through a list that
would fit on screen.

## Target

A column anchored bottom-right, directly above the bar and flush with the right edge:

```
                                             Panes
                                               |  split side by side
                                               -  split stacked
                                               x  close pane
                                               r  resize split
                                               [  history and copy
                                             Focus
                                               h  focus left
                                               …
                                             Session
                                               d  detach
                                               ?  show bindings
 default │ main  second                        │ 1: zsh ~/proj
```

- One row per binding, `key  label`, in the registry's group order, with the group headings kept
  as their own rows (bold). There is no title row and no footer row. Unavailable actions stay
  dimmed and pressing one still explains why. With the default bindings that is 25 rows; on a
  24-row terminal (23 above the bar) the column scrolls by two rows, which is accepted.
- Width is derived from the content: one cell of padding, the key column (as wide as the widest
  key name), two cells, the widest label, one cell. Clamped to the terminal width; labels are
  truncated with `…` only when the terminal is narrower than the column.
- Height is `min(entries, rows − 1)`; the column never covers the bar. The rest of the screen keeps
  painting the panes.
- Scrolling only when the entries do not fit, row-wise: `↑`/`↓` move the window by one row,
  `PgUp`/`PgDn` by a screenful, clamped (no wrap). When rows are hidden above or below, the top or
  bottom row of the column becomes an indicator, `▲ n more` / `▼ n more`, taking that row from the
  body. Scrolling is only offered while the popup is open, as today.
- The tab chooser, workspace chooser, rename and new-workspace inputs and the close confirmations
  use the same column and styling. They keep their title row (it is the question being asked) and
  their one-line footer of key hints; a chooser's focused row is kept visible by scrolling. The
  thin one-line hints (copy mode, resize) stay full-width single rows above the bar.
- Colours: keep the current popup style (white on dark gray) unless `[style]` gains nothing new;
  do not add configuration for this.
- Degradation order on small terminals: scroll, then truncate labels, then show the single row that
  fits. A one-row terminal shows only the bar.

## Files

- `src/client/hints.rs`: the column painter and its geometry (width, anchor, scrolling,
  indicators); `page_count` becomes a scroll clamp.
- `src/client/mod.rs`: `hint_page` becomes a row offset; `Page(delta)` applies one row for arrows
  and a screenful for page keys, clamped to the content.
- `src/client/input.rs`: emit distinct events for arrow and page keys while the popup is open if it
  does not already.
- `src/client/controller.rs`: the chooser/confirmation/input panels keep their API.
- Tests: `hints.rs` unit tests (right anchoring, width from content, all entries visible on a tall
  terminal, indicators and clamped scrolling on a short one, focus kept visible, tiny sizes never
  panic); `tests/verify/viewer.py` detects the popup by an entry (`split side by side`) instead of
  the removed title, checks the column sits at the right edge with no text left of it on those
  rows, and on a four-row terminal scrolls row by row through every binding.
- Docs: README "Keys" (describe the column, remove the "dim = unavailable" footer reference and
  say it in prose), docs/design.md viewer paragraph, docs/ecs-acceptance.md (a "Command column"
  subsection with evidence), CHANGELOG (0.3.3), version bump.

## Verification

Same gate as before (fmt, strict clippy, root tests with real zor, rustdoc, MSRV check,
fixture-child, koh gateway suites, packaged binary, `git diff --check`), plus an independent review
of the diff; fix confirmed findings before declaring it done. Commit or push only when asked.
