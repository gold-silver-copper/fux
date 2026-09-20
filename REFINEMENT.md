# Refinement evidence

## Idiomatic follow-up against `136c156`

`EntityHashMap`, `set_if_neq`, `resource_scope`, `insert_if_new` and borrowed formatting replace generic hashing, duplicate publication/ownership and temporary paint strings; key encoding shares CSI construction. Production code decreases 27 nonblank/noncomment lines; direct-frame CPU median improves 12.5%, scaling CPU 19.5%, and settled frame allocations fall 127→22. Capabilities, dependencies and native boundaries remain unchanged. [Paired raw evidence, conditions and reproduction scripts](https://gist.github.com/gold-silver-copper/9c38c3511a83748a949773d906bee3fa) distinguish this comparison from the historical refinement below.

## Identity and conditions

Refines `b80f9ee366711e1eabb193f5f9f9fded26273dd8` without changing ancestry or committing/publishing. Baseline source and release were isolated before editing; all six original regressions passed. Measurements below are fresh comparisons, not the historical figures in VERIFICATION.md.

- Apple M2 Max, arm64 Darwin 27.0.0; Rust 1.98.1 (`48a229cea`), Bevy 0.19.1. Canonical examples/source consulted at `b56fc29d3016e641754765244b5ba3f9cc504671`.
- Baseline source manifest: `066ad84b26a4120c959a1aac2f76043cdbc74cf27dfd765c0925bef16a1a18c2`.
- Refined source manifest: `a0d4c04defc209d3ca222a94581c62c19303c63847ec45bb493d57d175662469`.
- Baseline executable: `a03372027c970b14c0f8ba26150e76cedeb9a42c00c95c0583949b327d8b1e49`.
- Refined executable: `c12fd556cbbfb369d366d474d15c7dc9b9a6b20567273c64e5fb980be9cbec5e`.
- Unchanged lock SHA-256: `1f2c4d0afb4ecd66f47ae00cd1c3be1d9ae9292f568afe114147c5bd06052fc2`.

Reproduce the source fingerprint with:

```sh
shasum -a 256 Cargo.lock Cargo.toml rust-toolchain.toml src/*.rs tests/*.rs | shasum -a 256
```

Actual frontends ran in native outer PTYs with isolated HOME/cwd/startup/history, `/bin/sh`, 96×28 and 68×20 viewports. The JSON configuration specifies 2,000 history lines. In **both** binaries the initial Startup shell precedes asynchronous configuration loading and therefore has the default 10,000; subsequently created panes have 2,000. No history, buffer, frame cadence, release profile, watcher, toolchain or dependency-version reduction was used.

Python/pyte 0.8.2 decoded completed synchronized paints. Native `proc_pid_rusage` measured CPU, interrupt wakeups and RSS; `proc_pidinfo` measured descriptors/threads. Idle intervals were three seconds after settling, without polling RPC. Latency observes completed terminal output, not just request completion; polling resolution is about 2 ms. Raw samples, including frontend costs, are retained in [verification/refinement-results.json](verification/refinement-results.json).

## Changes: cost → mechanism → result

| Observed cost/problem | Implementation and removed work/state | Risk and verification |
| --- | --- | --- |
| One layout change invalidated all workspaces | Required, unreflected workspace `LayoutCache`; native descendant traversal and arbitrary component/archetype checks. Scan only cached roots, stop at first dirty member, collect membership on extraction. Native cache component change tick supplies the projection key instead of a global revision. | Generic coverage retained: insertion/removal, in-place changes, nested reparenting, despawn/root replacement and an unanticipated reflected component. Ten edits rebuilt unrelated projections **20 → 0** times. |
| Identical screen requests repeatedly allocated rows and visited every cell | One bounded cached row vector per terminal, keyed by parser revision/history offset; return borrowed rows and native `vt100::Screen`. Reset temporary history before returning. | Cursor-only, mode-only, resize, output, Unicode/styles and distinct history offsets verified. Same 100-direct-frame workload: **508 → 3** actual screen extractions, with 508 frame/snapshot requests in both. |
| Duplicate derived rectangles and terminal metadata | Borrow `Presentation::rects()`; remove `View.rects`, `TerminalSnapshot`, numeric `TerminalModes`, collecting first-leaf traversal and single-use viewer lookup adapter. | Native geometry, focus, picking, cursor and exact application bytes verified. No second layout solver or emulator. |
| New HTTP Agent/pool for every frontend RPC | Reuse the maintained Agent through `LazyLock`. | Interleaved real key tests: frontend Unix syscalls about **2,560 → 2,080**, CPU median **39.28 → 24.49 ms** per 40 echo/erase pairs. No transport replacement. |
| Equal explicit copies could disappear as replaceable paint state | Bounded 16-entry pending effect queue, visible overflow notice, effect-aware equality; frontend extracts every OSC52 in a frame. | Repeated equal copies, blocked frontend delivery, 16 accepted/17th rejected, and direct-frame no-replay verified. |
| Stock Viewer removal could retain its size constraint | One native `On<Remove, Viewer>` observer owns presentation-context cleanup; remove duplicate manual cleanup paths. | Stock component removal and entity despawn both release actual PTY size constraints. Repeated contexts/FDs/threads remain stable. |
| Unused direct features | Remove nix `poll` and `term`; retain `fs`, `process`, `signal`. | Locked graph unchanged: 33 direct dependencies and 312 unique host dependency-tree entries. No claim that transitive UI/render-related crates disappeared. |

### Deliberately retained mechanisms

Full self-contained ANSI paints and final string comparison remain: instrumentation still counts 508 frame constructions, not an invented elimination of all formatting. A complete prepared-frame dirty key would have to cover arbitrary reflected layout changes, viewer/help/prompt/settings state, process status and effects. Row reuse removes demonstrated extraction/allocation work without an incomplete dirty flag or acknowledgement protocol.

Full native scene replacement remains the structural fallback. Pinned `DynamicWorld::write_to_world_with` applies/inserts; it does not implement deletion/diff semantics. An incremental copier would add another scene engine. Independent inert Apps remain because native focus is World-global; measured memory and projection counts do not justify a bespoke context-switcher. They never install PTY ownership systems.

Size aggregation still walks authoritative borrowed rectangles during presentation transactions; separating it would require ensuring every affected native layout is ready without transient size oscillation. Native ioctl tests verify the existing minimum-size policy. `with_views` remains an exclusive boundary for native scene extraction/application and coherent cross-view sizing, not the ordinary control dispatcher. Controls remain typed queries/commands.

The parked runner, causal post-remote settling pass, pending-asset deadline, on-demand paint timer, bounded per-pane drain, native waiter/grace/master-close/reap ordering and ownership guards are unchanged. No readiness framework, periodic App tick, native-library replacement or hidden API filter was added.

## Comparable measurements

Three independent runs per main/scaling workload; interleaved baseline/refined runs for single-key/direct-snapshot tests, five startup samples each. These are workload-specific, not general speedup claims.

| Metric | Baseline | Refined |
| --- | ---: | ---: |
| Release bytes | 32,486,640 | 32,486,704 |
| Production Rust lines, excluding cfg(test) sections | 3,668 | 3,667 |
| Warm startup → live PTY, median ms (interleaved) | 11.18 | 11.36 |
| Server exec → first usable attached paint, median ms | 23.26 | 23.93 |
| Attach-only first paint, median ms | 11.78 | 11.72 |
| Single-key visible latency, per-run median ms | 8.61 / 8.78 / 8.36 | 8.39 / 8.46 / 8.38 |
| Single-key p95 samples, ms | 12.81 / 14.01 / 13.48 | 12.28 / 12.76 / 13.25 |
| Five-character echo median / p95, ms | 24.46 / 26.90 | 25.17 / 27.23 |
| Concurrent-pane command median / p95, ms | 23.39 / 26.72 | 22.89 / 25.78 |
| Producer MiB/s, three-second sustained output | 83.19 / 83.27 / 83.43 | 84.03 / 83.65 / 83.89 |
| Server CPU during output, ms | 6234.74 / 6228.39 / 6231.41 | 6225.30 / 6220.77 / 6211.14 |
| Interactive peer frontend CPU during output, ms | 33.04 / 32.68 / 32.75 | 24.93 / 25.31 / 25.06 |
| 1,000 direct frames with two viewers: server CPU, ms | 207.73 / 205.43 / 200.33 | 150.08 / 151.29 / 154.01 |
| 25 edits; 16 panes, six viewers, four workspaces: server CPU, ms | 69.26 / 73.01 / 74.94 | 40.66 / 47.79 / 44.39 |
| Same scaling workload: per-run median paint latency, ms | 12.24 / 13.96 / 14.17 | 13.82 / 14.26 / 14.17 |
| Scaling RSS, MiB | 36.42 / 36.42 / 36.31 | 36.30 / 36.45 / 36.33 |

Repeatable targeted server CPU reductions are about **26%** for direct snapshots and **39%** for workspace edits. Frontend work was reduced, not used to hide server costs. Producer throughput/server CPU and memory are effectively unchanged. The sub-millisecond five-character echo increase was investigated with interleaved single-key tests: neither their medians nor tails regress; the burst difference is below observer polling resolution and does not justify a general latency claim. Startup and binary size are effectively unchanged. Production line count is essentially flat, not a meaningful shrinkage claim; simplification is removed duplicate state/adapters and features, while also fixing three correctness gaps and retaining tests.

Idle server observations (median of three, CPU milliseconds and interrupt wakeups per three seconds):

| Viewers/state | Baseline CPU / wakes / RSS MiB | Refined CPU / wakes / RSS MiB |
| --- | --- | --- |
| None | 1.470 / 43 / 20.36 | 1.473 / 44 / 20.16 |
| One | 1.488 / 43 / 25.38 | 1.382 / 44 / 25.13 |
| Two | 1.398 / 44 / 27.03 | 1.436 / 44 / 26.95 |
| Two, after output | 1.461 / 43 / 30.13 | 1.375 / 43 / 30.02 |

These small native watcher/worker wakeups are not a recurring main-App tick. Full frontend idle samples are in the raw artifact.

## Capability matrix and adversarial evidence

The matrix was established before production edits. Final release checks used the executable fingerprint above; the raw artifact retains checks, PID observations, exact input bytes and timing samples.

| Required contract | Final evidence |
| --- | --- |
| CLI/defaults/options/named attach/rpc/stop/FUX_ENDPOINT | Real invocations without configuration, usable shell, named attachment and graceful stop; `acceptance-final` |
| Exact argv/cwd/configured shell; native PID/dimensions/errors | Child records actual argv/cwd; configured command used for subsequent split; native launch and zero-size errors observable; `acceptance-final`, `remote-contract-final` |
| Styles/Unicode/cursor/modes/history/final output | Decoded completed paints, cursor-only movement/hiding, exact cursor/paste/mouse/query-reply bytes, bounded history and independent offsets; natural exit 37 with final screen |
| Both splits/workspaces/naming/empty recovery/close/terminate | Actual controls, decoded geometry, process identities and close-vs-terminate checks; `acceptance-final` |
| Focus/picking/grow/shrink/reorder/move | Native mouse focus, geometry changes in both axes, sibling positions, cross-workspace movement; actual terminal surface |
| Independent viewers/focus/workspace/zoom/history/prompts | Differently sized native PTYs, shared PIDs, independent displayed state; extra viewers/workspaces in scaling/stress |
| Minimum PTY size/padding/no-viewer retention | Actual child ioctl sizes `[17,66] → [15,58] → [25,94] → [10,94] → [10,40] → [21,78]`; larger-view blank padding, detach growth and stock Viewer removal; `adversarial-final` plus regression |
| Prefix/literal prefix/bindings/help/prompts/no leakage | Actual key/paste input, all 24 help pages, reloaded prefix/bindings and raw child-byte assertions |
| Ordered input and explicit copies | Four 16,000-byte bracketed pastes arrive exactly in order; 65,537-byte input rejected without partial acceptance; repeated equal copies, blocked delivery, queue overflow and no snapshot replay |
| Native JSON/layout reload, invalid retention | Quiet file edits update completed paints without polling RPC; invalid JSON/scene retains usable state and PIDs; `adversarial-final` |
| Native save/load/mapping/arbitrary components | Real bracketed-paste scene prompts; same-server references and explicit remap; missing mapping/file leaves hierarchy intact; TabIndex survives serialization; no PTY resurrection |
| Complete trusted BRP/actual operational state | Exact 26 methods and all 172 schemas compared, ignoring ephemeral discover URL and additive unreflected LayoutCache requirement only. Actual stock component/resource/schedule/hierarchy/event operations exercised; no method or component filtering |
| Quiet arbitrary mutation/insert/remove/reparent/despawn/root replacement | Real Node/Visibility edits without follow-up RPC; generic unanticipated reflected `Extra` regression covers old/new roots and replacement; 200 batched mutations followed by fresh direct frames all correct |
| Launch recipe/removal/despawn and actual resize | Changing argv does not restart live PID; removal and stock process despawn clean up; reflected dimensions reach child ioctl |
| Layout ownership distinct from process references | Raw hierarchy deletion/projection replacement preserves referenced PIDs; fux closing the final reference terminates; original relationship regression retained |
| Natural exit/hot writers/background groups/ownership | Final output/status; ten fresh hot writers terminate in 102.5–113.8 ms; ordinary interactive-shell background groups cleaned up; unchanged ownership/reaping guards audited |
| Signals/EOF/forced loss/reattach | SIGINT/SIGTERM/SIGHUP restore full termios (mask only transient Darwin PENDIN); server EOF restores and exits 1; forced loss/reattach retains child PID; `stress-final` |
| Hot/blocked fairness and shutdown | Blocked viewer still receives four distinct copies after draining; peer response 12.87 ms. Two simultaneous hot panes plus idle pane/unrelated workspace respond in 6.16–26.97 ms. Server shutdown reaps children even while outer terminal is blocked |
| No retained contexts/entities/FDs/threads/cache growth | 31 frontend/API attach-detach and scene replacement cycles: 92 entities, 15 FDs, 21 threads throughout; RSS 28.55 → 28.95 MiB, only 0.094 MiB after cycle 10; original PIDs stable |

Physical backpressure is not concealed: in both baseline and refined builds, a frontend blocked writing its outer terminal waits for that terminal to drain before completing graceful output/restoration. The server does not wait for it and still reaps its children. After draining, full termios restoration and expected signal/EOF exit status were verified (`blocked-shutdown-final`). No claim of delivering terminal reset bytes to a permanently non-reading consumer is made.

Temporary instrumented copies additionally observed: 20 consecutive native scene replacements each retain exactly **256 projection entities / six mappings**, not accumulating process placeholders. A sustained hot-output interval with an unrelated workspace performs **zero projection rebuilds** while the cold pane responds. Profiling binaries were not used for CPU/latency comparisons. Six counters measured frame construction, snapshot requests, snapshot materializations, native UI updates, scene extraction and visited layout entities; raw totals are retained. The clean production executable was rebuilt after removing shared-target instrumented artifacts and matched the fingerprint above.

## Commands and final gates

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
git diff --check
cargo tree --locked -e features
cargo tree --locked --prefix none
```

All pass. The nine-test suite (all six original regressions plus arbitrary invalidation, repeated copy effects and stock Viewer cleanup) passed **20 consecutive complete runs**. An earlier intermediate parallel run had a connection reset on initial attach; it did not recur in these final runs or final real-terminal suites. It is not presented as a diagnosed production defect.

Disposable external harness commands used `/tmp/fux-refinement-python/bin/python` with `probe.py BINARY OUTPUT_JSON 3`, `scaling.py BINARY OUTPUT_JSON`, `latency.py`, `acceptance.py`, `adversarial.py`, `stress.py`, `remote_contract.py`, and `instrument.py BINARY LABEL`. The main probe measured settled idle phases, twenty five-character echoes, a three-second output producer plus twenty concurrent commands, and post-output idle. Scaling created 16 panes/six viewers/four workspaces and timed 25 workspace renames. Interleaved latency trials used 40 single-key echo/erase pairs and 1,000 direct frames with two watchers. Native acceptance checks used actual outer PTYs and completed paints, not mocked rendering. Only raw observations remain in the repository; no profiling code, benchmark framework, generated production code or compatibility shim was added.
