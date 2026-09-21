# fux-fuzz

An unpublished, opt-in **black-box scenario harness** for an already-built fux. It uses real servers, attached terminal viewers, PTYs, and the public BRP API. It is neither coverage-guided fuzzing nor a claim that fux survives everything.

This is an independent crate, not a workspace member. Root `cargo test --locked` does not build or run it. There is no new CI gate. Run the short smoke locally; request longer stress runs when useful.

## Build and run

From the repository root, with the pinned Rust toolchain:

```sh
cargo build --locked
cargo build --manifest-path fux-fuzz/Cargo.toml --locked

# Smoke: all twenty-seven scenarios (forty-eight isolated cases).
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux

# Individual scenarios:
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario startup
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario resize
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario shutdown
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario paste
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario keys
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario signal
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario mouse
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario copy
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario history
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario zoom
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario layout
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario process
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario nav
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario scene
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario config
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario overlay
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario limits
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario chrome
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario selection
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario race
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario memory
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario reorder
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario scene_map
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario mouse_edge
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario clipqueue
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario resize_cmd
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario api_misuse

# Explicit, bounded stress: 10 fresh fixtures, 30 generated resize pairs each.
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux \
  --scenario resize --seed 42 --iterations 10 --actions 30 --seconds 120

# Substitute the trace path printed by a previous run:
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux \
  --replay /path/to/run/trace.json --output /tmp/fux-fuzz-replay
```

`--fux` is mandatory and canonicalized before any child changes directory. No run implicitly builds fux or searches PATH for an installed fux. Use a trusted local binary: the application API permits unrestricted same-user command execution.

Scenarios in this harness have found seven production defects so far, each fixed in the pull request that added the scenario: a layout scene write corrupting the live hierarchy and a layout load surviving its own workspace's close (both below), an empty-workspace attach failure (PR #34), an unclamped scroll offset (PR #33), a legacy mouse-release encoding defect (PR #32), a control-key encoding defect (PR #31) and a bracketed-paste envelope defect (PR #30). The nine scenarios added in PR #35 found no further defects; they are retained as coverage. An earlier first-pane configuration bug this harness found was fixed in PR #29; its original saved failure trace still passes. Exit 1 means at least one scenario, setup, diagnostic, cleanup, interruption, or deadline failed. Other cases still execute within the overall budget; no failed case is retried. A dependency panic during scenario execution is reported as a harness/dependency failure, not an application defect.

## What the scenarios check

