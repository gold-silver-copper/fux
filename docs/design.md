# The Shape of fux

A zellij-based agent workspace with a mosh-grade peer-to-peer attach path, shipped as one
`cargo install`-able binary.

- **Status:** proposal, audited against source
- **Date:** 2 Sep 2026; open questions and checklist re-audited 3 Sep 2026; citations re-audited against
  koh 0.10.0 the same day
- **Reference trees:** `references/` — zellij `main` at 0.46.0 (af38660, 31 Aug 2026) ·
  koh 0.10.0 (fa637c2) · herdr 0.8.2 (94f6d9c, github.com/herdrdev/herdr)
- **Published:** zellij / zellij-client / zellij-server / zellij-utils **0.45.1** (crates.io, 28 Aug
  2026) · koh **0.10.0** (crates.io, 3 Sep 2026; fux depends on it) · fux 0.1.0 placeholder. The `herdr` crate on crates.io (0.1.0) is an
  unrelated project; herdr proper is not published.

---

## The read: three goals, three different answers

fux wants to be a better herdr, carry koh's transport, and live inside zellij. Those three sit at
different levels of the stack, and conflating them is the main way this design goes wrong.

| | Verdict | Why |
|---|---|---|
| **Agent state** | Fits the plugin | The wasm sandbox already receives rendered pane text on every redraw. herdr's detection model ports onto it directly. |
| **Notifications** | Either side; native by default | Plugins can `run_command`, so `notify-send` is reachable from wasm. But phone-side alerts need the transport, which is native. |
| **iroh peer-to-peer** | Cannot fit the plugin | Plugins run on wasmi under WASI preview 1 — preopened dirs, env and stdio, and no socket layer at all. QUIC has nowhere to bind. |
| **Attach transport** | Belongs to the binary | koh already hosts an arbitrary command in a pty and ships screen diffs. The zellij client is that command. |

---

## Architecture: one binary, four layers, one sandbox boundary

Because fux *is* the binary rather than a plugin someone installs into zellij, the iroh transport
becomes ordinary in-process Rust. There is no sidecar to launch, supervise, or version-match. The
only real boundary in the system is the wasm sandbox, and only one concern lives on the far side of
it.

```
  phone                      DESKTOP — ONE NATIVE PROCESS
  ┌──────────┐   iroh    ┌───────────────────────────────────────────┐
  │ termux   │  ──QUIC─> │ fux                                       │
  │ fux      │  screen   │                                           │
  │ connect  │  diffs    │  ┌─────────────────┐   ┌ ─ ─ ─ ─ ─ ─ ─ ┐  │
  └──────────┘           │  │ koh server      │                     │
                         │  │ SSP · vt100     │   │  WASMI SANDBOX│  │
                         │  └────────┬────────┘                     │
                         │      pty  │ runs     │   fux.wasm    │  │
                         │  ┌────────┴────────┐      detection      │
                         │  │ zellij client   │   │  UI          │  │
                         │  │   │ unix sock   │                     │
                         │  │ zellij server   │ <─┤  reads panes  │  │
                         │  └─────────────────┘   │  runs cmds    │  │
                         │                           no sockets     │
                         │  ┌─────────────────┐   │  16 MB/memory │  │
                         │  │ notifier        │ <─┐                  │
                         │  │ notify-send ·   │  pipe: state      │  │
                         │  │ osascript       │   └ ─ ─ ─ ─ ─ ─ ─ ┘  │
                         │  └─────────────────┘                      │
                         └───────────────────────────────────────────┘
                                        ^
                                        │  embedded assets
                                        │  include_bytes!(fux.wasm)
                                        │  + default layout
                                        │  unpacked to cache dir
```

The dashed boundary is the only process-level constraint in the design. Everything outside it is
ordinary native Rust in a single binary; everything inside it is an interpreted wasm module that can
read pane text, draw, and spawn host commands through the server, and nothing else.

