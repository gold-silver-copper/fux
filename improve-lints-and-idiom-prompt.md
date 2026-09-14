# Strengthen fux/zor lint discipline and idiom

Execute this in the fux/zor/local-ipc workspace. The goal is a stricter, deduplicated lint
baseline and the small idiom cleanups it surfaces — not a rewrite. Every new lint must be made
green by **fixing the code**, never by a blanket crate-level `allow`. A lint that would force
unreasonable churn is left out with a one-line recorded reason, not denied-then-suppressed.

Behavior must not change: prove it with the existing test suites after every batch. Preserve the
strict ownership boundaries and the standalone/companion CI split. This prompt does not authorize
commits, pushes, PRs, releases, or koh changes; follow explicit authorization in the conversation.

## Start from what is already enforced

Do not re-add lints that exist. Each crate's `Cargo.toml` already denies, in `[lints.clippy]`:
`unwrap_used`, `expect_used`, `panic`, `unreachable`, `unimplemented`, `todo`, `indexing_slicing`,
`string_slice`; and in `[lints.rust]`: `dead_code`, and `unsafe_code` (forbid in fux/local-ipc,
deny in zor). Production already has 14 `unwrap()` (none in hot paths), zero `expect()`, and
panics only in `#[cfg(test)]`. The `structure` test also forbids `#[ignore]`, `todo!`,
`unimplemented!` and unreviewed process spawns.

So "no unwrap / no panic" is done. The work is the next tier, the escape hatches, and dedup.

## 1. Deduplicate the lint config (minimality)

The three crates carry byte-identical `[lints.clippy]`/`[lints.rust]` blocks. Move the shared
baseline into `[workspace.lints]` in the root `Cargo.toml` and set `lints.workspace = true` in each
member (MSRV is 1.95, so workspace lints are available). Keep any genuinely crate-specific lint
(e.g. `unsafe_code = "forbid"` for fux/local-ipc vs `deny` for zor) as a per-crate override with a
comment. The `tools/xtask` and `crates/*/tools/xtask` helper crates should inherit the same
baseline unless a documented reason exempts them. Verify the full CI clippy matrix still passes
unchanged after the move, before adding any new lint.

## 2. The must-have additional lints

Add these in priority order, each as its own batch: enable, run the whole clippy matrix, fix every
finding in code, run the tests, then move on. Do not enable them all at once. If a lint produces
findings that cannot be fixed without degrading the code, drop that lint and record why.

**Correctness and safety (deny):**
- `clippy::panic_in_result_fn` — a function returning `Result` should return `Err`, not panic.
- `clippy::exit` — no `process::exit` outside the top-level `main` dispatch.
- `clippy::mem_forget`, `clippy::float_cmp`, `clippy::lossy_float_literal`.
- `clippy::undocumented_unsafe_blocks` and `clippy::missing_safety_doc` — zor allows `unsafe`, so
  every `unsafe` block/fn must carry a `SAFETY:` justification (fux/local-ipc forbid unsafe, so
  these are free there).
- `let_underscore_drop` (rust lint) — never silently drop a guard/`Result` with `let _ =` when it
  owns a resource; make the intent explicit.

**Discipline for a terminal program (deny, scope carefully):**
- `clippy::dbg_macro` — no stray `dbg!`.
- `clippy::print_stdout` and `clippy::print_stderr` — stray prints corrupt a TUI. The legitimate
  CLI/JSON output paths and diagnostics keep a **narrow, per-function** `#[allow]` with a comment
  naming why (this is the intended output surface), not a crate-level allow. This lint is the point
  of the exercise: it makes every write to the real streams a reviewed decision.

**Idiom cleanups that also do real work (deny where clean, else warn-and-fix):**
- `clippy::redundant_clone` and `clippy::clone_on_ref_ptr` — these directly target the `.clone()`
  hotspots in `ecs/systems/requests.rs` (~27) and `client/controller.rs` (~26); fixing them is the
  borrow-vs-own pass.