- **Startup:** missing, malformed, and valid configuration; real frontend attach immediately after read-only readiness checks; first `Launch.argv` and its actual terminal output. Distinct executable wrappers identify the configured and environment-default shell. No settling sleep precedes the initial observation.
- **Resize:** two real viewers of a shared process, rapid PTY resize bursts, settled tiny viewports, conflicting dimensions, responsive BRP, reflected viewer/process sizes, and child-side `stty size` at the final negotiated size and after detach. A larger diagnostic emulator checks frame overflow and full-width bottom chrome without silently clipping the oracle. Raw-mode child files prove input isolation across split/focus/resize; delivery is acknowledged before crossing from PTY input to a BRP focus change.
- **Paste:** bracketed-paste envelopes fragmented across PTY writes, multibyte payloads, and sizes straddling the documented 64 KiB bound, against children that do and do not request bracketed-paste mode (`DECSET 2004`). It checks ownership acknowledgement, byte-exact child delivery, explicit rejection of oversized payloads, and that an ordinary key after the end marker still reaches the pane. This scenario found a production defect, fixed in PR #30; see the finding below.
- **Keys:** every canonical xterm key encoding, one press per PTY write, sent through the real frontend to a raw-mode `cat` child that records what arrived. Plain, shifted and multibyte characters, NUL, every Ctrl letter except the prefix, the four C0 controls above Ctrl-Z, Backspace, Enter, Tab and Shift-Tab, a lone Escape (resolved through its 35 ms deadline, never merged with the next key), Alt-x, arrows, Home/End, Insert/Delete/PageUp/PageDown, F1..F12 forms, and modified variants. Delivery must be byte-identical; each press is acknowledged before the next. A seeded shuffle of the same set covers neighbour interactions. This scenario found a production defect, fixed in PR #31; see the finding below.
- **Mouse:** outer-terminal SGR mouse events sent through the real frontend to raw-mode children that requested DECSET 1000, 1002 or 1003, with or without SGR 1006, in one full-width pane or two split panes located by the marker each painted. Expectations are xterm's table, written independently of fux: pane-relative one-based coordinates including the far corner, motion filtered by mode, left/middle/right presses and releases, Ctrl and Alt bits, drags, wheel events, and no delivery for clicks on the bar or a separator. Each forwarded event is acknowledged in the child's file before the next; a final forwarded press per pane flushes anything wrongly forwarded. This scenario found a production defect, fixed in PR #32; see the finding below.
- **Copy:** keyboard and mouse-drag selections in a raw child that painted known rows. The OSC 52 payload the frontend writes to the outer terminal must decode to exactly the selected text: a single word, the tail of one row joined to the head of the next by a newline, a wide glyph whose continuation cell never appears, a soft-wrapped row joined without an invented newline, and a mouse drag without copy mode. One case starts with the clipboard enabled; the other starts disabled, expects the explicit refusal, then creates `fux.json` while the server runs and expects the watched configuration to apply.
- **History:** sixty numbered lines with 38 in history. `scroll` commands move by half the viewer height and clamp at the oldest line, scrolling back moves immediately, ordinary input returns to live output, the wheel browses without application mouse mode, copy-mode `u` pages and `k` at the top scrolls one line, a history line copies exactly and returns to live, and new output typed by a second viewer clears an anchored selection with a notice instead of copying stale text. This scenario found a production defect, fixed in PR #33; see the finding below.
- **Zoom:** two viewers of different sizes share two side-by-side panes. Zoom in one viewer shows only the focused pane at full width and leaves the other viewer's layout unchanged; shared PTY sizes are the minimum over the rectangles visible in any viewer, before, during and after zoom; a hidden pane is refused as a focus target while zoomed, with a notice and no frame change; and detaching the larger viewer leaves the hidden pane at its last size without killing either process.
- **Layout:** a pane moved to a new tab and to a new workspace, each then closed: only the unreferenced process is terminated and the viewer follows and returns. A second `PaneView` of the shell's process is spawned and reparented over the API; closing either view alone keeps the process. An interactive close confirmation whose target is closed by the API must cancel rather than retarget the newly focused pane. Closing the last pane leaves an empty tab a split can fill. Closing the only workspace must detach the frontend gracefully, terminate its process, and leave a server a new frontend can still attach to. **This scenario found a production defect, fixed in this branch; see the finding below.**
- **Process:** stock-spawned `Launch` recipes with exact argv and cwd shown through an API `PaneView`, reflected `ProcessState` dimension edits resizing the real PTY, `Launch` removal and entity despawn both terminating the child, unstartable and empty-argv launches reporting a failed status, natural exit retaining the final screen and exit code, input into an exited pane reporting, and history scrolling clamped at the oldest retained row.
- **Nav:** a known three-pane geometry checking the documented directional ranking, that edges never wrap, that `focus_last` is its own inverse, that `focus_next` visits each pane once and returns, and that swaps preserve processes and focus.
- **Scene:** `save_layout` writes a file containing no runtimes or process recipes, reloading it keeps both processes and launches nothing, and both a stale process reference and a missing file fail without replacing the current layout.
- **Config:** hot reload of the prefix, confirmed through the command column rather than through a key that could merely have passed through, the old prefix becoming ordinary input, doubled-prefix literal forwarding for a configured prefix, bindings running under it, and a malformed reload keeping the previous usable configuration.
- **Overlay:** unavailable actions explaining why without acting, unknown shortcuts leaving the command column open, rename prompts applying and cancelling, close confirmations cancelled by `n` and by Esc, a chooser cancelled by `q`, and a paste inside the command column never executing menu commands. Every step also asserts that overlay keys never reach the pane.
- **Limits:** attach dimensions clamped to 4096, the copy viewport cell cap reached through a workspace sized by one viewer alone, a copy refused while the clipboard is disabled without writing OSC 52, a zero viewport painting nothing, and the 64 KiB paste bound.
- **Chrome:** painting at eight sizes down to 2x2, asserting that nothing paints outside the requested viewport and that no wide glyph ever starts in the last column, plus the active tab remaining in the bar as it narrows to four columns.
- **Selection:** an anchored selection cleared with a visible notice when the viewer resizes or scrolls under it, and copying with no anchor reporting rather than writing OSC 52.
- **Race:** a scene load held in flight by a named pipe while a concurrent command runs, so the race is exact rather than a matter of scheduling. It covers the load's workspace being closed mid-flight, the requesting viewer detaching mid-flight, a rename prompt whose target a second viewer closes, and detaching while in copy mode. **This scenario found a production defect, fixed in this branch; see the finding below.**
- **Memory:** two viewers on one workspace switching tabs independently, per-tab focus memory restored across round trips, `focus_last` returning to the pane left behind, and a pane moved to another tab never surviving in the other viewer's memory.
- **Reorder:** tab order in both the hierarchy and the bar, reorder being a no-op at either edge, `WorkspaceOrder` staying a duplicate-free sequence across reorders and a middle workspace close, and `reorder_pane` swapping siblings and repainting.
- **Scene map:** layout loads with an explicit old-to-existing mapping, and duplicate, layout-entity and missing-target mappings each failing without replacing the current layout, then the configured `layout:` reload path replacing a same-named workspace and adding a renamed one. **This scenario found a production defect, fixed in this branch; see the finding below.**
- **Mouse edge:** pane-relative coordinates past the legacy encoding's 223-column limit dropped rather than wrapped, the same coordinate delivered exactly under SGR, Shift-right-click opening fux's pane menu while the application owns the mouse, wheel events on the bar never reaching the pane, and API coordinates outside the viewer ignored without an error.
- **Clipboard queue:** sixteen queued copies accepted and the seventeenth refused with the documented message, one paint delivering all sixteen and the next delivering none, and a reload that disables the clipboard dropping what was pending instead of writing it.
- **Resize commands:** Ctrl+arrow pane resizing conserving rows and columns, never pushing a sibling below the 2-cell backing minimum, leaving the other axis untouched, and a no-op axis reporting nothing. The height case uses a fresh tab because a pane carries one flex factor for whichever axis its container uses.
- **API misuse:** unsupported key names, unknown input and command kinds, a close with no subject, a retired paired command kind and a mouse action outside the enum all rejected when they deserialize, with a JSON-RPC error and no change to the notice; a despawned target reporting "target no longer exists" while a live entity of the wrong kind reports otherwise; and zero and oversized viewports.
- **Signal:** SIGINT, SIGTERM and SIGHUP delivered to an attached frontend. It must exit successfully, restore termios and the alternate screen, and its viewer must disappear while the server is otherwise idle (only read-only queries), without killing the shared process. On failure the case records whether an unrelated paint would have removed the viewer, separating a lost detach from a lost server.
- **Shutdown:** a SIGTERM after the pre-`app.run` announcement but before waiting for readiness; pane termination and separate bash background-job cleanup; natural exit with retained output/status; graceful detach with both termios and alternate-screen restoration; abrupt viewer loss without killing the shared process; server shutdown during `yes` output while the outer PTY is temporarily unread.

