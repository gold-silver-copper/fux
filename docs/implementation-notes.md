# Implementation notes

Phase 0 was checked on 2026-09-03 against koh 0.11.0. The completed implementation now requires
koh 0.12.1 at reviewed follow-up commit `df81a96f0d2483095c5ac6b26ffe9aa6b669fce3`
(PR #17 and registry publication are still pending), herdr at
`94f6d9c0d9bb9cf9ffae99d8bbfb09e9bf2fc9e0`, and zellij at
`af38660c5884f50bb3726682fb92961326c4268f`.

- koh's public clap-free adapters are `client::{run_id, connect, IdConfig, ConnectConfig}` and
  `keycmd::{run, KeyConfig, KeyOp}`. fux retains its own clap types because koh's `cli` feature is
  disabled.
- `SessionHost` mutates through `&mut self`; `snapshot` is synchronous, while `stamp_echo_ack`
  changes only the returned snapshot. PTY drains stay outside koh's outer session lock and notify
  through `ChangeSignal`.
- `HostProvider` selects a `SharedHost` by ALPN, while `serve_with` owns endpoint policy. Named
  workspaces need one fux-owned endpoint/router policy rather than a koh modification.
- `ClientState::predict_target` exposes a borrowed `ScreenView`; invalid or copy-mode targets
  return `None`. `ClientTerminal` owns terminal setup, rendering, input, resize, and restoration.
- `ServerTerminal::with_scrollback_screen` temporarily moves the live viewport inside a
  panic-safe callback, avoiding a full-history clone; callers still bound rows and output bytes
  before materializing capture text.
- koh's `connect_with` separates state/terminal factories and input/resize receivers. The Phase F3
  fux client uses it with koh's cancellation-aware public client-I/O producers.
- koh owns PTYs through public `Pty`, while backend and callback ledgers needed by fux are private;
  fux supplies those integrations itself.
- **3 Sep 2026 — zor integration:** the wrapper described by `wrapper-design.md` is implemented as
  zor 0.1.2; fux consumes its OSC schema and production hosts execute the configured zor binary.
- **3 Sep 2026 — shared copy viewport:** copy/scroll viewport and selection state are shared between
  viewers in v1. koh supplies `ClientId` for resize and detach callbacks, but not for input, so fux
  cannot safely maintain viewer-local copy state without extending the transport contract.