| Layer | Runs in | Built from |
|---|---|---|
| **Transport** | native | koh's iroh endpoint, SSP loop, and server-side vt100 emulator, unchanged. Its pty runs `fux` (the local attach path) instead of a login shell. Identity, allow-lists and key handling come over as-is. |
| **Workspace** | native | `zellij-client` and `zellij-server` as library dependencies, plus fux's own CLI dispatch, default layout, and config defaults. |
| **Agent state** | wasm | `Event::PaneRenderReport` feeding a port of herdr's TOML rule manifests. State published back over the plugin pipe API. |
| **Notifications** | native | fux consumes state transitions from the pipe and calls the platform notifier. See the open question on phone-side alerts. |

### The transport bridge is a command line, not a bridge

koh's server does not forward bytes. It allocates a pty, spawns a command in it (`--shell`, defaulting
to the login shell), runs a vt100 emulator over the output, and ships screen diffs to the client over
SSP. That is what makes it mosh-grade: the client renders state, not a byte stream, so a dropped
link costs nothing on resume.

The earlier draft proposed re-terminating that loop in the zellij IPC socket. That is the wrong
layer: the zellij client is the thing that turns IPC messages into terminal output, and koh needs
terminal output. So `fux serve` is `koh serve --shell "fux"`, in-process. The remote peer sees
exactly what a local `fux` invocation sees, the zellij server keeps its session alive across
disconnects the way it already does, and koh's session retention (24 h default) layers on top.

Cost: one extra vt100 emulation pass per frame on the desktop, plus screen-diff bandwidth. That is
the cost mosh users already pay to run tmux over mosh, and it is the whole reason the design works
without touching koh's SSP loop.

---

## Distribution: why `cargo install fux` needs no wasm toolchain

This is the part that looks impossible and isn't. zellij solves it by committing its built plugins
to git: `zellij-utils/assets/plugins/*.wasm` are tracked files, 1.2–1.8 MB each, pulled into the
binary by `add_plugin!` into `ASSET_MAP` and written out to the plugin directory by `setup.rs` on
first run.

fux does the same with a single artifact. The wasm target is a CI concern, never a user concern.

1. **CI builds `fux.wasm`** for `wasm32-wasip1` in release mode (the target zellij's `xtask` and
   `rust-toolchain.toml` already use). fux is one crate, so the plugin is a second `[[bin]]` target
   (`fux-plugin`, `required-features = ["plugin"]`) whose `main.rs` calls `register_plugin!`,
   exactly how zellij's own `default-plugins/*` are laid out. The `plugin` feature pulls in
   `zellij-tile` and nothing native; the build is
   `cargo build --bin fux-plugin --target wasm32-wasip1 --release --no-default-features --features plugin`.
2. **The artifact is committed** to `assets/plugins/fux.wasm` and listed in the crate's `include`
   key so it ships inside the published `.crate`.
3. **The binary embeds it** with `include_bytes!`, alongside the default fux layout that references
   it.
4. **First run unpacks it** to the cache directory and the layout loads it by `file:` URL. The user
   installs one crate and gets one binary.

---

## Surface: what the binary does

Argv dispatch over one executable. The local-workspace path and the remote path share an identity
store and a session model, which is the whole reason for fusing them rather than shipping koh and a
plugin separately.

```sh
fux                        # attach or create a local session
fux id                     # print this machine's endpoint id
fux serve --allow <id>     # expose this workspace to an allowed peer (koh serve, shell = fux)
fux connect <id>           # attach to a remote workspace, mosh-style (koh connect)
fux key passwd | info      # identity key management (koh key)
```

`id`, `key`, `connect` and `serve` are koh's four stable entry points (`run_id(IdConfig)`,
`keycmd::run(KeyConfig)`, `connect(ConnectConfig)`, `serve(ServeConfig)`) behind fux's own clap
layer; koh's `cli` feature is off, so fux owns argv parsing for all of them. `fux serve` fills
`ServeConfig.command` with `std::env::current_exe()` and leaves `session_ttl_secs` at koh's
24 h default. Only the bare invocation and the flag definitions are new code.

