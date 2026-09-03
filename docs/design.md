# The Shape of fux

A zellij-based agent workspace with a mosh-grade peer-to-peer attach path, shipped as one
`cargo install`-able binary.

- **Status:** proposal, audited against source
- **Date:** 2 Sep 2026; open questions and checklist re-audited 3 Sep 2026
- **Reference trees:** `references/` — zellij `main` at 0.46.0 (af38660, 31 Aug 2026) ·
  koh 0.9.1 (6c84ffe) · herdr 0.8.2 (94f6d9c, github.com/herdrdev/herdr)
- **Published:** zellij / zellij-client / zellij-server / zellij-utils **0.45.1** (crates.io, 28 Aug
  2026) · koh 0.9.1 · fux 0.1.0 placeholder. The `herdr` crate on crates.io (0.1.0) is an
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
   `rust-toolchain.toml` already use), from the plugin crate in the same workspace.
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

`id`, `key`, `connect` and `serve` are koh's existing subcommands (`Cmd::Id`, `Cmd::Key`,
`ConnectArgs`, `ServeArgs`) re-exposed under a new name. Only the bare invocation is new code.

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
(21 of them, from `amp.toml` to `qwen.toml`) of prioritised regex rules scoped to named regions —
`osc_title`, `bottom_non_empty_lines(n)`, `last_non_empty_above_prompt_box`,
`after_last_horizontal_rule`, `whole_recent`, `prompt_box_body`. The Claude manifest alone carries
rules keyed on the braille spinner range, the `esc to interrupt` footer, and a set of negative guards
so that a user typing "do you want to proceed?" cannot impersonate a state change.

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

## Licensing: relicense koh, keep fux MIT

| Component | License |
|---|---|
| zellij | MIT |
| herdr | Apache-2.0 |
| koh | GPL-3.0-or-later today; **relicensing to MIT** (same author as fux) |
| fux | MIT |

Linking koh as a library makes fux a derivative work, so koh's license has to be compatible before
fux publishes anything past the placeholder. koh and fux share an author, so the decision is to
relicense koh (MIT, or dual MIT/GPL) rather than move fux to GPL. Do this as a koh release before
fux depends on it, so the crates.io metadata and the git history agree.

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
- `zellij/zellij-server/src/plugins/plugin_loader.rs:478` — store limits: 1 instance, 4 memories,
  16 MB each, trap on grow failure
- `zellij/zellij-utils/src/consts.rs:143`, `:179` — `add_plugin!` / `ASSET_MAP` embedding;
  `zellij-utils/src/setup.rs:211` — first-run unpack
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
- `koh/src/server/cli.rs:49`, `:102` — `--shell`: the command koh hosts in the pty; `serve` entry point
- `koh/src/main.rs:58` — koh's own binary is dispatch over `koh::server::serve`, `client::connect`, `run_id`, `keycmd::run`
- `koh/src/lib.rs:41` — koh's documented stable surface: exactly those four entry points plus `ssp`
- `koh/src/pty.rs:134` — `--shell` is passed whole as argv[0]; no argument splitting, no `sh -c`
- `koh/src/client/render.rs:245` — client rings the local bell when the server's bell count climbs
- `koh/src/server/cli.rs:47` — `--allow` is required; `serve` refuses to start with an empty list
- `koh/src/terminal/mod.rs:22`, `:132` — bell and OSC 2 title carried out-of-band over SSP
- `koh/src/server/session.rs:57` — `spawn_session`: pty + vt100 emulator + drain task
- `koh/src/terminal/server.rs:107` — `ServerTerminal`, the server-side screen model
- `koh/Cargo.toml` — GPL-3.0-or-later (pending relicense), pinned `iroh = "=1.0.0"`, `vt100 = "=0.16.2"`, 1.91 floor;
  `git shortlog -sn` shows a single author, so relicensing needs no third-party consent
- `zellij/zellij-utils/src/input/permission.rs:13`, `consts.rs:104` — `PermissionCache`: granted permissions
  persist as `permissions.kdl` in the cache dir, keyed by plugin name
- `zellij/zellij-tile/src/shim.rs:1653`, `:1661` — `cli_pipe_output`, `pipe_message_to_plugin`: the plugin
  pipe API in both directions; `zellij-utils/src/cli.rs:678` — the `pipe` CLI action
- `zellij/zellij-client/src/lib.rs:577` — synchronized output is enabled only when `TERM=alacritty`
- `herdr/src/detect/manifests/claude.toml` — rule regions, priorities, negative guards
- `herdr/src/detect/manifest.rs:1107` — the region vocabulary
- `herdr/src/pane/agent_detection.rs:5` — idle confirmation hysteresis constants
- `herdr/src/platform/linux.rs:554`, `macos.rs:547` — `notify-send`, `osascript` notifiers

---

## Open questions

Everything still undecided, in the order it blocks work. Two items that were open in the first
draft are now answered from source and moved to *Settled by audit* below.