- `clippy::implicit_clone`, `clippy::needless_pass_by_value`, `clippy::trivially_copy_pass_by_ref`.
- `clippy::semicolon_if_nothing_returned`, `clippy::manual_let_else`, `clippy::uninlined_format_args`,
  `clippy::str_to_string`, `clippy::string_to_string`, `clippy::unnecessary_wraps`.
- `clippy::ref_option`, `clippy::explicit_iter_loop`, `clippy::map_unwrap_or`.

**Consider but measure first (enable at warn, triage, deny only if the fix count is small):**
- `clippy::pedantic` as a group is too noisy to deny wholesale; instead cherry-pick from it the
  specific lints above. Do not blanket-deny `pedantic` or `nursery`.
- `clippy::cast_possible_truncation`, `cast_sign_loss`, `cast_precision_loss` and
  `clippy::arithmetic_side_effects` are high-value for a program doing terminal geometry and
  sequence math, but noisy. Enable at `warn`, count the findings, and only deny the ones whose
  fixes are contained (checked casts, `saturating_*`/`checked_*` math). Record the decision either
  way; a large unfixable count is a recorded no-op, not a suppression.

## 3. Drive down the existing escape hatches

The tree currently holds these crate/module/function `#[allow(clippy::...)]`: `expect_used` (17),
`panic` (9), `too_many_arguments` (6), `indexing_slicing` (6), `unwrap_used` (1). For each one:

- If it guards test code, keep it but make it as narrow as possible (module- or function-scoped,
  not crate-wide) and confirmed by the `structure`-style invariants.
- If it guards production code, either remove it by rewriting to the checked form (`?`, `get`,
  `let-else`, `saturating_*`), or, where a panic/expect is a genuine invariant, keep the allow with
  an inline comment stating the invariant. No un-commented production allow may remain.
- `too_many_arguments`: prefer grouping parameters into a small struct where it reads better;
  otherwise keep the allow with a reason. Do not chase it at the cost of clarity.

Add a `structure`-test assertion (or extend the existing one) that fails on any new
crate-level `#![allow(clippy::...)]` and on any production `#[allow(clippy::unwrap_used)]` /
`expect_used` / `panic` without an accompanying justification comment, so the baseline cannot
silently erode.

## 4. The remaining idiom items

- Apply the `.clone()` borrow pass driven by `redundant_clone` above; keep only clones that are
  genuine snapshots (`Ids`, config, retained protocol values).
- Optional: if `proto/control.rs` (~1640 lines, pure schema) is worth reducing, split it by
  message family into `proto/control/{requests,layout,workspace,tabs}.rs` behind the same public
  path — behavior-preserving, verified by tests. This is tidying, not required; skip if low value.
- Do not touch the client rendering trio (`controller.rs`, `render.rs`, `client/mod.rs`) beyond the
  lint fixes; further splitting there is churn with regression risk.

## 5. Verification and completion

- After each lint batch: the full CI clippy matrix green with `-D warnings`
  (`--workspace --all-targets`; the zor all-features / no-default / cli variants; local-ipc; the
  fixture-child and xtask manifests; the betamax harness), `cargo fmt --all --check`, and the
  affected `cargo test` suites. `verify-boundaries` and the `structure` test must stay green.
- No new blanket `allow`. Every retained `allow` is narrow and commented.
- Keep the standalone builds independent of companion availability; do not alter the pinned-koh
  composition job except to inherit the workspace lints.
- Write `docs/lint-baseline.md`: the final lint set and where it lives, the lints considered and
  deliberately not adopted (with the finding counts that justified skipping), the escape hatches
  removed vs. retained-with-reason, and the idiom cleanups made. Separate verified changes from
  recorded no-ops.

Completion requires the consolidated workspace lint table, the must-have lints from section 2
landed (or each recorded as a measured no-op with its finding count), the escape-hatch pass done,
the erosion guard added, the full gate green, and the honest ledger. Do not weaken any existing
lint to make the build pass, and do not blanket-`allow` a new lint instead of fixing it.