Paste sizes are counted in UTF-8 payload bytes, excluding the terminal's `\e[200~`/`\e[201~` framing, matching `paste::LIMIT`. An accepted paste must arrive byte-exact, and a rejected paste must deliver nothing at all rather than a truncated prefix. The oracle is validated by passing cases on both sides of the boundary, including a payload delivered with the 12-byte envelope intact.

A one-row viewer has chrome but no visible pane-size constraint. A fully hidden process retains its previous size. A supported zero-size API viewer must paint nothing; OS PTYs are only resized to positive dimensions. The application and pinned vt100 require at least a 2×2 backing grid. The harness's diagnostic frontend parser also uses that minimum to avoid vt100's tiny-grid underflow; **the actual PTY ioctls still receive the exact requested tiny dimensions**. The separate frame-bounds oracle uses a larger grid to detect overflow.

Readiness checks the initial pane's working directory before mutating the endpoint, so a foreign fux winning the ephemeral-port release/bind race is a setup collision, not a target to exercise. There is no port/scenario retry loop.

The initialization shutdown case samples the spawn-to-readiness window, not a deterministic internal startup barrier: OS scheduling can allow initialization to finish before the signal arrives. No production hooks were added to manufacture an ordering.

## Trace and evidence

Each invocation creates a fresh directory under `fux-fuzz/runs/` (override with `--output`). It retains:

