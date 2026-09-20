# Keybinding consistency verification

Base: `9807d5c`, merged PR #21 on `minimal-bevy-fux`. Changes are confined to
bindings, modal command navigation, copy-mode exit, tests and documentation.

## Implemented contract

- Exact new defaults, unique keys and previous/next pairs are asserted in
  `assets::tests::coherent_defaults_have_exact_unique_keys_and_action_pairs`.
  README contains the full schema, old-to-new migration table and configuration
  precedence. Existing user binding lists are not rewritten.
- Prefix/help lists select actionable rows, skip headings and indicators, keep
  selection visible, and execute via the existing shared action dispatch.
  Unavailable selected actions retain both dim and reverse styles and report
  their existing reason without acting.
- Unmodified arrows, paging, Home/End, Enter and Esc belong to the list before
  configured bindings. Left/Right are reserved no-ops. Nonreserved prefix
  shortcuts still dispatch directly. Explicit menus/help retain modal ownership.
  A doubled configured prefix remains literal, including a navigation-key prefix.
- Existing chooser/context overlays retain captured targets and revalidation.
  Workspace actions now include save/load; actions losing default shortcuts
  remain in their context menus or available to custom bindings. List paging
  uses the same capacity as its rendering, including tiny views.
- Copy-mode `c` clears without exiting; Esc and `q` exit in one press; `g` returns
  live. Exiting clears the obsolete mode hint. Existing clipboard/paste ownership,
  selection stability, native picking and process ownership remain unchanged.
- The existing viewer-local `help_scroll` field now denotes the selected action
  index, not a heading-inclusive viewport offset. It is clamped to the current
  binding list and, like the whole Viewer, excluded from layout scenes. Context
  overlays remain unreflected viewer components. No new registry, controller,
  protocol, dependency, timer or persistent configuration migration was added.
  Obsolete help key dispatch and cached overlay wheel bounds were removed.

## Direct regression evidence

- `tests/design/keybindings.rs`: hot-reloaded custom Down/Right bindings cannot
  resize the pane while navigating; compares actual native Nodes and focus,
  checks independent viewers and reverse-styled selection, then executes a tab
  creation with Enter. Also verifies dim/reverse unavailable commands, no action
  on invocation, copy clear/exit/live semantics, and byte-exact doubled navigation
  prefix forwarding to a real raw-mode child.
- `actual_default_shortcuts_decode_modifiers_pairs_and_menu_navigation`: a real
  outer-PTY frontend receives actual Ctrl/Alt/Shift arrow escape sequences,
  Tab/Shift+Tab, Backspace and `[`/`]`/`{`/`}`. Assertions check resize via native
  Nodes, movement via native ChildOf, pane focus, tab/workspace selection, rendered
  command-row selection and empty child-input files until ordinary `OK` typing.
- `menus_keep_unbound_actions_and_share_navigation_including_layout_prompts`:
  pane/tab/workspace action reachability, Home/End and paging at heights 0–7,
  selected-row styling, cancellation and actual save/load text-prompt entry.
- `tiny_unicode_command_selection_is_visible_even_when_disabled`: heights 0–4
  and widths 0–2, Unicode/custom labels, clipped output and selected dim/reverse
  cells. Existing every-binding short-height tests verify full reachability.
- Existing suites still exercise wheel scrolling, custom bindings/prefix hot
  reload, unknown keys, literal prefixes, prompt/confirmation paste isolation,
  actual frontend incomplete-paste cancellation, captured targets under focus
  changes/removal, independent selection/history, scene remapping, native PTY
  sizes, process cleanup and blocked paint/hot-output regressions.

## Executed gates

macOS arm64, from the new worktree:

```sh
cargo fmt
cargo fmt --check
RUSTC_WRAPPER= CARGO_TARGET_DIR=../fux-minimal-bevy/target FUX_DESIGN_CAPTURE="$PWD/verification/keybindings" cargo test --locked
RUSTC_WRAPPER= CARGO_TARGET_DIR=../fux-minimal-bevy/target cargo clippy --locked --all-targets -- -D warnings
RUSTC_WRAPPER= CARGO_TARGET_DIR=../fux-minimal-bevy/target cargo build --release --locked
git diff --check
```

**32 unit + 33 integration tests pass.** Logs: [tests](keybindings/tests.log),
[Clippy](keybindings/clippy.log), [release build](keybindings/build.log).
The shared target directory is a build cache, not the source checkout.

## Render review

Manually inspected [renders.png](keybindings/renders.png): custom arrow bindings
remain visible; selected commands are reversed; unavailable selected commands
are also dim; grouped default help shows the new modifiers/pairs; narrow help
retains selection and an overflow marker; pane menus and workspace choosers keep
compact bottom-right placement. Matching ANSI/text/JSON captures preserve actual
cells, styles and cursor metadata. The PNG is an optional Pillow rasterization,
not a desktop screenshot; font metrics can leave gaps in box-drawing strokes.

Reproduce the contact sheet with the existing optional renderer:

```sh
python verification/render-interactions.py verification/keybindings \
  keybindings-selected keybindings-unavailable help narrow-help \
  interaction-menu interaction-workspaces
```

`keybindings-frontend.ansi` is actual attached frontend output for the new
shortcuts; `frontend-interactions.ansi` includes copy, confirmation and fragmented
paste isolation. Tests assert child bytes and native state, not just screenshots.

## Limits

Linux/other Unix platforms and every external terminal emulator were not run.
Modifier decoding is proven through the actual macOS outer PTY with standard
terminal escape sequences, not a claim about OS shortcuts intercepted before
reaching fux. Clipboard still requires explicit write-only policy and terminal
OSC52 support. Existing bounded viewport selection and process-containment
limitations remain unchanged. No benchmark result is claimed for this PR.