The top-level zellij binary is 414 lines of `main.rs` over 1,065 lines of `commands.rs`, sitting on
real library crates (`start_client`, `start_server`, `start_server_detached`). fux's workspace layer
is that shape of code — dispatch and defaults — not a reimplementation and not a fork.

---

## Detection: herdr's model, on zellij's event

`Event::PaneRenderReport` delivers, per client, a map of pane id to `PaneContents` — the viewport
plus lines above and below it — in both plain and ANSI form, for every pane that redrew. It requires
the `ReadPaneContents` permission. That is precisely the input herdr's detectors consume, and it
arrives without polling.

herdr classifies each pane as **working**, **blocked**, or **idle** using per-agent TOML manifests
(21 of them, from `amp.toml` to `qwen.toml`) of prioritised regex rules scoped to named regions.
The vocabulary is twelve fixed regions (`whole_recent`, `osc_title`, `osc_progress`,
`prompt_box_body`, `above_prompt_box`, `last_non_empty_above_prompt_box`,
`after_last_horizontal_rule`, and the prompt-marker family) plus the parameterised
`bottom_lines(n)` / `bottom_non_empty_lines(n)`. The Claude manifest uses nine of them and carries
rules keyed on the braille spinner range, the `esc to interrupt` footer, OSC 9;4 progress, and a set
of negative guards so that a user typing "do you want to proceed?" cannot impersonate a state
change.

The manifests are data and port verbatim. The evaluator is not small: `detect/manifest.rs` is 1.5k
lines and `detect/mod.rs` 1.6k, plus 1k for manifest updates that fux does not need. Budget a real
port of the region vocabulary and rule matcher, not a weekend. Agent identification by process name
(`identify_agent`) maps onto `get_pane_running_command` and `get_pane_pid`; `get_pane_scrollback`
fills in what a rule needs beyond the visible frame.

Where fux can beat herdr is the debounce. herdr holds a working-to-idle transition across three
confirmations at 100 ms, capped at 700 ms, with a 3 s startup grace window, precisely because a
spinner blinking between frames would otherwise flap the state. Inheriting that hysteresis from the
start, rather than rediscovering it, is most of the difference between a status indicator people
trust and one they learn to ignore.

---

## Licensing: koh relicensed, fux MIT

| Component | License |
|---|---|
| zellij | MIT |
| herdr | Apache-2.0 |
| koh | **MIT** as of 0.10.0 (was GPL-3.0-or-later through 0.9.1; same author as fux) |
| fux | MIT |

Linking koh as a library makes fux a derivative work, so koh's license had to be compatible before
fux publishes anything past the placeholder. koh and fux share an author, so koh was relicensed to
MIT rather than moving fux to GPL. Done: koh 0.10.0 on crates.io carries `license = "MIT"`, and
fux's `Cargo.toml` depends on that release.

Porting herdr's manifests and matcher under Apache-2.0 into an MIT crate is fine with attribution
and a NOTICE entry.

---

## Risks: what could make this wrong

### zellij's library crates are published, but as internals — *pin exactly*

Verified: `zellij-client`, `zellij-server` and `zellij-utils` 0.45.1 are on crates.io (28 Aug 2026),
and 0.45.1 already carries `PaneRenderReport`, `PaneContents` and wasmi 1.1 at the same source lines
as `main`. 0.46.0 is unreleased. The crates exist to serve one binary, and nothing constrains their
internals across minor releases.

**Mitigation.** Pin `=0.45.1`, the way koh pins `iroh = "=1.0.0"` for the same reason. Keep fux's
contact surface with those crates to `start_client`, `start_server`, `start_server_detached`, and the
config and layout types, so a re-point at a fork is an afternoon.

### wasmi is an interpreter — *design around it*

