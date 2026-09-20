# Verification

This document preserves the baseline observations, including the old bordered/top-bar presentation. See [verification/design-restoration.md](verification/design-restoration.md) for the current visual/input gates and captures, and [REFINEMENT.md](REFINEMENT.md) for historical refinement measurements and the capability audit.

## Exact build

Observed 2026-09-19T23:04:25Z on Apple M2 Max / arm64, Darwin 27.0.0. Rust **1.98.1** (`48a229ceaefd4985c50990b14116b6d856af0985`, 2026-09-01); Bevy crates **0.19.1**, pinned by `Cargo.lock`. Canonical Bevy examples were consulted at `b56fc29d3016e641754765244b5ba3f9cc504671` in the separate Bevy checkout.

Implementation: `/Users/kisaczka/Desktop/code/fux-minimal-bevy`, orphan branch `minimal-bevy-fux`, uncommitted. No commit, push, tag or publication was performed. The main and prior rewrite codebases were not used as ancestry.

Source manifest SHA-256: `066ad84b26a4120c959a1aac2f76043cdbc74cf27dfd765c0925bef16a1a18c2`. Reproduce from this directory:

```sh
shasum -a 256 Cargo.lock Cargo.toml rust-toolchain.toml src/*.rs tests/*.rs | shasum -a 256
```

Release executable SHA-256: `a03372027c970b14c0f8ba26150e76cedeb9a42c00c95c0583949b327d8b1e49`.

