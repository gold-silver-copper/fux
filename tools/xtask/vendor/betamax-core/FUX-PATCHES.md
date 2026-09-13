# Betamax integration patches

This is the published `betamax-core` 0.1.11 source from crates.io, corresponding to
joshka/betamax commit `78d0ae29148ad55cc138ed59fcbd54801f37f55c`.
Upstream: https://github.com/joshka/betamax
Licenses: LICENSE-MIT and LICENSE-APACHE; additional notices: THIRD_PARTY.md.

The fux harness applies this local Cargo patch only within tools/xtask. Production
fux/zor binaries do not depend on it. Preserve these changes when updating:

- `ghostty/engine.rs`: public live resize of the Ghostty terminal and raster canvas,
  preserving primary/alternate buffers, modes, and scrollback; expose synchronized
  output mode so checkpoints can wait for completed application redraws.
- `ghostty/renderer.rs`: update canvas/grid without resetting the renderer; paint
  backgrounds before glyphs so wide characters are not erased by spacer cells;
  omit wide continuation placeholders from plain text/state spans; rasterize the
  basic box-drawing strokes on cell boundaries so line spacing and font weight
  cannot disconnect or misalign pane separators.

These changes are needed for resize, Unicode labels, and wide-character drag
preview acceptance. The harness tests exercise raw VT, styles, raster output and
resize. The viewer scenario compares Ghostty and vt100 text at each checkpoint.
No screenshots are generated from reconstructed vt100 output.