zellij runs plugins on wasmi 1.1, not a JIT, with one instance, up to four linear memories and a
16 MB ceiling per memory, and a trap on failed grow. herdr's detection loop is native code running on
every frame; the same work interpreted, on every `PaneRenderReport`, across every pane, is a
different cost profile.

**Mitigation.** Evaluate rules against the bottom-*n* non-empty lines and the OSC title only, never
full scrollback; short-circuit on priority order; debounce before matching, not after. If it ever
gets expensive enough to feel, that is the signal to fork and move detection server-side — not
before.

### The plugin cannot reach the network — *accepted*

The WASI context is built with inherited env and four preopened directories — `/host`, `/data`,
`/cache`, `/tmp` — and nothing else. The single egress path in the whole plugin API is `web_request`,
which is HTTP only and dispatched host-side.

**Mitigation.** None needed under this design; the transport was never going to live there. Worth
stating explicitly so it is not rediscovered as a blocker later. The preopened dirs give a file
channel to the native side, and `run_command` (under `RunCommands`) gives a process channel, in
addition to the pipe API.

### Termux is a build target, not just a run target — *scope it*

Only the client half lives on the phone, so nothing wasm has to compile there. But zellij's client
and fux's transport still have to build under Termux's toolchain. koh's install notes require
`pkg install rust clang pkg-config`, and the toolchain floor is now zellij's `rust-version = "1.95"`,
not koh's 1.91.

**Mitigation.** Treat prebuilt aarch64-Android binaries as the primary phone distribution and
`cargo install` as the fallback, rather than the reverse. The phone needs only `connect`, `id` and
`key`, so gate the zellij crates and the embedded plugin behind a default `workspace` Cargo feature.
`cargo install fux --no-default-features` then builds the client-only binary on Termux from the same
crate, without dragging zellij's dependency tree through Android's toolchain.

---

## Decision: depend, don't fork — until you need a host function

Everything above is reachable with upstream zellij as a dependency. The single case that forces a
fork is needing a capability the sandbox cannot express: a new host function, a native notification
API, or detection running inside the server for speed.

Hold that in reserve. A fork is cheap to start and expensive forever — every upstream release becomes
a merge. The narrow dependency surface recommended above is what keeps the option open at roughly the
cost of an afternoon, instead of a rewrite.

---

## Read from source

All paths relative to `references/`.

- `zellij/zellij-server/src/plugins/plugin_loader.rs:420` — `create_wasi_ctx`: `inherit_env`, four
  preopened dirs
- `zellij/zellij-server/src/plugins/plugin_loader.rs:479` — store limits: 1 instance, 4 memories,
  16 MB each, 16 tables, trap on grow failure
- `zellij/zellij-utils/src/consts.rs:143`, `:179` — `add_plugin!` / `ASSET_MAP` embedding;
  `zellij-utils/src/setup.rs:210` — `dump_builtin_plugins`, the first-run unpack
- `zellij/zellij-utils/src/data.rs:1022`, `:2452`, `:2489` — `PaneRenderReport`, `PaneContents`
- `zellij/zellij-server/src/plugins/wasm_bridge.rs:2104` — `PaneRenderReport` gated on
  `ReadPaneContents`
- `zellij/zellij-utils/src/plugin_api/plugin_permission.proto` — the 17 permission types
- `zellij/zellij-tile/src/shim.rs:829`, `:1890`, `:1954`, `:2009` — `run_command`,
  `get_pane_scrollback`, `get_pane_pid`, `get_pane_running_command`
- `zellij/zellij-server/src/plugins/zellij_exports.rs:2566` — `web_request`, the sole network egress
- `zellij/zellij-utils/src/consts.rs:336` — `ZELLIJ_SOCK_DIR`, the client/server link
- `zellij/zellij-client/src/lib.rs:938`, `:1638`; `zellij-server/src/lib.rs:863` — `start_client`,
  `start_server_detached`, `start_server`