- `trace.json`: a versioned list of scenario actions, including every concrete resize pair and input token. All generation happens before execution. Replay reads this list; it does **not** regenerate choices from the seed.
- `metadata.json`: seed, limits, controlled environment policy, OS/architecture, `uname`, local Rust/Cargo versions, and the fux binary's canonical path, size and streaming FNV-1a identity hint (not a cryptographic digest).
- `summary.json`: case outcomes and durations, including cases not executed because the overall deadline was exhausted.
- `replay.txt`: a shell-quoted, copyable command with the original binary and trace paths.
- Failed case directories: a flushed `events.jsonl` journal of intended RPC/input/resize actions and observations; bounded server stdout/stderr tails; frontend ANSI tails and final diagnostic screens **only if captured**; controlled fixture files such as input/PID/size files.

Fixed setup/assertion steps belong to the built-in scenario recipes; traces are versioned (1: startup/resize/shutdown, 2: adds paste, 3: adds keys, signal, mouse, copy, history, zoom, layout, process, nav, scene, config, overlay, limits, chrome, selection, race, memory, reorder, scene_map, mouse_edge, clipqueue, resize_cmd and api_misuse) and every earlier version still loads and validates. Use the same harness revision to replay them. New ports, directories, entity IDs and process IDs are necessarily rebound to the new fixture. The event journal records those runtime values. A seed or saved action trace reproduces choices, **not OS scheduling**. No arbitrary sleeps are generated; short sleeps only pace bounded observation loops.

Successful case directories are removed after cleanup. Their plan, summary, metadata and replay command remain, normally a few KiB for smoke. Failed case directories remain for diagnosis. No automatic pruning of prior invocations: remove a specific run directory when done. No real HOME, caches, target tree, or arbitrary temporary directories are copied; child HOME is the freshly created fixture directory with a cleared environment.

## Bounds and cleanup

- Default: one iteration, six generated resize pairs, 120-second overall action budget. CLI maxima: 100 iterations, 200 generated pairs per resize case, 600 seconds. Fixed tiny/final pairs are additional; at most 700 cases and 210 pairs per resize case. Replay is validated against the same caps.
- Operations: five-second monotonic observation/write/reap bounds; HTTP requests at most 500 ms and 1 MiB per response. Overall deadlines and SIGINT/SIGTERM/SIGHUP interruption are checked between operations. An in-flight bounded RPC may finish after its enclosing observation deadline.
- At most three frontend handles per case, at most two live attached viewers, and a small fixed process scenario. No harness reader threads or unbounded queues. Nonblocking PTY/pipe pumps read at most 64 KiB per pass so hot output cannot starve observations.
- Each capture retains only its last 64 KiB plus a total byte count. Each case event log is capped at 4 MiB; hitting the cap fails the case. Traces loaded for replay are capped at 4 MiB. Fixture input commands and files are bounded by the validated recipes. These are cooperative test limits, **not an OS sandbox for a malicious executable**.
- Cleanup runs even after assertion failure/interruption and has its own bounded grace beyond the action budget: up to five seconds per frontend, five seconds for server SIGTERM, five for fallback kill/reap, five for child/group disappearance. With at most three frontend handles, the normal cleanup path has a 30-second worst-case allowance; OS-level uninterruptible processes cannot be guaranteed reapable.
- Signal only directly owned, unreaped server/frontend handles. Observe recorded child PIDs and original groups with signal 0; never kill a cached descendant PID or use broad process-name cleanup. Record remaining children/groups as cleanup failures rather than concealing them. If fux itself cannot clean its children before a forced server kill, the harness reports that limitation; it is not a descendant-containment service.
- The job-propagation fixture explicitly execs `/bin/bash --noprofile --norc -i`. Ubuntu's dash `/bin/sh` does not provide bash's background-job SIGHUP propagation. No claim is made about disowned jobs or other groups that ignore hangup, nor about terminal restoration after SIGKILL.

