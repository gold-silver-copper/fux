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

Commit `0e49c4b`. 14.7k lines in `crates/fux/src`. Results:

* `cargo check --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`: clean
  (`type_complexity` allowed workspace-wide; test files carry a crate-level allow for
  unwrap/expect/panic/indexing in helpers, since `clippy.toml` only relaxes `#[test]` bodies).
* `cargo test -p fux`: 80 tests green across `--lib` (27), `attach` (7), `brp` (4),
  `layout_mechanism` (2), `layout_ops` (14), `layout_props` (1 proptest: random template edits
  interleaved with attach/detach/show/resize/zoom, `check_invariants` + instance==template after
  every update), `lifecycle` (6), `pty_adapter` (5), `shutdown` (3), `viewer` (11).
* Smoke (real processes, disposable XDG dirs, hub-supervised `fux serve --name smoke`):
  `<runtime>/fux/smoke.brp.json` written 0600 with http/attach ports and tokens;
  `fux --server smoke fux/server.info`, `fux/workspace.list`, `world.query` over `PaneView`
  answer; `world.query` over `fux::model::components::Process` is refused (-32002);
  `rpc.discover` lists exactly the allowlist (32 methods, no `world.*` mutators).
* Smoke (viewer over a Python `pty.fork`, 24x80): attach shows the shell; `echo` echoes;
  `C-b %` splits and the second pane goes starting → live and receives input; `C-b d`
  detaches with exit status 0 (fixed during integration: a `Bye` and the socket EOF in the same
  batch used to let the EOF win, `viewer/mod.rs`).
* Smoke (two viewers, 24x80 and 40x120, one root): `fux/workspace.list` shows 2 viewers and
  every pane's size folded to the minimum (23 rows: 24 minus the status bar; 16 cols across
  five panes); exact attach `--pane 1 --pid <pid>` attaches, `--pid 1` is refused with
  `Refused: pane pid mismatch`; all viewers detached cleanly.
* Smoke (shutdown): `kill -TERM` with five live shells: server exited within 1 s with status 0,
  descriptor removed, no orphaned shells.

Recorded deviations from the prompt (all in code comments / HANDOFF):
* `SpawnPane` is emitted in `PostUpdate` after the size fold (so the PTY opens at the laid-out
  size), not in `Requests`.
* Close ownership: lifecycle marks viewers `Detaching`; the attachment projection sends `Bye`
  with the reason derived from world state and emits `Effect::CloseViewer`.
* Frames are JSON (`serde_json`) rather than a binary row format; per-cell `String`s are the
  known allocation on the frame path, to be measured in milestone 8.
* `bevy_remote` requests are forwarded from `BrpReceiver` into a fux mailbox with
  `Inbound::Wake` so the runner sleeps with no polling; dispatch runs in `RemoteLast` moved
  after `First`.