- `zellij/xtask/src/build.rs:199`, `rust-toolchain.toml` — `wasm32-wasip1` plugin target
- `koh/src/server/cli.rs:45`, `:54`, `:74` — `ServeConfig`; its `command` argv field; `DEFAULT_SESSION_TTL_SECS = 86_400`
- `koh/src/server/cli.rs:115`, `:188`, `:208` — repeatable `--shell` (`cli` feature); `serve` entry point; empty `--allow` is rejected
- `koh/src/client/cli.rs:27`, `:57`; `keycmd.rs:31` — `ConnectConfig`, `IdConfig`, `KeyConfig`
- `koh/src/main.rs:58` — koh's own binary is dispatch over `koh::server::serve`, `client::connect`, `run_id`, `keycmd::run`
- `koh/src/lib.rs:42` — koh's documented stable surface: the four config types and entry points plus `ssp`
- `koh/src/pty.rs:138` — `Pty::spawn` takes argv: `command[0]` is the program, the rest are arguments verbatim, no `sh -c`
- `koh/src/server/session.rs:59`, `:63` — `spawn_session`: pty + vt100 emulator + drain task; `TERM=xterm-256color`
- `koh/src/client/render.rs:246` — client rings the local bell when the server's bell count climbs
- `koh/src/terminal/mod.rs:21`, `:133`, `:142` — bell and OSC 2 title carried out-of-band over SSP
- `koh/src/terminal/server.rs:107` — `ServerTerminal`, the server-side screen model
- `koh/Cargo.toml` — MIT (0.10.0), pinned `iroh = "=1.0.0"`, `vt100 = "=0.16.2"`, 1.91 floor; `cli` feature
  owns clap and gates the binary, so `default-features = false` is the library tree
- `zellij/zellij-utils/src/input/permission.rs:13`, `consts.rs:104` — `PermissionCache`: granted permissions
  persist as `permissions.kdl` in the cache dir, keyed by plugin name
- `zellij/zellij-tile/src/shim.rs:1653`, `:1661` — `cli_pipe_output`, `pipe_message_to_plugin`: the plugin
  pipe API in both directions; `zellij-utils/src/cli.rs:678` — the `pipe` CLI action
- `zellij/zellij-client/src/lib.rs:578`, `:1394` — synchronized output is enabled only when `TERM=alacritty`
- `herdr/src/detect/manifests/claude.toml` — rule regions, priorities, negative guards
- `herdr/src/detect/manifest.rs:1104` — `validate_region_name`, the region vocabulary
- `herdr/src/pane/agent_detection.rs:5` — idle confirmation hysteresis constants
- `herdr/src/platform/linux.rs:554`, `macos.rs:547`, `:643` — `notify-send`; `terminal-notifier` first, `osascript` fallback

---

## Open questions

Everything still undecided, in the order it blocks work. Two items that were open in the first
draft are now answered from source and moved to *Settled by audit* below.

1. **Does zellij's client render cleanly under koh's vt100 grid?** koh advertises
   `TERM=xterm-256color`; zellij's client probes for kitty keyboard support and answers DSR/DA/DECRQM
   queries through `session.rs:105`. Synchronized output is off under any `TERM` other than
   `alacritty`, so that probe is moot. This is the only risk that can sink the design, and it is
   testable today with no fux code. koh 0.10.0 takes the hosted command as argv, so no wrapper
   script is needed; `--allow` is mandatory, since `koh serve` refuses an empty allow-list:

   ```sh
   koh serve --allow <phone-id> --shell zellij --shell attach --shell -c --shell main
   ```

   Exercise from the phone: resize, split panes, scroll, a full-screen app inside a pane, detach
   and reattach. fux itself does the same thing in-process: `fux serve` fills
   `ServeConfig.command` with its own executable path, and bare `fux` is the attach path.