## Harness checks

```sh
cargo fmt --manifest-path fux-fuzz/Cargo.toml --all --check
cargo clippy --manifest-path fux-fuzz/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path fux-fuzz/Cargo.toml --locked
```

Six unit tests check deterministic generation, trace round-tripping/validation, deadline/interruption checks, bounded capture, the frame oracle, and the paste payload/envelope oracle. Three subprocess tests use deliberately faulty **fixtures, not modified fux code**: early exit, hanging startup, and interrupted startup. They assert nonzero status, bounded termination, retained diagnostics, no invented frontend capture, and disappearance of the fixture PID after cleanup.

## Finding: writing a layout scene corrupted the live hierarchy (fixed)

The scene_map scenario failed against `406b1f2` (merged `main` including PR #35). The fix is included in this branch.

Writing a layout scene inserts an entity's components one at a time, so a tab briefly has its `ChildOf` before it has `Tab`. The normalize-on-child-added observer saw a loose child directly under a workspace and wrapped it in a fresh tab, leaving the scene's own tab nested inside that wrapper:

```
Workspace
└── Tab "main"          <- wrapper added by normalization
    └── Tab "main"      <- the scene's own tab
        └── Split → two PaneViews
```

`extract_layout` rejects that with `tabs must be direct workspace children`, and because every painted frame extracts the workspace layout, **every** `fux.frame` request then failed permanently and every attached frontend was killed with a JSON-RPC error. The server had to be restarted.

Reproduced outside the harness in five steps: attach, split, `save_layout`, `load_layout`, `save_layout` again, then set `layout:` in `fux.json`. The fix suspends both normalization observers for the duration of the scene write with an `ApplyingLayout` guard; `apply_layout` already normalizes once when the write is complete.

## Finding: a layout load outlived the workspace it was loading into (fixed)

The race scenario failed against the same commit. The fix is included in this branch.

`load_layout` checks its workspace when the request arrives, then reads the file on the I/O pool. Closing that workspace before the read finished did not stop the completion: `replace_workspace` found no viewers on the dead root, its despawn was a no-op, and a workspace nobody asked for was added while the viewer was told `loaded <path>`. The scenario makes this exact rather than timing-dependent by pointing the load at a named pipe and writing to it only after the close has been observed. The fix re-checks the workspace on the ECS thread before deserializing and reports `target no longer exists`, matching every other command that names a missing entity.

## Earlier finding: a server with no workspace refused every attach (fixed in PR #34)

The layout scenario failed against `3811746` (merged `main` including PR #33). The fix is included in this branch.

Closing the only workspace detached its viewers as documented and terminated the unreferenced process, but the server kept running with zero workspaces. Every later `fux attach` failed with `workspace not found` and the frontend exited immediately, so the server was unusable until restarted. `fux.attach` now runs the same initialization as startup when no workspace exists: the `main` workspace, its tab and the configured shell are created, then the viewer attaches to them. Every earlier step of the scenario passed unchanged: moves, tab and workspace closes, shared references, the cancelled stale confirmation, and the empty tab.

## Earlier finding: history scrolling accumulated an offset past retained history (fixed in PR #33)

The history scenario failed against `c9e8d2f` (merged `main` including PR #32).

`scroll` added or subtracted half a viewer height to `Viewer.scrollback` without regard to how much history the emulator retains. With 38 lines of history, seven `previous` commands left the offset at 84 while the view was clamped at LINE-001. The next `next` command lowered it to 72 and the view did not move; a user had to press it four more times before anything happened. Copy mode already stored the emulator's actual offset, so keyboard paging behaved differently from command and wheel scrolling.

| Step | `Viewer.scrollback` before | after |
| --- | --- | --- |
| 7 × `previous` | 84 (view at LINE-001) | 38 (view at LINE-001) |
| then `next` | 72 (view still at LINE-001) | 26 (view at LINE-013) |

The fix clamps the requested offset through the focused pane's emulator so command, wheel and copy-mode scrolling agree.

## Earlier finding: legacy mouse releases lost their modifier bits (fixed in PR #32)

The mouse scenario failed against `f8a9286` (merged `main` including PR #31) in both legacy-encoding cases.

xterm's normal tracking mode (`1000`/`1002`/`1003` without `1006`) encodes a release as button 3 plus the same Shift/Meta/Control bits the press carried: Ctrl-release is `ESC [ M 3 x y` (3 + 16 + 32). fux emitted a bare 3, so an application saw Ctrl-release and Alt-release as a plain release:

| Sent (outer SGR) | Delivered before | Delivered now |
| --- | --- | --- |
| `ESC [ < 16 ; 1 ; 2 m` (Ctrl-release) | `ESC [ M # ! "` | `ESC [ M 3 ! "` |
| `ESC [ < 8 ; 2 ; 1 m` (Alt-release) | `ESC [ M # " !` | `ESC [ M + " !` |

The SGR path already kept modifiers on release, and every other event in the set (44 of 46 in the split case) was delivered exactly, including pane-relative coordinates for the second pane and nothing for bar or separator clicks. The fix keeps the modifier bits on legacy release; a unit test pins both encodings.

## Earlier finding: Ctrl-\, Ctrl-], Ctrl-^ and Ctrl-_ reached the pane as Ctrl-T..Ctrl-W (fixed in PR #31)

The keys scenario failed against `8f64d5c` (merged `main` including PR #30).

Termina names the C0 controls above Ctrl-Z after the digit xterm sends them for: bytes `0x1c..=0x1f` decode as Ctrl-4..Ctrl-7. fux re-encoded a Ctrl character as its uppercase form masked with `0x1f`, which maps `'4'..='7'` to `0x14..=0x17`. So the four presses arrived at the application as Ctrl-T, Ctrl-U, Ctrl-V and Ctrl-W:

| Sent | Decoded as | Delivered before | Delivered now |
| --- | --- | --- | --- |
| `0x1c` (Ctrl-\, SIGQUIT in cooked mode) | Ctrl-4 | `0x14` (Ctrl-T) | `0x1c` |
| `0x1d` (Ctrl-], telnet escape) | Ctrl-5 | `0x15` (Ctrl-U) | `0x1d` |
| `0x1e` (Ctrl-^) | Ctrl-6 | `0x16` (Ctrl-V) | `0x1e` |
| `0x1f` (Ctrl-_, undo in Emacs and readline) | Ctrl-7 | `0x17` (Ctrl-W) | `0x1f` |

The other 60 encodings in the set round-tripped byte-exact, including NUL (Ctrl-Space), every Ctrl letter, and every escape-prefixed form, so the mismatch is confined to the encoder's control table. The fix encodes Ctrl through xterm's table: Ctrl-2 is NUL, Ctrl-3 is Escape, Ctrl-4..Ctrl-7 are `0x1c..=0x1f`, Ctrl-8 and Ctrl-? are DEL, Ctrl-0/1/9 send the digit itself, and letters/punctuation are unchanged. A unit test now pins every C0 control.

The same branch also corrects the frontend's explicit detach request after a signal-driven exit, which still used the pre-`Command` wire shape and was rejected by the server. The signal scenario passed before that fix because the server also detaches a viewer when its watch stream closes; the scenario retains the explicit-detach expectation so a regression in either path is caught.

## Earlier finding: a valid paste was accepted, then dropped (fixed in PR #30)

The paste scenario failed against `addd9052fee737e23dc55e36c26e52827af1bffe`.

When the focused application had requested bracketed-paste mode, a paste that fux's own policy accepted could still be discarded by its PTY write path:

- `paste::LIMIT` is `64 * 1024`, and `paste::input` rejects only `text.len() > LIMIT`, so a 65,536-byte payload is explicitly accepted.
- `server::terminal_input` then wraps it as `\e[200~{text}\e[201~`, adding 12 bytes.
- `Terminal::input` rejected anything over `MAX_INPUT` (then 65,536), so the wrapped write failed.

The effective bracketed limit was therefore 65,524 payload bytes. Payloads of 65,525..=65,536 bytes are accepted by the policy layer and then dropped whole: the child receives **zero** bytes, and the bar shows the internal message `input exceeds 65536 bytes; send smaller chunks`, which misattributes a fux-generated envelope to the user's paste and is not actionable within the documented 64 KiB bound.

Observed boundary, all with the same payload delivered to a real `cat` child:

| Payload bytes | Child requested `2004` | Result |
| --- | --- | --- |
| 65,524 | yes | delivered byte-exact, envelope included |
| 65,525 | yes | **dropped, 0 bytes** |
| 65,535 (`界` × 21,845) | no | delivered byte-exact |
| 65,535 (`界` × 21,845) | yes | **dropped, 0 bytes** |
| 65,536 | no | delivered byte-exact |
| 65,536 | yes | **dropped, 0 bytes** |
| 65,537 | no / yes | correctly refused with `paste exceeds 64 KiB; discarded` |

The same size succeeds or fails purely on whether the application requested the mode, so this is an inconsistency between two layers' limits rather than an intentional bound. On this machine `zsh` and `vim` both request `2004`; `/bin/bash` 3.2 does not, which is why non-bracketed cases pass.

The fix frames the envelope in `paste::bracketed` and sizes the transport budget as `paste::LIMIT + paste::ENVELOPE` (65,548 bytes), so the accepted payload bound and the delivered bound agree. Pastes are neither truncated nor split; the 64 KiB payload bound is unchanged. A unit test pins the largest accepted paste plus envelope to the transport budget, and the paste scenario passes all ten cases.

## Earlier finding and verification

Verified locally on macOS arm64, Darwin 27.0.0, Rust/Cargo 1.98.1. **Linux execution is not verified.** No hosted workflow was added.

The root gates pass: fmt, strict Clippy, 51 unit tests and 35 integration tests (including idle asset wake/parking and a one-shot initialization regression). The independent harness gates pass: fmt, strict Clippy, six unit tests and three controlled-failure integration tests.

Before the production fix, full smoke and saved-trace replay each observed six passing cases and one valid-config startup failure. The first recipe was `default-shell` and the terminal printed `DEFAULT-SHELL`, even though `fux.json` named `configured-shell`. Missing/malformed config correctly used the default. After the fix, replay of that exact seven-case trace passes in 3.92 seconds; a seed-42 run covering the three original scenarios for five iterations passes all 35 cases in 16.00 seconds. The unchanged resize-only stress and saved-trace replay also passed before the fix. Process cleanup is checked after every case. A standalone hanging fixture failed in approximately two seconds, retained its stderr/trace, and was reaped.

The startup contract is now explicit: **a valid startup configuration governs the first pane**. Previously, default `Settings` were used by `server::initialize` in Startup before the asynchronously loaded asset was applied. At the user's request, this PR now also fixes that ordering: initial configuration settlement is latched on either successful application or failure, then one-shot initialization runs after settings application in PostUpdate. Early BRP requests wait until that initial workspace exists. A wake settles the new Launch's native PTY on the next update even without another request.

Reloads do not close the readiness gate, recreate the initial workspace, or replace a running shell. Missing/invalid initial configuration still falls back to defaults; shutdown signals remain responsive while loading. The native asset pending/wake bridge is unchanged. The regression checks waiting updates, configured first launch and no restart on reload; real harness cases check both fallback paths and startup shutdown. No existing dependency, toolchain pin, controls or API shape changed.
