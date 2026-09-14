# Codebase simplification record

Tracks `docs/prompts/simplify-fux-codebase-prompt.md`. One row per batch: what changed, lines
removed, any behaviour difference, and the gate result. Line counts are `git diff --shortstat`
for the batch commit.

| Batch | What changed | Lines | Behaviour difference | Gate |
| --- | --- | --- | --- | --- |
| 1 | Hygiene: removed 2,351 tracked gate logs (`docs/verification/`, now gitignored) and 18 dated reports, indexed in `docs/verification.md`; root prompts moved to `docs/prompts/`; `HANDOFF.md`, `release-readiness.md` rewritten; `design.md` corrected (no Waits phase, four exclusive systems, real dirty-flag reason); orphan regex comment dropped. `tools/archive` kept: xtask provenance hashes it. | 2398 files changed, 566 insertions(+), 282938 deletions(-) | None. | fmt, clippy, all workspace tests, rustdoc green; release-package.sh fails on main already (local-ipc `read_exact_until` unpublished), fixed in Batch 9 |
| 2 | Correctness: single `CloseViewer` on queue overflow; `Viewer::attached_to` shared by both admission paths; unreachable `Inspect` arm removed and `layout_control::apply` split into `read`/`edit` (`is_read_only` deleted); `PaneId(0)` sentinel replaced by `Outcome::{Now, Deferred}`; `#[serde(default)]` on the six tracked-operation `instance` fields; `ManagerReply::into_descriptor` replaces two enumerated wildcard matches | 10 files changed, 247 insertions(+), 108 deletions(-) | One `CloseViewer` effect per overflowed viewer instead of two. A detaching viewer no longer counts toward the per-workspace viewer limit on the select/transfer path. The six tracked operations with `instance` omitted now fail validation with `invalid-request` ("tracked operations require a server instance") instead of a JSON field error, matching `events` and `split`. | fmt, clippy, boundary regenerated, fux/local-ipc/zor tests, rustdoc all green; release-package.sh skipped (preexisting failure, Batch 9) |
