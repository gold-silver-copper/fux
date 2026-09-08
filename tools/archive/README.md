# Historical evidence source archives

These non-executable text files preserve exact first-party source bytes named by retained
capture hashes. They are provenance records, not runnable tooling or current implementations.
Paths below this directory mirror original repository paths, with `.txt` appended.

The workflow acceptance source was recovered from `target/package/fux-0.3.3` and verified
against the retained SHA-256 `67d99545444086e875e49539884ed787443d625ccceaa336103fddc525c8700e`.
The workflow comparison capture source matches
`217b868447d056f43d705f23d35fa1897842f47d88a02f690e7ca0b25b82b87b`.
The Rust validator selects these archives only for those historical path/hash pairs.

`zor/src/main.rs.txt` reconstructs the screen comparison's historical CLI source by removing
only the later H4 headless launch argument handling and capability dispatch, and restoring
its original argv field position. Its exact recorded hash is
`23b0a94f3ca7249425afc8ba95d64536e042b4da791370e328efc39c3191a534`.
Independent review checked this diff against the current source. Archive selection requires
that historical path/hash pair; a capture recording current bytes uses current source.

`tools/headless_journal.py.txt` retains the original journal capture source used by
`docs/headless-performance/journal-baseline.json`. The Rust offline validator checks
its exact hash for this path. It is not executable; current captures use
`fux-xtask headless-journal` and record their own Rust harness/source provenance.

`tools/comparisons/detection_screens.py.txt` is the exact original screen capture
harness (`b6ba5464871932b881fff5832658eccd6bc1551d05b31afb31cb54c37be7500d`).
The verifier uses this nonexecutable archive only for that historical path/hash;
Rust capture provenance instead verifies `tools/xtask/src/evidence/screens.rs`.

`tools/comparisons/detection_freshness.py.txt` retains the original signed-out
freshness capture. The validator accepts this archive only for the exact historical
path/hash. Rust captures list `tools/xtask/src/freshness_capture.rs` in source
provenance and verify those current bytes; no historical timing is relabelled.

`tools/comparisons/capture_traffic.py.txt` retains the exact original traffic capture.
Its historical path/hash alone selects this nonexecutable archive. New Rust reports
record their actual Rust capture, proxy and C-worker sources instead.

`tools/comparisons/controller_setup.py.txt` preserves the original setup capture.
Only its exact historical path/hash selects this archive. Fresh Rust reports verify
the Rust capture and C-worker source; historical results are unchanged.

`tools/comparisons/resources.py.txt` preserves the paired resource capture's exact
historical bytes (`ea33ab75e5770f2234f02652e1bfea42f5c05a064adc2e943b8a72242d0c7209`).
Only that path/hash selects the archive. `capture-resources` records its own Rust
capture/support and unchanged C-worker/sampler sources; old timings remain unchanged.

`tools/comparisons/prompt_boundary.py.txt`, `input_retry.py.txt` and
`service_failure.py.txt` retain exact historical comparison harness bytes. The
prompt helper archive is selected by the retained setup/resource validators only
for `3a29778f9717501d45edf8968e28e7124f913eb3df0d5e6d2c794eef057a2d89` at its
original path. Current capture commands use Rust and the exact extracted
`tools/xtask/src/prompt-worker.c`; fresh reports record their actual source hashes.

`tools/headless_performance.py.txt` preserves the exact original viewer capture
(`a528eaa007ad7ccaf61b00a27028c72224399e8327843f3b902fdf9fff2e1e7f`). The
historical validator selects it only for that original path/hash. Current captures
use `headless-performance` and validate their actual Rust/worker/helper source set;
historical before/after measurements are not replaced.

`tests/verify/zor_contention.py.txt` preserves the workflow capture's historical
helper (`2436494f573378559056bd43cf4b2ba6141873aa6ed62e41ac70d7c1f186a189`).
Only that original path/hash selects this nonexecutable archive. Runtime callers
use Rust contention support; the retained workflow measurement is unchanged.
