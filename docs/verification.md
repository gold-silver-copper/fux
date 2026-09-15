# Verification record

Dated records per milestone (prompt section 6). Each entry names the commands run and the
observed result; blockers are recorded where they were found.

## 2026-09-15 — Milestone 1: workspace skeleton

* `cargo check --workspace`: fux and zor compile against the section-2 stack (cold check 3m38s on
  Apple M2 Max). No `DefaultPlugins`, no `bevy_winit`.
* `cargo run --manifest-path tools/xtask/Cargo.toml -- deps`: passes with the narrowed rule
  documented in `docs/dependencies.md`.
* CI workflow `.github/workflows/ci.yml`: fmt, clippy, build, test, doc, package, deps report.

### Accepted deviation: render crates in the graph (user-approved 2026-09-15)

`bevy_remote` 0.19.1 hard-depends on `bevy_dev_tools`, which hard-depends on `bevy_render`
(details and the enforced replacement rule in `docs/dependencies.md`). The prompt's "no wgpu in
the resolved graph" acceptance rule contradicts Bevy source and neither the precedence rule of
section 1 nor the forking policy of section 2 resolves it (Bevy crates are never forked). The user accepted wgpu in the graph. Decision
taken: keep `bevy_remote` as published, never use render types, enforce the narrowed rule in
`xtask deps`, and propose the upstream change. No milestone task depends on this beyond the
report itself.

Pre-existing `ecs-rewrite` branch (an older, fully merged attempt at 0.3.0, tip `4265ce5`) was
renamed to `ecs-rewrite-2026-09-05-merged` so the orphan branch could take the prompt's name.

## 2026-09-15 — Milestone 2: fux App shell

Commands (results filled in at the milestone commit):

* `cargo check --workspace` / `cargo clippy --workspace --all-targets -- -D warnings`: result.
* `cargo test -p fux --test lifecycle`: World-only pane/viewer lifecycle through
  `app::build_headless` (one `SpawnPane` per pane, `PaneSpawned` lifts `Disabled`, exit →
  `FinalRecord` + leaf removal + retarget, exact viewer detaches on target loss, split queues
  input until the new pane is live, `ViewerGone` detaches, workspace retires on its last exit;
  `check_invariants` after every update): result.
* `cargo test -p fux --test shutdown`: `Signal` → `Terminate` for every live pane → `Exit`
  after both exited; the 5 s `Clock` deadline exits regardless; late spawns are terminated:
  result.
* `cargo test -p fux --lib`: unit tests of `paths`, `config` and the other modules: result.
* `cargo test -p fux --test layout_mechanism` and the layout/terminal/remote/attach suites:
  result.
* Smoke: `XDG_RUNTIME_DIR=$(mktemp -d) cargo run -p fux -- serve --name t-$RANDOM`, observe
  `<runtime>/fux/t-*.brp.json` written; `kill -TERM <pid>` exits within 6 s with status 0:
  result.
* Smoke: `fux` (no args) starts the `default` server detached, attaches a viewer; `C-b %`,
  `C-b "`, `C-b x`, `C-b d` behave; a second `fux` attaches to the same root at another size:
  result.
