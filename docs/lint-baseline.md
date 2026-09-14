# Lint baseline

Tracks `improve-lints-and-idiom-prompt.md`. One shared lint table in the root `Cargo.toml`
(`[workspace.lints]`), inherited by every shipped crate with `lints.workspace = true`. Test code
is exempted from the panic-family lints through `clippy.toml`, not per-test `#[allow]`. The
`structure` test (`lint_baseline_is_enforced_and_not_eroded`) fails if a must-have lint is
dropped or a new crate/module-level `#![allow(clippy::…)]` appears outside the reviewed set.

## Where it lives

- **`Cargo.toml` `[workspace.lints.rust]`**: `dead_code`, `unsafe_code`, `unsafe_op_in_unsafe_fn`
  (all deny). fux and local-ipc additionally keep `#![forbid(unsafe_code)]` in source (stricter);
  zor keeps `#![deny(unsafe_code)]` with reviewed per-block `#[allow(unsafe_code)]`.
- **`Cargo.toml` `[workspace.lints.clippy]`** (all deny): the pre-existing `unwrap_used`,
  `expect_used`, `panic`, `unreachable`, `unimplemented`, `todo`, `indexing_slicing`,
  `string_slice`; plus this pass's additions.
- **`clippy.toml`**: `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests`,
  `allow-dbg-in-tests` — so test code needs no per-test allow for those.

## Added this pass (landed, all findings fixed)

| Lint | Kind | Findings fixed |
|---|---|---|
| `exit` | correctness | 0 |
| `mem_forget` | correctness | 0 |
| `float_cmp`, `lossy_float_literal` | correctness | 0 |
| `undocumented_unsafe_blocks`, `missing_safety_doc` | safety | 0 |
| `dbg_macro` | discipline | 0 |
| `print_stdout`, `print_stderr` | discipline (TUI) | 3 lib sites scoped, plus CLI/wrap surfaces |
| `clone_on_ref_ptr` | idiom | 9 (`.clone()` → `Arc::clone(&…)`) |
| `implicit_clone` | idiom | (folds the removed `str_to_string`/`string_to_string`) |
| `semicolon_if_nothing_returned` | idiom | 25 (autofix) |
| `map_unwrap_or` | idiom | 23 (autofix) |
| `manual_let_else` | idiom | 2 |
| `uninlined_format_args`, `str_to_string`, `explicit_iter_loop` | idiom | autofix |

Zero-finding lints are ratchets: they cost nothing now and block regressions.

`print_stdout`/`print_stderr` output surfaces (reviewed `#![allow]` with a comment): the two
`main.rs` CLI binaries, the `wrap`-feature `zor/src/pty.rs` diagnostics, three library statements
(`--once` dashboard JSON, the codex-start probe line, the foreground `serve` banner), and two
integration-test harnesses.

## Considered and deliberately not adopted (recorded no-ops)

| Lint | Findings | Why not |
|---|---|---|
| `let_underscore_drop` | 95 | The codebase's pervasive best-effort cleanup idiom (`let _ = child.wait()` / `kill()` / `await`); converting all to `drop(...)` is churn without clarity gain. |
| `panic_in_result_fn` | 142 | Almost all are `assert!` inside `-> Result` test and helper functions; it does not honor `allow-panic-in-tests`, so it is pure test noise here. |
| `needless_pass_by_value` | 31 | Flags bevy ECS system/param signatures (`Step`, `Scene`, `Query`) that must be by value; the "fix" is impossible for those, and the rest are not worth the mixed-signal churn. |
| `trivially_copy_pass_by_ref` | 3 | All are serde `skip_serializing_if` predicates that must take `&T`. |
| `unnecessary_wraps` | 1 | A `cfg`-gated function whose other-platform variants need the `Result`; the signature must stay uniform. |
| `string_to_string` | — | Removed from clippy (folded into `implicit_clone`). |
| `pedantic` / `nursery` groups, `arithmetic_side_effects`, numeric `cast_*` | not measured to completion | Too noisy to deny wholesale; the high-value members were cherry-picked above. Left for a future measured pass. |

## Escape hatches driven down

Before: 39 `#[allow(clippy::…)]` sites (17 `expect_used`, 9 `panic`, 6 `too_many_arguments`,
6 `indexing_slicing`, 1 `unwrap_used`). After: 14, all reviewed —

- 25 removed: test-code `unwrap_used`/`expect_used`/`panic` allows made redundant by `clippy.toml`.
- 6 reduced: combined allows trimmed to only the parts `allow-in-tests` does not cover
  (`indexing_slicing`/`string_slice` in test fixtures).
- Retained: 7 test-fixture `indexing_slicing` (+1 with `string_slice`), and 6 production
  `too_many_arguments` on genuinely wide functions.

## Verification

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings` and the zor
matrix (all-features, no-default, cli) and `local-ipc`, all clean; `cargo test` for fux
(lib/ecs/structure 193/84/13), zor lib (197), and local-ipc (13). No behavior change; no
commits, pushes, or koh changes. `tools/xtask` is a separate workspace and a dev/test harness
that prints by design; it is intentionally left outside this baseline.
