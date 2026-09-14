# Finish the workspace migration: release, retire the companion patches, settle CI

Complete the follow-ups left by PR #6 (zor merged into fux as `crates/zor`). Work in a fresh
worktree of `main` of `https://github.com/gold-silver-copper/fux`; keep the runtime of
`/Users/kisaczka/Desktop/code/fux` unchanged. The evidence and notes from the merge are in
`/Users/kisaczka/Desktop/code/fux-zor/.verification/merge-zor/REPORT.md`.

Authorization for this task, and only this task: you may push branches and open PRs on
`gold-silver-copper/fux` and `gold-silver-copper/koh`, create annotated tags and GitHub
releases on fux, and archive `gold-silver-copper/zor`. Do not merge any PR, do not publish to
crates.io, and do not delete anything. Each outward step below says what evidence must exist
before it runs; if the evidence is missing, stop that step and report instead.

Keep iteration fast: no single build, test run or CI wait over five minutes; never launch
batch campaigns; do not run the full 45-command headless gate yourself (leave its command in
the handoff). Ask nothing mid-task; make routine calls and state assumptions in the report.

## 0. Preconditions to verify

- PR #6 is merged into `main` (record the merge commit). If it is not, stop and report; do not
  merge it.
- `cargo build --workspace --locked` at `main` produces `target/debug/fux` and
  `target/debug/zor`; `cargo test -p fux --test structure` passes.
- Identify the current release practice: look at `CHANGELOG.md`, existing tags
  (`git tag --list`), `.github/workflows/release-verify.yml`, `crates/fux/Cargo.toml` version
  (`0.6.0`), and whether `fux` and `zor` exist on crates.io (`cargo search fux --limit 1`,
  `cargo info fux` if available). Record what you find; it decides step 2.

## 1. CI settling (fux PR)

Branch `ci/settle-workspace`. Small, separately reviewable commits:

- Restrict `on: push` in `.github/workflows/ci.yml` to `branches: [main]` so a branch with an
  open PR is tested once, not twice in parallel. Keep `pull_request` and `workflow_dispatch`.
  Update `crates/fux/tests/structure.rs` expectations only if they check the trigger text.
- Hosted macOS flakiness: four different timing-bound real-process tests failed once each on
  hosted macOS during PR #6 (fux session-server startup in `control-workflow`, the viewer copy
  notice in the `viewer` scenario, fixture-child `concurrent_first_clients_elect_exactly_one_server_and_workspace`,
  zor's `tasks::git::tests::subprocess_deadline_and_output_pressure_are_bounded`). For the fux
  ones, find the deadline each test uses, and raise only that harness-side deadline on hosted
  runners (for example read an env var such as `FUX_TEST_STARTUP_DEADLINE_MS` set in CI to a
  larger value, default unchanged) rather than loosening the product's own timeouts. Do not
  weaken assertions. If a test cannot be made deterministic that way, run it on Linux only and
  say so in the PR. zor's tests already run Linux-only; leave them.
- Prove locally: `cargo test -p fux --test local_cli`, the fixture-child suite, structure test,
  formatting and strict clippy. Push, open the PR, wait for hosted CI (green on every job,
  including macOS), report.

## 2. Release fux (and zor) from the workspace

Only after step 1's PR is merged by the user, or from `main` if the user merges it while you
work; otherwise prepare everything on a branch and report.

- Decide the version with the changelog: the native integration, the performance passes and
  the workspace move are one minor release (`0.7.0`) unless `CHANGELOG.md` says otherwise.
  Update `crates/fux/Cargo.toml` and `CHANGELOG.md` (dated entry summarizing PRs #4, #5, #6 and
  the release-relevant follow-ups). Give zor its own entry and version in `crates/zor` only if
  its changelog convention exists; otherwise leave zor at `0.1.2` and say so.
- Run the release-package check and `cargo package --locked -p fux` / `-p zor`; run
  `cargo publish --dry-run -p fux` and `-p zor` (dry-run only).
- Branch `release/0.7.0`, PR against `main` with the version bump and changelog. After the user
  merges it, create an annotated tag `v0.7.0` on the merge commit and a GitHub release whose
  notes are the changelog entry. If the repository has never had tags or releases, create the
  first one exactly the same way and say that it is the first.
- crates.io publication stays manual: report the exact `cargo publish` commands for fux and,
  if applicable, zor, and whether the dry runs passed.

## 3. Retire the koh patch workflow

The koh patch (`dependency-patches/koh.patch`) changes only two test files
(`src/gateway/sessions_real_fux.rs`, `tests/gateway.rs`) so koh's real-fux fixtures send
main's unversioned hello. The user owns koh.

- In a fresh clone of `gold-silver-copper/koh` at the pinned base
  (`af776a39ddea8826fe0915e712c787e303d5dbf0`), apply the patch, run koh's non-R6 gateway
  library tests and `cargo test --test gateway` with `FUX_BIN` pointing at a fux built from the
  tagged release (or `main` if step 2 is not merged yet), `KOH_REQUIRE_FUX_BIN=1`, and with
  `ZOR_BIN`/`KOH_REQUIRE_ZOR_BIN=1` for the test that needs zor. Push branch
  `fux-unversioned-hello` and open a koh PR with that evidence.
- In fux, once the koh PR is merged (or on a branch prepared against the koh PR's head commit,
  clearly labeled as waiting), point `dependency-patches/manifest.json` at the new koh base
  with an empty diff, then remove the patch mechanism for koh entirely: delete
  `dependency-patches/`, replace `dependencies apply/verify` in `tools/xtask` with a plain
  pinned clone at the recorded commit (keep the exact-HEAD check), update `checks.json`, the
  manual CI job, README, HANDOFF and docs. Keep the `references/koh` layout and the
  `FUX_BIN`/`ZOR_BIN` requirement flags so the koh integration can never silently skip.
- Add to koh's CI (in the koh PR, or a second one) a job that builds fux at the pinned release
  tag and runs koh's real-fux tests, so koh no longer depends on fux's repository for that
  coverage. Use the tag from step 2; if no tag exists yet, pin the fux commit and say so.

## 4. Archive the zor repository

Only after PR #6 is merged and `crates/zor` is on `main`: add a final commit to
`gold-silver-copper/zor` `main` that prepends a notice to its README ("moved to
`gold-silver-copper/fux` at `crates/zor`; this repository is archived and read-only") and then
archive the repository (`gh repo archive gold-silver-copper/zor --yes`). Record the zor `main`
commit before archiving. Do not delete tags or history.

## Verification and handoff

For every fux change: `cargo fmt --all --check`, strict `cargo clippy --workspace --all-targets
-- -D warnings`, the affected test suites, the tooling tests, and `cargo test -p fux --test
structure`. Use one independent subagent to review the koh-patch retirement diff (tooling and
CI) and the release PR (version/changelog/package contents). Write
`.verification/finish-migration/REPORT.md` with: the state of each step (done, prepared and
waiting on which merge, or blocked and why), every outward action taken with its URL or
commit, CI results, the crates.io commands left for the user, and the remaining follow-ups.
Do not merge anything; do not publish to crates.io.
