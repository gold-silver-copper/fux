# Merge zor into the fux repository

Move zor from its own repository into the fux repository as a second crate of one Cargo
workspace, with its history, so that a control-protocol change is one PR, one CI run and one
gate. Keep fux a single crate with a single binary and keep zor a separate crate with its own
binary, tests and ownership boundary (agent/task/check/artifact policy stays in zor; fux stays a
generic terminal multiplexer). Koh stays a separate repository; its companion pin/patch workflow
is unchanged by this task except where the code that implements it must be generalized.

Work in a fresh worktree of `main` (currently `8efc8b3`) of
`https://github.com/gold-silver-copper/fux`, branch `merge/zor-workspace`. Keep the runtime of
`/Users/kisaczka/Desktop/code/fux` unchanged. Read-only GitHub inspection is fine; do not push,
open a PR or mutate the zor repository until the verification below passes, then push the
branch and open a PR against `main`. Do not merge.

## Facts to verify before changing anything

- Zor's owning repository is `https://github.com/gold-silver-copper/zor`. fux currently pins
  base `2a8769ede679211f81624823247c8494f046d869` plus `dependency-patches/zor.patch` (six
  wire/consumer files) through `dependency-patches/manifest.json`; the patched state is the one
  fux is verified against. Check whether zor `main` has moved past that base and whether the
  patch was ever published upstream. The imported code must equal the verified state: base plus
  patch, or a newer upstream that already contains it.
- The Rust gate (`tools/xtask`, commands in `checks.json`) reconstructs zor from the manifest
  in `dependencies verify --build --headless`, and CI's "Optional koh and zor integrations" job
  (manual dispatch only) does the same. `tests/zor_integration.rs` and the `zor-*` xtask
  scenarios take `ZOR_BIN`, with `FUX_REQUIRE_ZOR_BIN=1` turning absence into failure.
- `tools/xtask/reference.json` pins a separate `references/herdr` source used by comparison
  tooling; understand whether it is affected before touching the reconstruction code.
- `tools/xtask` is its own workspace today. Decide whether it joins the new root workspace or
  stays separate; the deciding constraint is that `cargo install fux` and the package check keep
  working and the lockfile story stays simple. Explain the decision in the PR.

## Shape of the result

- The root `Cargo.toml` becomes a virtual workspace (no root package) with members
  `crates/fux` and `crates/zor`, one shared `Cargo.lock` at the root, and
  `[workspace.package]` for the shared edition/rust-version/license. Move fux's package
  (`Cargo.toml`, `src/`, `tests/`, `build.rs` if any, `LICENSES` references) to `crates/fux`
  with `git mv` so history follows. Do not split fux into library sub-crates and do not add
  feature-gated builds; `crates/fux` stays one crate with one binary.
- Import zor with history into `crates/zor` (`git subtree add --prefix=crates/zor <zor-remote>
  <commit>` or an equivalent filter-repo merge). Record the exact zor commit imported and, if the
  pinned base plus patch was used, apply the patch as a separate commit so the history shows
  what changed.
- Path update checklist (each must be found and fixed, then proven by the checks below):
  `cargo install --path crates/fux --locked` replaces `--path .`; `tests/verify/release-package.sh`
  and the xtask package-version helper package `crates/fux`; every `--manifest-path` and
  `CARGO_BIN_EXE_*` use in `tools/xtask`, `checks.json`, `reference.json`, CI and docs; the
  agent-boundary test's source roots (fux inventory from `crates/fux/src` only); `.gitignore`
  entries (`/zor` becomes tracked, `/target` stays at the root); the fixture-child crate under
  `tests/verify/fixture-child` and its lockfile; README/HANDOFF/docs paths; and the `tests/`
  directory of fux, which moves with the crate.
- Remove the zor entries from `dependency-patches/manifest.json`, delete `zor.patch`, and
  remove zor reconstruction from the xtask dependency runner, the gate plan and CI. Koh's
  entries, reconstruction and checks remain and must still pass exactly; the koh checkout path
  in the manifest may stay `references/koh`.
- `tools/xtask` either joins the workspace as `tools/xtask` (member, shared lockfile) or stays
  its own workspace; the deciding constraints are that the gate's `cargo run --locked
  --manifest-path tools/xtask/Cargo.toml` entry point keeps working and that the root lockfile
  does not pull xtask's dependencies into `cargo install` of fux. Explain the decision in the PR.
- Zor's tests and the `zor-*` scenarios run against the workspace-built binary
  (`CARGO_BIN_EXE_zor` where available, or the xtask locating the workspace target) instead of
  an externally supplied `ZOR_BIN`. Keep `ZOR_BIN` as an optional override for measuring a
  different build, and keep the "required binary" enforcement so the integration cannot
  silently skip.
- Move the zor integration coverage from the manual companion CI job into the normal PR
  matrix (the zor scenarios currently take under five minutes locally; confirm hosted timing
  and split the job if it exceeds fifteen minutes). Keep the manual dispatch for koh only.
- Zor's own lints, formatting config and clippy deny set apply to `crates/zor`; fux's apply to
  `crates/fux`. Do not relax either to match the other; per-crate `[lints]` tables are fine.
- Preserve the agent-boundary inventory test: declarations under `crates/zor` must not enter
  fux's inventory, and fux must not gain any agent semantics from the move.
- Update `README.md`, `HANDOFF.md`, `docs/design.md`, `docs/native-integration.md` and the
  koh/zor sections of `dependency-patches/` documentation so no text still describes zor as a
  pinned external repository or fux as living at the root. Leave zor's own README in
  `crates/zor` and add a short note at its top saying where it now lives.
- Leave the zor GitHub repository untouched; the PR description should state what should
  happen to it afterwards (archive with a pointer) as a separate manual step.

## Rules

- Keep iteration fast: no single build, test run or CI wait over five minutes; prefer under
  two. Never launch batch campaigns. Do not run the full 45-command headless gate yourself
  before the end; leave its final command for the user, but do run each of its individual
  checks that you changed.
- No behavior change in either binary. Any protocol, CLI or wire-format change is out of
  scope; if the import reveals one is needed, stop and report it.
- Do not vendor or duplicate code between the crates to make the workspace build; shared
  types stay where they are today.
- Use one independent subagent to review the final diff for: accidental fux/zor coupling,
  dropped zor tests or scenarios, lockfile drift, CI coverage that was lost versus the manual
  companion job, and documentation that still describes the old layout.

## Verification and handoff

Run within the time budget: `cargo fmt --check` for every crate, strict `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo test --workspace` (split by crate if needed),
the `local_cli` and zor integration suites, the tooling tests, the koh reconstruction
(`dependencies verify` for koh only), and the release-package check. Confirm
`cargo install --path crates/fux --locked` into a temporary prefix still produces a working
`fux`, and that `cargo build --workspace` from the root builds both binaries.

Commit in logical steps on `merge/zor-workspace` (move fux to `crates/fux`, subtree import
into `crates/zor`, patch application, workspace wiring, tooling/CI, docs) so each step's diff
is reviewable and `git log --follow` works for moved files. Push and open a PR against `main` only after all checks
above pass; wait for hosted CI and report its result. Write
`.verification/merge-zor/REPORT.md` with the imported zor commit, the exact reconstruction
equivalence check (patched base bytes versus imported bytes), what changed in tooling and CI,
the reviewer's disposition, and the follow-ups: archiving the zor repository, cutting a fux
release, and repinning koh against a published version so its patch workflow can retire too.
