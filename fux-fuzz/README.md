# fux-fuzz

An unpublished, opt-in **black-box scenario harness** for an already-built fux. It uses real servers, attached terminal viewers, PTYs, and the public BRP API. It is neither coverage-guided fuzzing nor a claim that fux survives everything.

This is an independent crate, not a workspace member. Root `cargo test --locked` does not build or run it. There is no new CI gate. Run the short smoke locally; request longer stress runs when useful.

## Build and run

From the repository root, with the pinned Rust toolchain:

```sh
cargo build --locked
cargo build --manifest-path fux-fuzz/Cargo.toml --locked

# Smoke: all four scenarios (seventeen isolated cases).
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux

# Individual scenarios:
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario startup
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario resize
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario shutdown
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario paste

# Explicit, bounded stress: 10 fresh fixtures, 30 generated resize pairs each.
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux \
  --scenario resize --seed 42 --iterations 10 --actions 30 --seconds 120

# Substitute the trace path printed by a previous run:
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux \
  --replay /path/to/run/trace.json --output /tmp/fux-fuzz-replay
```

`--fux` is mandatory and canonicalized before any child changes directory. No run implicitly builds fux or searches PATH for an installed fux. Use a trusted local binary: the application API permits unrestricted same-user command execution.

The paste scenario found a bracketed-paste envelope defect against merged `main`, fixed in this branch; see "Finding" below. An earlier first-pane configuration bug this harness found was fixed in PR #29; its original saved failure trace still passes. Exit 1 means at least one scenario, setup, diagnostic, cleanup, interruption, or deadline failed. Other cases still execute within the overall budget; no failed case is retried. A dependency panic during scenario execution is reported as a harness/dependency failure, not an application defect.

## What the scenarios check

- **Startup:** missing, malformed, and valid configuration; real frontend attach immediately after read-only readiness checks; first `Launch.argv` and its actual terminal output. Distinct executable wrappers identify the configured and environment-default shell. No settling sleep precedes the initial observation.
- **Resize:** two real viewers of a shared process, rapid PTY resize bursts, settled tiny viewports, conflicting dimensions, responsive BRP, reflected viewer/process sizes, and child-side `stty size` at the final negotiated size and after detach. A larger diagnostic emulator checks frame overflow and full-width bottom chrome without silently clipping the oracle. Raw-mode child files prove input isolation across split/focus/resize; delivery is acknowledged before crossing from PTY input to a BRP focus change.
- **Paste:** bracketed-paste envelopes fragmented across PTY writes, multibyte payloads, and sizes straddling the documented 64 KiB bound, against children that do and do not request bracketed-paste mode (`DECSET 2004`). It checks ownership acknowledgement, byte-exact child delivery, explicit rejection of oversized payloads, and that an ordinary key after the end marker still reaches the pane. **This scenario found a production defect, fixed in this branch; see the finding below.**
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

Fixed setup/assertion steps belong to version 1 of the built-in scenario recipes. Use the same harness revision to replay them. New ports, directories, entity IDs and process IDs are necessarily rebound to the new fixture. The event journal records those runtime values. A seed or saved action trace reproduces choices, **not OS scheduling**. No arbitrary sleeps are generated; short sleeps only pace bounded observation loops.

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

## Finding: a valid paste was accepted, then dropped (fixed)

The paste scenario failed against `addd9052fee737e23dc55e36c26e52827af1bffe`. The fix is included in this branch.

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

The fix frames the envelope in `paste::bracketed` and sizes the transport budget as `paste::LIMIT + paste::ENVELOPE` (65,548 bytes), so the accepted payload bound and the delivered bound agree. Pastes are neither truncated nor split; the 64 KiB payload bound is unchanged. A unit test pins the largest accepted paste plus envelope to the transport budget, and the paste scenario now passes all ten cases; full smoke passes 17/17.

## Earlier finding and verification

Verified locally on macOS arm64, Darwin 27.0.0, Rust/Cargo 1.98.1. **Linux execution is not verified.** No hosted workflow was added.

The root gates pass: fmt, strict Clippy, 51 unit tests and 35 integration tests (including idle asset wake/parking and a one-shot initialization regression). The independent harness gates pass: fmt, strict Clippy, six unit tests and three controlled-failure integration tests.

Before the production fix, full smoke and saved-trace replay each observed six passing cases and one valid-config startup failure. The first recipe was `default-shell` and the terminal printed `DEFAULT-SHELL`, even though `fux.json` named `configured-shell`. Missing/malformed config correctly used the default. After the fix, replay of that exact seven-case trace passes in 3.92 seconds; a seed-42 run covering the three original scenarios for five iterations passes all 35 cases in 16.00 seconds. The unchanged resize-only stress and saved-trace replay also passed before the fix. Process cleanup is checked after every case. A standalone hanging fixture failed in approximately two seconds, retained its stderr/trace, and was reaped.

The startup contract is now explicit: **a valid startup configuration governs the first pane**. Previously, default `Settings` were used by `server::initialize` in Startup before the asynchronously loaded asset was applied. At the user's request, this PR now also fixes that ordering: initial configuration settlement is latched on either successful application or failure, then one-shot initialization runs after settings application in PostUpdate. Early BRP requests wait until that initial workspace exists. A wake settles the new Launch's native PTY on the next update even without another request.

Reloads do not close the readiness gate, recreate the initial workspace, or replace a running shell. Missing/invalid initial configuration still falls back to defaults; shutdown signals remain responsive while loading. The native asset pending/wake bridge is unchanged. The regression checks waiting updates, configured first launch and no restart on reload; real harness cases check both fallback paths and startup shutdown. No existing dependency, toolchain pin, controls or API shape changed.
