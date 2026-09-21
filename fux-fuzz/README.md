# fux-fuzz

An unpublished, opt-in **black-box scenario harness** for an already-built fux. It uses real servers, attached terminal viewers, PTYs, and the public BRP API. It is neither coverage-guided fuzzing nor a claim that fux survives everything.

This is an independent crate, not a workspace member. Root `cargo test --locked` does not build or run it. There is no new CI gate. Run the short smoke locally; request longer stress runs when useful.

## Build and run

From the repository root, with the pinned Rust toolchain:

```sh
cargo build --locked
cargo build --manifest-path fux-fuzz/Cargo.toml --locked

# Smoke: all three scenarios (seven isolated cases).
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux

# Individual scenarios:
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario startup
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario resize
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux --scenario shutdown

# Explicit, bounded stress: 10 fresh fixtures, 30 generated resize pairs each.
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux \
  --scenario resize --seed 42 --iterations 10 --actions 30 --seconds 120

# Substitute the trace path printed by a previous run:
fux-fuzz/target/debug/fux-fuzz --fux target/debug/fux \
  --replay /path/to/run/trace.json --output /tmp/fux-fuzz-replay
```

`--fux` is mandatory and canonicalized before any child changes directory. No run implicitly builds fux or searches PATH for an installed fux. Use a trusted local binary: the application API permits unrestricted same-user command execution.

**The startup smoke currently fails against base `cac0def02caedad8c60b6bd4357e1be013999da1`.** That is a retained application finding, not a skipped test or a green harness claim. Exit 1 means at least one scenario, setup, diagnostic, cleanup, interruption, or deadline failed. Other cases still execute within the overall budget; no failed case is retried. A dependency panic during scenario execution is reported as a harness/dependency failure, not an application defect.

## What the scenarios check

- **Startup:** missing, malformed, and valid configuration; real frontend attach immediately after read-only readiness checks; first `Launch.argv` and its actual terminal output. Distinct executable wrappers identify the configured and environment-default shell. No settling sleep precedes the initial observation.
- **Resize:** two real viewers of a shared process, rapid PTY resize bursts, settled tiny viewports, conflicting dimensions, responsive BRP, reflected viewer/process sizes, and child-side `stty size` at the final negotiated size and after detach. A larger diagnostic emulator checks frame overflow and full-width bottom chrome without silently clipping the oracle. Raw-mode child files prove input isolation across split/focus/resize; delivery is acknowledged before crossing from PTY input to a BRP focus change.
- **Shutdown:** a SIGTERM after the pre-`app.run` announcement but before waiting for readiness; pane termination and separate bash background-job cleanup; natural exit with retained output/status; graceful detach with both termios and alternate-screen restoration; abrupt viewer loss without killing the shared process; server shutdown during `yes` output while the outer PTY is temporarily unread.

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

Five unit tests check deterministic generation, trace round-tripping/validation, deadline/interruption checks, bounded capture, and the frame oracle. Three subprocess tests use deliberately faulty **fixtures, not modified fux code**: early exit, hanging startup, and interrupted startup. They assert nonzero status, bounded termination, retained diagnostics, no invented frontend capture, and disappearance of the fixture PID after cleanup.

## Initial finding and verification

Verified locally on macOS arm64, Darwin 27.0.0, Rust/Cargo 1.98.1. **Linux execution is not verified by this PR.** No hosted workflow was added, and the existing CI PR was neither merged nor used as the base.

The root gates pass unchanged: fmt, strict Clippy, 50 unit tests and 35 integration tests (including idle asset wake/parking). The independent harness gates pass: fmt, strict Clippy, five unit tests and three controlled-failure integration tests. See the PR body for final command timings and local bundle paths.

The full smoke and saved-trace replay each observed six passing cases and one valid-config startup failure. The first recipe was `default-shell` and the terminal printed `DEFAULT-SHELL`, even though `fux.json` named `configured-shell`. Missing/malformed config correctly used the default. Resize stress (seed 42, ten cases × thirty generated pairs) and saved-trace replay passed; process cleanup was checked after every case. A standalone hanging fixture failed in approximately two seconds, retained its stderr/trace, and was reaped.

The README describes configured shells and fallback for missing/invalid config, but does not explicitly define the first-pane asynchronous-loading boundary. This harness makes the proposed user-facing contract explicit: **a valid startup configuration should govern the first pane**. The implementation currently installs default `Settings`, runs `server::initialize` in Startup, and only later applies the asynchronously loaded asset. The failure is not excused by inspecting a later split or sleeping before observation.

Suggested separate production follow-up: define that startup contract, delay only initial pane creation until initial settings loading has settled (success or failure), preserve fallback for missing/invalid config and bounded signal-driven shutdown, and retain the idle asset wake regressions. This PR changes neither the production initialization path nor any existing dependency/toolchain/control/API contract.
