# Permanent expectations

These `.snap` files were captured **only from vt100 0.16.2**, with the existing
fux Replies callback, during the pre-migration differential phase. They were
not generated from fux-vt. The temporary recorder's source is preserved in
that phase's commit history. `FUX_VT_CAPTURE_ORACLE=1 cargo test -p fux-vt
--test differential permanent_fixtures --locked` was the capture command;
`/tmp/fux-vt-evidence/golden-capture.log` records its passing result. The
recorder also asserted that its independent snapshots matched the owned
parser before writing anything.

`../corpus/fixtures.rs` preserves all eleven original common-subset operation
sequences; the differential suite asserts that the lists agree exactly.
`../golden.rs` now pins every operation's complete retained cell data, styles,
wide flags, wrap flags, cursor, exposed modes and reply bytes at chunk sizes
1, 2, 3, 7 and whole. Missing cells in a snapshot mean empty/default cells,
not wildcard cells. Row numbering includes retained history oldest first.
Row IDs/versions are intentionally excluded from oracle expectations because
the oracle has no such concept; independent invariant tests cover them.

## Mapping before scaffolding removal

| Differential coverage | Permanent coverage |
| --- | --- |
| text, cursor, editing, scrolling, margins, SGR, input/mouse modes, alternate buffers, saved cursor/replies/reset, Unicode, ignored/invalid input | All eleven fixtures at every operation and every chunking in `golden.rs`, plus focused assertions in `semantics.rs` |
| ASCII history + resize, height-only wrap reset | `resize_rejects_bad_capacity_without_mutating_state`, `height_only_resize_clears_live_wrap_metadata` |
| terminal-edge main/history/alternate/application-cursor/C1 streams | `terminal_edge_streams_preserve_primary_history_and_modes`, exact expected text/history/modes |
| Generated adversarial seeds 0–19, 4096 bytes, 2x2/4x12/24x80; whole/byte/seeded chunks | `permanent_adversarial_corpus_is_chunk_invariant_and_bounded`, same inputs/geometries plus 1x1/1xN/Nx1 and a full 160 KiB stream; independent structural/storage and chunk-equivalence invariants |
| Required DECAWM and corrected out-of-margin IL/DL differences | `autowrap_disabled_overwrites_without_scrolling`, `line_edits_outside_margins_leave_the_grid_unchanged`; executed xterm evidence in `verification/fux-vt-xterm-results.json` |
| Known tiny-grid oracle crashes | `tiny_grids_wrap_and_drop_wide_glyphs_without_underflow` with explicit safe expectations |
| ASCII fast path | `ascii_run_path_equals_scalar_dispatch_on_the_permanent_corpus` bypasses the run path in its scalar reference |

The generated-stream comparison is not claimed to provide a golden final
screen for every random input. It provides full oracle comparison at each
operation plus causal diagnostics for precisely the two documented oracle
bugs; permanent invariants cover the complete retained generated corpus and
focused/golden fixtures pin its individual terminal operations. Exact
measured divergence boundaries and the minimized cases are retained in
`verification/fux-vt-corpus-differences.json` and
`verification/fux-vt-divergences.json`.

Upstream's internal origin flag, pending wrap and margins lack getters.
Their effects are compared through cursor/text/scrolling probes. Programmed
tab stops, graphical protocols, unimplemented aliases, and private query
forms remain documented exclusions, not guessed compatibility.
