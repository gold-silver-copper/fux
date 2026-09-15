# Verification record

Dated records per milestone (prompt section 6). Each entry names the commands run and the
observed result; blockers are recorded where they were found.

## 2026-09-15 — Milestone 1: workspace skeleton

* `cargo check --workspace`: fux and zor compile against the section-2 stack (cold check 3m38s on
  Apple M2 Max). No `DefaultPlugins`, no `bevy_winit`.
* `cargo run --manifest-path tools/xtask/Cargo.toml -- deps`: passes with the narrowed rule
  documented in `docs/dependencies.md`.
* CI workflow `.github/workflows/ci.yml`: fmt, clippy, build, test, doc, package, deps report.

### Blocker recorded (section 7): render crates in the graph

`bevy_remote` 0.19.1 hard-depends on `bevy_dev_tools`, which hard-depends on `bevy_render`
(details and the enforced replacement rule in `docs/dependencies.md`). The prompt's "no wgpu in
the resolved graph" acceptance rule contradicts Bevy source and neither the precedence rule of
section 1 nor the forking policy of section 2 resolves it (Bevy crates are never forked). Decision
taken: keep `bevy_remote` as published, never use render types, enforce the narrowed rule in
`xtask deps`, and propose the upstream change. No milestone task depends on this beyond the
report itself.

Pre-existing `ecs-rewrite` branch (an older, fully merged attempt at 0.3.0, tip `4265ce5`) was
renamed to `ecs-rewrite-2026-09-05-merged` so the orphan branch could take the prompt's name.
