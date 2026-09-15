# Dependencies

Execution-time revisions (2026-09-15): rewrite branch `ecs-rewrite` created as an orphan from
`main` at `8b41296c65ad50c7fe05aa108b3a1276f619cfd5` (the prompt cites `d8b64bd2`; `main` moved
by prompt-only commits since). Bevy 0.19.1 from crates.io, matching the pinned checkout
`../many_rigs/inspirations/bevy` at `b56fc29d3016e641754765244b5ba3f9cc504671`.
Toolchain: rustc nightly 2026-02 (edition 2024, `rust-version = 1.95`).

The stack is the section-2 table of `docs/prompts/ecs-native-rewrite-prompt.md`, declared once
under `[workspace.dependencies]`. `bevy_camera`, `bevy_math` and `bevy_color` are listed
explicitly because fux names their types (`Camera`, `RenderTarget`, `UVec2`, `Color`); they were
already in the graph through `bevy_ui`.

## Accepted deviation: `bevy_render` is in the resolved graph (user-approved 2026-09-15)

Prompt section 2 sets the acceptance rule "no `wgpu`, `wgpu-hal`, `naga` in the resolved graph".
That rule cannot be met with `bevy_remote` 0.19.1 as published:

* `crates/bevy_remote/Cargo.toml:28-30` declares a **non-optional** dependency on
  `bevy_dev_tools` with feature `schedule_data` (used only by `schedule.list`/`schedule.graph`,
  `builtin_methods.rs:8`).
* `crates/bevy_dev_tools/Cargo.toml` declares non-optional `bevy_render`, `bevy_pbr`,
  `bevy_core_pipeline`, `bevy_ui_render`, `bevy_sprite_render`.

The prompt forbids forking Bevy crates and forbids `[patch]` vendoring, so the graph keeps
`bevy_render`/`wgpu`/`naga` through exactly that one edge. Nothing in fux or zor names a type from
those crates and no render plugin is added; the code compiles but is never used. The check
`cargo run --manifest-path tools/xtask/Cargo.toml -- deps` enforces the narrowed rule: every path
to a render crate goes through `bevy_remote -> bevy_dev_tools`, the only `bevy_render` dependants
are that stack, and neither binary has a direct dependency on tokio, ratatui,
tracing-subscriber, anyhow, bevy_render, bevy_winit, bevy_dev_tools, bevy_text or bevy_sprite.

Upstream fix to propose: make `bevy_dev_tools` optional in `bevy_remote` (feature
`schedule_data`), or make `bevy_render` optional in `bevy_dev_tools`. Until then the graph is
419 crates for fux and 402 for zor (`xtask deps` output, 2026-09-15).

`tokio` appears transitively (hyper's runtime traits); it is not a direct dependency and no
tokio runtime is started.

## Non-Bevy runtime dependencies

| crate | reason |
|---|---|
| `portable-pty`, `nix` | PTYs, process groups, signals; Bevy has no PTY layer |
| `vt100` | terminal emulation inside the `Terminal` component |
| `termina` | raw mode and output on the viewer's own terminal |
| `serde`, `serde_json`, `ron` | Bevy's serialization backends and BRP JSON |
| `clap`, `toml` | command line and user configuration |
| `async-channel` | the mailboxes between `IoTaskPool` tasks and the runner (already a `bevy_remote` dependency) |
| `unicode-width` | cell width of glyphs in the painter |

Forks (`fux-*` crates under `crates/`): none yet.
