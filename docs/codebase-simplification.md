# Codebase simplification record

Tracks `docs/prompts/simplify-fux-codebase-prompt.md`. One row per batch: what changed, lines
removed, any behaviour difference, and the gate result. Line counts are `git diff --shortstat`
for the batch commit.

| Batch | What changed | Lines | Behaviour difference | Gate |
| --- | --- | --- | --- | --- |
| 2 | Correctness: single `CloseViewer` on queue overflow; `Viewer::attached_to` shared by both admission paths; unreachable `Inspect` arm removed and `layout_control::apply` split into `read`/`edit` (`is_read_only` deleted); `PaneId(0)` sentinel replaced by `Outcome::{Now, Deferred}`; `#[serde(default)]` on the six tracked-operation `instance` fields; `ManagerReply::into_descriptor` replaces two enumerated wildcard matches | 10 files changed, 247 insertions(+), 108 deletions(-) | One `CloseViewer` effect per overflowed viewer instead of two. A detaching viewer no longer counts toward the per-workspace viewer limit on the select/transfer path. The six tracked operations with `instance` omitted now fail validation with `invalid-request` ("tracked operations require a server instance") instead of a JSON field error, matching `events` and `split`. | fmt, clippy, boundary regenerated, fux/local-ipc/zor tests, rustdoc all green; release-package.sh skipped (preexisting failure, Batch 9) |
