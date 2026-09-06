# Replace pane boxes with a top bar and shared separators

Execute this specification in the fux repository (0.3.0, bevy_ecs rewrite on `main`). It changes
only how the viewer draws a workspace; the ECS model, protocols and process lifecycle stay as they
are. Read README.md, docs/design.md, docs/ecs-acceptance.md, `src/client/render.rs`,
`src/client/hints.rs`, `src/client/screen.rs`, `src/ecs/systems/layout.rs`, `src/layout.rs`,
`src/view.rs` and `tests/verify/viewer.py` first.

## Problem

Every pane is drawn as a full cyan box (`┌─┐│└┘`). With one pane that is a frame around nothing;
with splits, neighbours get two lines where one would do; each pane loses two rows and two columns;
and the fixed colour fights the terminal's theme. The tab strip appears only with several tabs, so
the screen jumps when a second tab opens and there is no permanent answer to "which workspace, which
tab, which pane".

## Target look

One bar at the top, always visible, one row high, three zones:

```
 default │ main  tab-2 ▌second▐ tab-4 │ 2: vim ~/proj
```

- Left: workspace name, dim.
- Middle: the tabs in order. The current tab is the only emphasised element on screen (reverse
  video). Other tabs plain. Labels are truncated with `…` when the bar is too narrow; the current
  tab keeps priority, then its neighbours.
- Right: the focused pane as `id: title`, truncated from the left, dim. When the focused pane has
  exited, show `id: title (exit N)`.

Panes have no outer frame. Adjacent panes share a single separator (`│`, `─`, junctions
`┬ ┴ ├ ┤ ┼`), one cell wide, drawn between them only, never on the screen edges. Separators are dim;
the separators touching the focused pane are drawn bold/bright. With one pane there are no
separators at all; focus is carried by the bar.

Colours are configurable, with toned-down defaults that are easy on the eyes and work on dark and
light backgrounds: no saturated hues, no background fills except the current tab. Add a `[style]`
table with exactly these keys, each taking one of the sixteen ANSI colour names, `default`
(the terminal's foreground) or `none`:

| Key | Default | Used for |
|---|---|---|
| `bar` | `bright-black` | workspace name, inactive tabs, pane `id: title` |
| `tab-active` | `default` (drawn reverse) | the current tab |
| `separator` | `bright-black` | separators not touching the focused pane |
| `separator-focused` | `default` | separators touching the focused pane (also bold) |
| `notice` | `yellow` | transient notices in the bar's right zone; errors use `red` |

Unknown keys and values are configuration errors. Do not add any other styling options; there is
no true-colour or 256-colour support in this change.

Transient notices (copy result, errors, "Workspace NAME") move into the right zone of the bar,
replacing the pane title for two seconds or until the next key; the separate bottom notice bar
goes away. The command popup and the copy-mode hint stay where they are (bottom, transient).

## Geometry

- The bar occupies row 0 of every viewer, always, so the tab area is `rows - 1` for one tab too.
  Remove the "strip only with several tabs" rule in `src/ecs/systems/layout.rs` and wherever the
  frame or viewer relies on it.
- Sibling leaves are separated by exactly one cell: `LayoutTree::geometry` (or its caller) must
  reserve the gap between siblings and give the rest to the panes. Pane content size is the leaf
  rectangle itself; there is no longer an inner area. Update `view::PaneRect`/`Frame` only if the
  compositor cannot derive separators from adjacent leaf rectangles; prefer deriving.
- Keep the smallest-viewer negotiation. Hidden and viewer-less tabs keep their last area.
- Tiny screens: a 1-row terminal shows only the bar; a 2-row terminal shows the bar and one row of
  pane; zero-area leaves stay safe and never panic. Mouse hit testing treats separator cells and the
  bar as non-pane cells; clicks there do nothing (drag-resize on separators is out of scope).

## Files that must change

- `src/client/render.rs`: bar, separators, focus emphasis, notice in the bar, configured colours.
- `src/client/controller.rs`/`hints.rs`: remove the standalone info/error bar; route
  `report_info`/`report_error` into the bar zone with the two-second expiry (viewer-local timer,
  no server involvement).
- `src/ecs/systems/layout.rs`, `src/layout.rs`: bar row always reserved; sibling gap.
- `src/config.rs`: the `[style]` table above, validated, documented in README's configuration
  example; `fux bindings` unaffected.
- Tests: `viewer.py` asserts a `┌` on the first row and no strip with one tab; replace those with
  bar assertions (workspace name, current tab emphasised, `id: title` on the right, no box
  characters at the edges, exactly one separator column between two side-by-side panes). The
  fixture-child tiny-viewer scenario and `render.rs`/`hints.rs` unit tests must reflect the new
  geometry. `tests/ecs.rs` `fresh_workspace_has_one_tab_and_pane_and_no_strip` becomes a
  bar-row assertion (`rect.y == 1`, content rows `rows - 1`).
- Docs: README "Keys" section, docs/design.md (layout phase, viewer), docs/ecs-acceptance.md rows
  that cite the strip rule and the border, CHANGELOG (0.3.1).

## Guarantees to keep

- Byte-exact input, per-viewer frames and focus, the creation barrier, mouse coordinate translation
  (pane-relative coordinates now start at the leaf rectangle, not one cell inside a box), Shift
  override, copy-mode selection bounds (a selection never includes a separator cell), and the final
  frame painted before terminal restoration.
- No protocol version change unless a frame field must change; if it must, bump the attachment
  version, update koh's real-fux tests through `dependency-patches/` and rerun the koh suites.

## Verification

Run, and record results in docs/ecs-acceptance.md under a "Top bar" section:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
ZOR_BIN=$PWD/zor/target/debug/zor FUX_REQUIRE_ZOR_BIN=1 cargo test --locked -- --test-threads=1
cargo doc --no-deps --locked
cargo +1.95.0 check --all-targets --locked
cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked
FUX_BIN=$PWD/target/debug/fux KOH_REQUIRE_FUX_BIN=1 cargo test --manifest-path references/koh/Cargo.toml --test gateway --locked
tests/verify/release-package.sh --allow-dirty
```

Add real-viewer coverage in `viewer.py` for: single pane (bar, no separators, no edge boxes),
two side-by-side panes (one separator column, bright next to the focused pane, dim otherwise),
a stacked split (one separator row with the right junction where it meets a column), three tabs
with a long label truncated, a notice appearing in the bar and disappearing, and a 2×20 terminal.
Have an independent reviewer who did not implement the change review the diff and fix confirmed
findings before declaring it done.

## Authorization

Implement, verify and review autonomously; routine design details are yours. Commit or push only
when asked. Use disposable HOME/XDG directories and owned processes only; never touch personal
sessions or a running server that is not yours.
