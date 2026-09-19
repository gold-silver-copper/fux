# Dependencies

The workspace pins Bevy **0.19.1** from crates.io. The mechanism reference uses the matching
checkout `../many_rigs/inspirations/bevy` at
`b56fc29d3016e641754765244b5ba3f9cc504671` (see
[Bevy source patterns](bevy-source-patterns.md)). The manifests declare edition 2024 and
`rust-version = 1.95`; the actual verification toolchain and results belong in
[verification](verification.md).

The dependency stack is declared once under `[workspace.dependencies]` in
[Cargo.toml](../Cargo.toml), with package dependencies in the
[fux](../crates/fux/Cargo.toml) and [zor](../crates/zor/Cargo.toml) manifests. See
[design](design.md#native-bevy-mechanisms-not-a-second-framework) for the concrete relationships,
cloning, layout, assets, scenes, states and BRP mechanisms the products use. Dependency counts
are not evidence of ECS-native implementation.

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
those crates and no render plugin is added. The check
`cargo run --manifest-path tools/xtask/Cargo.toml -- deps` enforces the narrowed rule: paths
to a render crate must go through `bevy_remote -> bevy_dev_tools`, and direct render-stack
dependants must remain in the accepted stack. Both packages reject direct dependencies on
tokio, ratatui, tracing-subscriber, anyhow, bevy_render, bevy_winit, bevy_sprite,
bevy_animation and the umbrella bevy crate. The fux-only resource initialization exception
for bevy_text is described below; zor does not receive that exception. `bevy_dev_tools`
(its `schedule_data` module) is allowed by the recorded 2026-09-15 decision.

Upstream fix to propose: make `bevy_dev_tools` optional in `bevy_remote` (feature
`schedule_data`), or make `bevy_render` optional in `bevy_dev_tools`. Resolved graph counts and
gate outcomes are intentionally not maintained here; see the dated verification evidence.

`tokio` appears transitively (hyper's runtime traits); it is not a direct dependency and no
tokio runtime is started.

## Recorded exception: native UI resource initialization

Execution-time amendment 3 in the [rewrite prompt](prompts/ecs-native-rewrite-prompt.md)
permits direct `bevy_text` use in fux solely to initialize resources that native `UiPlugin`
systems require. This overrides the earlier blanket direct-dependency exclusion; it is not
permission for terminal text layout or rendering through Bevy. The concrete use is
[`layout::add_ui_stack`](../crates/fux/src/layout/mod.rs): font/image assets, font atlas,
text pipeline, layout/font/scale contexts, scratch and `RemSize`. `bevy_image` supplies the
corresponding image/atlas resource types. No `TextPlugin`, window integration or GPU renderer
is installed for terminal painting. `bevy_audio` is optional behind fux's `bell` feature.

## Non-Bevy runtime dependencies

| crate | reason |
|---|---|
| `portable-pty`, `nix`, `filedescriptor` | PTYs, safe descriptor duplication, process groups and signals; Bevy has no PTY layer |
| `vt100` | terminal emulation inside the `Terminal` component |
| `termina` | raw mode and output on the viewer's own terminal |
| `serde`, `serde_json`, `ron` | Bevy's serialization backends and BRP JSON |
| `clap`, `toml` | command line and user configuration |
| `async-channel`, `async-io` | bounded adapter mailboxes and nonblocking I/O/timers on Bevy task pools |
| `hyper`, `smol-hyper`, `http-body-util` | fux's bounded BRP acceptor (`remote/http.rs`): `RemoteHttpPlugin` buffers bodies unbounded, caps nothing and loses its listener on `EMFILE` (`docs/verification.md`, "BRP resource exhaustion"); the same three crates and versions `bevy_remote`'s transport already pulls in |
| `unicode-width` | cell width of glyphs in the painter |
| `regex` | zor's passive observation rule bundles are regex-gated (`rules/*.toml`) |

No forked Bevy crate is part of the workspace. The dependency policy is checked by
[`tools/xtask/src/main.rs`](../tools/xtask/src/main.rs); this document does not assert that
the current gate or benchmark run passed.