1. **Does zellij's client render cleanly under koh's vt100 grid?** koh advertises
   `TERM=xterm-256color`; zellij's client probes for kitty keyboard support and answers DSR/DA/DECRQM
   queries through `session.rs:105`. Synchronized output is off under any `TERM` other than
   `alacritty`, so that probe is moot. This is the only risk that can sink the design, and it is
   testable today with no fux code. Two details make the naive one-liner fail:
   - `--allow` is mandatory; `koh serve` refuses an empty allow-list.
   - `--shell` is handed to the pty as a single argv[0] with no splitting, so
     `--shell "zellij attach -c main"` tries to exec a program with a space in its name.

   The working test is a wrapper script:

   ```sh
   cat > ~/bin/fux-attach <<'EOF'
   #!/bin/sh
   exec zellij attach -c main
   EOF
   chmod +x ~/bin/fux-attach
   koh serve --allow <phone-id> --shell ~/bin/fux-attach
   ```

   Exercise from the phone: resize, split panes, scroll, a full-screen app inside a pane, detach
   and reattach. fux itself will not need the wrapper: once koh's `cli` feature plan lands,
   `fux serve` fills `ServeConfig.command` with its own executable path, and bare `fux` is the
   attach path. koh could still grow `--shell` argument splitting so the test is reproducible
   without a script.
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
   region vocabulary is seven regions and a priority loop. A fresh evaluator under the 16 MB wasmi
   cap, validated against herdr's manifests with captured-pane fixtures, is the recommendation.
   Needs a decision before the plugin crate starts.
7. **wasmi cost in practice.** Interpreted regex over the bottom-*n* lines of every redrawn pane.
   No measurement yet. Measure with the fixture set before optimising.
8. **Plugin permissions surface.** `ReadPaneContents` is required. `RunCommands` if (3) is
   plugin-side. `ReadApplicationState` for pane and tab metadata. Confirm the set. Pre-granting is
   solved (see *Settled*).
9. **Version pins.** `zellij-* = "=0.45.1"` now; 0.46.0 when it publishes. `iroh = "=1.0.0"` via
   koh. Toolchain floor becomes 1.95. The Termux question is settled below.
10. **Termux build of the client-only feature set.** `cargo install fux --no-default-features`
    should need only koh's dependency tree. Unverified until the koh relicense release lands.

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
  and everything else as unstable. fux depends on exactly that set. The one gap is that the
  argument structs are clap types with private fields; the `cli` feature plan in the koh checklist
  replaces them with plain config types.

---

## Your side: koh and everything else outside this repo

Work that has to happen in repos other than fux, or in accounts only you control.

### koh

- [ ] **Relicense to MIT** (or MIT/GPL dual). Change `license` in `Cargo.toml`, replace `COPYING`,
      note it in the changelog. `git shortlog` shows one author, so no consent round is needed.
- [ ] **Cut a koh release** carrying the new license so crates.io metadata and git agree. fux pins
      that version. crates.io is at 0.9.1 (29 Jun 2026); the relicense makes 0.10.0.
- [ ] **Run the vt100 compatibility test** (open question 1) from a phone, using the wrapper
      script. Fix whatever zellij's client does that koh's emulator mishandles.
- [ ] Optional: **let `--shell` take a command line.** With `ServeConfig.command` as argv,
      this is only a clap-layer choice: split on whitespace, accept `--shell` repeated, or add
      `-- <cmd> [args]` passthrough. Removes the wrapper script from the test and from anyone
      else hosting a non-shell program.
- [ ] **Decide the phone notification channel** (open question 2). If bell/title: make koh's client
      call `termux-notification` on bell or on a title change matching a pattern. If a state stream:
      add a frame type to SSP.
- [ ] **Add a `cli` Cargo feature so fux can call koh without clap.** Every field of `ServeArgs`
      is private, so fux cannot build one that points the pty at its own executable. Rather than
      a builder on the clap struct, or a crate split, gate clap behind a feature. clap is confined
      to `main.rs`, `keycmd.rs`, `server/cli.rs` and `client/cli.rs`, so the change is small:
      - `cli` feature, on by default, owns the `clap` dependency; the `koh` binary gets
        `required-features = ["cli"]`. `cargo install koh` is unchanged.
      - Each entry point takes a plain config type with public fields: `serve(ServeConfig)`,
        `connect(ConnectConfig)`, `run_id(IdConfig)`, `keycmd::run(KeyConfig)`. The existing
        `*Args` structs stay, gated on `cli`, with `From<ServeArgs> for ServeConfig` and so on.
        The config types replace the clap structs as the documented stable surface in `lib.rs`.
      - `ServeConfig` carries `command: Vec<String>` (argv), so fux passes its own executable
        directly and the `--shell` splitting item below becomes a clap-layer detail.
      - The QR renderer and the passphrase prompt stay in core, not `cli`; fux wants `id` and
        `key` too. Helpers reachable only from the clap structs get `#[cfg(feature = "cli")]` so
        `dead_code = "deny"` holds with the feature off.
      - fux depends on koh with `default-features = false`.
      Blocks `fux serve` as designed; do it in the same release as the relicense.

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

- fux is one crate, one binary, with a default `workspace` Cargo feature; the phone build is
  `--no-default-features`.
- fux stays MIT; koh relicenses.
- Depend on upstream zellij, pinned exactly; fork only when a host function is needed.
- Depend on koh as a library, not an extraction: nearly all of koh is on fux's path, and the
  clap-free surface comes from a `cli` feature in koh rather than a crate split.