Passed commands:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release --locked
```

**Six tests passed:** two unit tests and four real-server integration tests. They defend layout/process ownership, modified xterm key semantics, stock launch/removal settling without a subsequent request, interactive shell background-job cleanup, scene entity remapping/prompt paste/native visibility changes, and a real attached terminal whose output is deliberately left unread. The last also checks that its hot writer is reaped and publishes an exit rather than a cleanup error.

Integration fixtures isolate their working/home directories and disable user startup/history hooks. Their temporary directories are removed when the owned server exits.

## Full stock remote surface

Final `rpc.discover`: **26 methods**, consisting of all **23 stock methods** and three terminal-native extensions. Final `registry.schema`: **172 registered type schemas**. Requests used ordinary JSON-RPC without credentials or authorization setup.

```text
fux.attach
fux.frame
fux.frame+watch
registry.schema
rpc.discover
schedule.graph
schedule.list
world.despawn_entity
world.get_components
world.get_components+watch
world.get_resources
world.insert_components
world.insert_resources
world.list_components
world.list_components+watch
world.list_resources
world.mutate_components
world.mutate_resources
world.observe+watch
world.query
world.remove_components
world.remove_resources
world.reparent_entities
world.spawn_entity
world.trigger_event
world.write_message
```

Stock spawn/get/insert/remove/despawn changed a scratch `Name` entity. Stock `Node.flex_grow` mutation moved painted pane boundaries from columns `[0,47,48,95]` to `[0,32,33,95]`; inserting and removing native `Visibility` hid/restored the pane while otherwise idle. Actual `Launch` argv/cwd plus `PaneView` and native reparenting produced `CWD:/private/tmp/fux-minimal-proof/assets`. These were actual World operations, not mirrored application endpoints. See [README.md](README.md#unrestricted-remote-control) for runnable request shapes and ID substitution.

## Actual terminal evidence

Disposable native PTYs launched the real release `fux attach`. A shell wrapper recorded terminal modes before/after; a Python 3 harness with pyte 0.8.2 decoded the actual ANSI stream. Assertions observed completed synchronized paints, not source text or mocked frames. Shell-output markers used escaped prefixes so command echo could not satisfy output assertions.

| Scenario | Observed result |
| --- | --- |
| Zero configuration | `fux server --port 17944` in an empty directory started the default `/bin/sh`; native input produced `DEFAULT_READY` in its real terminal frame. No `fux.json` existed. |
| Shared interactive views | 96×28 and 68×20 viewers shared two real PTYs; typed output stayed in the selected pane. Focus, zoom and scrollback remained viewer-local. |
| Layout/workspaces | Both split directions, focus cycling/explicit focus, pane/workspace naming, workspace creation/switching, moving, sibling reordering, width/height changes, close, terminate and recovery from an empty workspace were exercised. Moving and scene replacement retained process identities. |
| Actual resize/input | Resizing to 100×30 repainted the bottom/borders at the new dimensions. An application received pane-relative SGR mouse press/release, application-cursor Left, Ctrl-Shift-F1 and bracketed paste exactly as shown below. |
| Copy/help | OSC 52 decoded to the focused visible history; help browsed all 24 bindings, including first/last-page wrap. |
| Native assets | Editing JSON while idle changed Ctrl-B to Ctrl-A and its real binding behavior; malformed JSON preserved the last valid settings. A configured native scene asset and a direct scene Row→Column edit changed the displayed layout. |
| Native scenes | Actual save/load prompts accepted bracketed-pasted paths. Save, rename and load restored the workspace without new PTYs. Explicit mapping swapped two pane positions while retaining their PIDs; missing references left the old layout intact. |
| Viewer lifecycle | Graceful detach/SIGTERM returned 0 and restored terminal modes. SIGKILL returned 137; a fresh viewer reattached to the same processes and produced new shell output. Final-release retained PIDs were 91395 and 91985. |
| Natural exit | Exit 37 retained final output and an `[exit:37]` indicator, with no live PID or cleanup error. |
| Hot termination | Ten fresh final-release `/usr/bin/yes` processes were visibly terminated, retained exit 129 with no error, and had no remaining PID. Observed stop-to-paint times were 114.1–130.4 ms, including the shell-hangup grace. |
| Server shutdown | Direct shells and their ordinary background job-control groups were gone after shutdown. Viewers restored terminal modes on server EOF; their EOF exit status is 1, not a successful user detach. |

Application input bytes:

```python
b'\x1b[<0;3;2M\x1b[<0;3;2m\x1bOD\x1b[1;6P\x1b[200~PASTE_PROOF\x1b[201~'
```

Terminal mode comparisons excluded only Darwin's transient `PENDIN` bit. SIGKILL cannot run terminal restoration; the forced-death scenario used a disposable PTY, not the user's terminal.

Smoke testing found and corrected idle stock-mutation settling, native layout invalidation, macOS asset-path aliasing, prompt paste routing, shell hangup propagation, a paint-lock/SSE drain deadlock, queued-output PTY reaping, and the macOS exiting→zombie group-error classification race. Hot-output paints now coalesce before rendering behind an on-demand 16 ms interval rather than rendering every PTY wake.

## Final release observations

Single-host observations, not a comparison against either earlier fux implementation or a performance guarantee. Release profile: stripped, thin LTO, one codegen unit. Command:

```sh
SHELL=/bin/sh PS1='$ ' ./target/release/fux server --port 17941 --config /tmp/fux-minimal-proof/assets/fux.json
FUX_ENDPOINT=http://127.0.0.1:17941 ./target/release/fux attach
```

Settings were `{"shell":["/bin/sh"],"history_lines":2000}`; two PTYs and two differently sized viewers were attached. Measurement helpers and terminal emulation ran outside fux and are excluded from its process memory/CPU figures.

- **Executable:** 32,486,640 bytes (30.982 MiB).
- **Warm-filesystem startup:** exec to first successful stock `ProcessState` query with a live default PTY PID; 1 ms observation polling, five independent launches. Raw milliseconds: `15.797792, 15.773000, 12.995666, 12.513542, 11.484584`. Median **12.996 ms**. This is not time to first attached paint.
- **Idle window:** 5.000204 seconds after settling, with no API polling. `proc_pid_rusage` counters; CPU Mach ticks converted with this machine's `mach_timebase_info` ratio 125/3 ns per tick. The conversion was checked against an approximately 100 ms CPU-bound probe.

| Process | CPU ms | % of one core | Interrupt wakeups | Package-idle wakeups | RSS MiB | Physical footprint MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| server | 1.266250 | 0.025324 | 73 | 0 | 27.484375 | 11.625572 |
| left | 0.000000 | 0.000000 | 0 | 0 | 11.921875 | 5.484879 |
| right | 0.000000 | 0.000000 | 0 | 0 | 10.781250 | 4.375504 |

The main runner has no recurring idle update, but the stock native file-watcher debouncer still wakes its worker periodically (300 ms debounce, internal quarter-interval polling). **This is not a zero-wakeup server claim.** Streamed painting uses only on-demand one-shot timers.

- **Key echo, 20 samples:** median 16.913 ms, nearest-rank p95 17.612 ms, maximum 18.330 ms. Raw ms: `14.664375, 15.420959, 16.210834, 17.028625, 16.852583, 16.721208, 17.036708, 16.746208, 17.199542, 16.692250, 17.612250, 16.858708, 17.197958, 17.070292, 17.034584, 16.801417, 18.329792, 16.320208, 16.966708, 17.511791`.
- **Enter→shell response, 10 samples:** median 17.084 ms, p95/max 17.547 ms. Raw ms: `17.330125, 17.081750, 17.261667, 16.574334, 16.940542, 17.154875, 17.547042, 16.884875, 17.015334, 17.086833`. Commands were already typed before timing Enter. Both latency measurements end when the external terminal decoder observes the completed paint, so they include harness/terminal decoding overhead.
- **Sustained output:** a producer repeatedly wrote 9,984-byte ASCII/newline chunks (`("LOAD "+"x"*72+"\n")*128`) through its real PTY for three seconds. It wrote **302,495,232 bytes** in **3.000111417 s**, **96.157 MiB/s** of producer-side output. The final output marker was painted. While the producer was still running, a whole command sent to the other pane produced its completed paint in **33.327 ms**; both viewers remained attached.
- **RSS after output:** server 37.843750 MiB, left viewer 11.953125 MiB, right viewer 11.640625 MiB. These are sampled resident sizes, not measured peaks or total system cost.

## Boundaries

Only macOS arm64 was executed. Linux/other Unix behavior is unvalidated; Windows/mobile are not implemented. The UI adapter paints terminal surfaces, borders and status, not arbitrary Bevy text/image/shader rendering. PTY resize uses vt100 behavior rather than paragraph reflow. OSC 52 depends on the outer terminal. Stock Bevy may close a full watch response channel; the actual viewer continuously drains/coalesces without a custom transport or resubscription layer.

Owned direct children/original process groups are reaped; interactive shells propagate hangup to ordinary job groups. Deliberately detached/disowned or other-group hangup-ignoring descendants are not a containment guarantee. No numeric PID/PGID is signalled after its owner is reaped. macOS group membership is checked through native [libproc](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.c) only after the owned leader's exit is observable, before reaping.

The unrestricted API is same-user command execution, not an authenticated service. No authentication/hardening, recovery/restart policy, host catalog, durable input receipts, plugin system, graphics protocol or cross-version scene/API compatibility is claimed.

## Cleanup

Disposable terminal drivers, captures, settings/scenes, producers, measurement scripts and temporary Python dependencies were removed after recording the observations. All owned `minimal-*` verification processes are stopped. Pre-existing services were left untouched. The six regression tests and verified release executable remain; no benchmark framework or smoke scaffolding was added to the package.