2. **Phone-side notifications.** The phone only receives screen diffs. koh's SSP already carries
   the bell and OSC 2 title out-of-band, and koh's client already rings the local terminal on a
   bell-count increase, so the cheapest path is: the plugin rings the bell or sets a pane title,
   koh's client (or fux's client feature) maps that to `termux-notification`. Alternatives are a
   dedicated state frame in SSP, or desktop-only notifications for v1. Decide before the plugin's
   pipe protocol is designed, since the bell/title option changes what the plugin publishes.
   Caveat: zellij's own bell handling in a pane must propagate to the outer terminal for this to
   work; verify during (1).
3. **Desktop notifications: plugin or native?** The plugin can call `notify-send` / `osascript`
   through `run_command`, which removes the native notifier and the pipe protocol from v1. Keeping
   them native only pays off once (2) needs a state channel anyway. Recommendation: plugin-side for
   v1, revisit with (2).
4. **Pipe protocol shape.** If (3) goes native or (2) needs a state stream, what the plugin
   publishes per transition: pane id, agent, old state, new state, timestamp, and a short title
   line. The transport is already there: `cli_pipe_output` on the plugin side and the `pipe` CLI
   action on the native side, both in 0.45.1. Only the payload is undesigned.
5. **Bare `fux` semantics.** Attach to the most recent session or create `main`? Does it honour
   `ZELLIJ_SESSION_NAME` and zellij's own config dir, or keep a separate `~/.config/fux`? Note that
   the permission cache (see *Settled*) lives in zellij's cache dir, so a separate config dir still
   shares state with any plain zellij install on the machine unless fux also overrides the cache dir.
6. **Rule evaluator: port or rewrite?** herdr's evaluator is 3.2k lines (`manifest.rs` 1.5k,
   `mod.rs` 1.6k) plus 1k of manifest-update code fux does not need. The manifests are the value; the
   region vocabulary is a dozen regions plus two parameterised ones, and a priority loop. A fresh
   evaluator under the 16 MB wasmi
   cap, validated against herdr's manifests with captured-pane fixtures, is the recommendation.
   Needs a decision before the plugin target starts.
7. **wasmi cost in practice.** Interpreted regex over the bottom-*n* lines of every redrawn pane.
   No measurement yet. Measure with the fixture set before optimising.
8. **Plugin permissions surface.** `ReadPaneContents` is required. `RunCommands` if (3) is
   plugin-side. `ReadApplicationState` for pane and tab metadata. Confirm the set. Pre-granting is
   solved (see *Settled*).
9. **Version pins.** `zellij-* = "=0.45.1"` now; 0.46.0 when it publishes. `iroh = "=1.0.0"` via
   koh. Toolchain floor becomes 1.95. The Termux question is settled below.
10. **Termux build of the client-only feature set.** `cargo install fux --no-default-features`
    should need only koh's dependency tree. koh 0.10.0 is published and fux's `default-features =
    false` tree is clap-free on the desktop; the Android build itself is still unverified.

### Settled by audit

- **Termux's toolchain clears the floor.** termux-packages ships `rust` 1.98.0 on `master`
  (`packages/rust/build.sh`), above zellij's 1.95 and koh's 1.91. No install-note change needed
  beyond stating the floor.
- **Permissions can be pre-granted.** zellij persists granted permissions in
  `$ZELLIJ_CACHE_DIR/permissions.kdl` keyed by plugin name (`PermissionCache`). fux's first-run
  unpack, which already writes `fux.wasm` to the cache dir, can write that entry alongside it, so
  the user is never prompted. This is a cache-format dependency; pin it with the crates.
- **koh's stable API is already declared.** `koh/src/lib.rs` names `server::serve`,
  `client::connect`, `client::run_id`, `keycmd::run` and the `ssp` core as the supported surface
  and everything else as unstable. fux depends on exactly that set. As of 0.10.0 each entry point
  takes a plain config type with public fields (`ServeConfig`, `ConnectConfig`, `IdConfig`,
  `KeyConfig`); the clap `*Args` structs live behind the `cli` feature, which fux turns off.
  `ServeConfig` and `IdConfig` implement `Default`; `ConnectConfig` is built with
  `ConnectConfig::new(server)`. `ServeConfig.command` is argv, never shell-split.

---

## Your side: koh and everything else outside this repo

Work that has to happen in repos other than fux, or in accounts only you control.

### koh

- [x] **Relicense to MIT.** Done in koh PR #13 (merged 3 Sep 2026).
- [x] **Cut a koh release** carrying the new license. koh 0.10.0 published 3 Sep 2026, tagged
      `v0.10.0`; fux pins `koh = "=0.10.0"` with `default-features = false` and `backend-termina`.
- [ ] **Run the vt100 compatibility test** (open question 1) from a phone with the repeatable
      `--shell`. Fix whatever zellij's client does that koh's emulator mishandles.
- [x] **Let `--shell` take a command line.** 0.10.0 makes `--shell` repeatable (one argv element
      per flag, never whitespace-split), backed by `ServeConfig.command`.
- [ ] **Decide the phone notification channel** (open question 2). If bell/title: make koh's client
      call `termux-notification` on bell or on a title change matching a pattern. If a state stream:
      add a frame type to SSP.
- [x] **Add a `cli` Cargo feature so fux can call koh without clap.** Shipped in 0.10.0 as
      specified below (kept for the record):
      - `cli` feature, on by default, owns the `clap` dependency; the `koh` binary gets
        `required-features = ["cli"]`. `cargo install koh` is unchanged.
      - Each entry point takes a plain config type with public fields: `serve(ServeConfig)`,
        `connect(ConnectConfig)`, `run_id(IdConfig)`, `keycmd::run(KeyConfig)`. The existing
        `*Args` structs stay, gated on `cli`, with `From<ServeArgs> for ServeConfig` and so on.
        The config types replace the clap structs as the documented stable surface in `lib.rs`.
      - `ServeConfig` carries `command: Vec<String>` (argv), so fux passes its own executable
        directly.
      - The QR renderer and the passphrase prompt stay in core, not `cli`; fux wants `id` and
        `key` too. Helpers reachable only from the clap structs get `#[cfg(feature = "cli")]` so
        `dead_code = "deny"` holds with the feature off.
      - fux depends on koh with `default-features = false`.
      CI now checks the library-only tree with clippy and fails if clap reappears in it.

### herdr

- [ ] **Attribution.** Copy herdr's Apache-2.0 LICENSE text into a NOTICE entry in fux before the
      manifests land. No upstream action needed.
- [ ] Optional: **open an issue or PR** if fux finds manifest bugs, so the fixture set stays shared.

### crates.io and accounts

- [ ] **`fux` is already yours** (0.1.0 placeholder, 18 Aug 2026). Nothing to reserve. The
      `herdr` name on crates.io belongs to an unrelated 0.1.0; irrelevant unless you want to
      publish the manifests separately.
- [ ] **Decide the `fux` repo's public remote.** The `references/` trees are gitignored; CI needs
      them as git submodules, subtrees, or plain crates.io dependencies. Recommendation: crates.io
      deps for koh and zellij, and only herdr's `manifests/` directory vendored. herdr is not on
      crates.io, so the manifests have to be vendored either way.
- [ ] **CI runner with `wasm32-wasip1`** for building `fux.wasm`, and an `aarch64-linux-android`
      cross target for the prebuilt phone binary. Pin the toolchain to 1.95.0 to match zellij's
      `rust-toolchain.toml`.

### Decisions already made

- fux is one crate with two bin targets: `fux` (native, default `workspace` feature) and `fux-plugin`
  (`plugin` feature, built for `wasm32-wasip1` in CI and committed). The phone build is
  `--no-default-features`.
- fux stays MIT; koh relicensed to MIT in 0.10.0.
- Depend on upstream zellij, pinned exactly; fork only when a host function is needed.
- Depend on koh as a library, not an extraction: nearly all of koh is on fux's path, and the
  clap-free surface comes from a `cli` feature in koh rather than a crate split.
