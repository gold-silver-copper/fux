# Changelog

## 0.5.0 - 2026-09-11

- Requires fux 0.10.0: `input-reserve` and `split` carry zor's retention policy under fux's
  published ceilings (`info.limits.input_retention_ms` = 600 000, `final_retention_ms` =
  14 400 000; `crates/zor/src/fux.rs` mirrors both and a test pins them to fux's `info` reply
  fixture). zor clamps first, so a receipt's `expires_ms` and a record's lifetime are exactly
  what zor asked for.
- `task submit` reserves input with `retain_ms` = the prompt's remaining window plus one
  reconcile round (10 s): zor reads the receipt through `input-status` on every reconcile until
  delivery is settled, on a late binding and on arm retirement, all bounded by the prompt
  deadline.
- `zor run` splits with `final_retain_ms` = `--timeout` plus a 5 s poll margin: the run polls
  `final` every 25 ms until its deadline and never reads the record after it.
- A managed launch splits with `final_retain_ms` = fux's ceiling (four hours): its exit is read
  by the service's recovery loop, by `wait`/`follow` or after a service restart, an open-ended
  horizon on zor's side, so the ceiling is the documented bound on how long a supervisor may be
  away before the exit evidence is gone.

## 0.4.0 - 2026-09-11

- New `zor run [--timeout MS] [--rows R] [--columns C] [--env K=V ...] [--cwd DIR]
  [--workspace NAME] -- <command> [args...]`, moved from fux 0.9.0's `fux run` with the same
  observable behavior: a throwaway workspace is created create-only on the fux session server
  (started from `fux` on PATH when none is running), the command runs in a pane of the given
  size and environment, the retained `final` record supplies the final screen and exit status,
  the screen is printed, the process exits with the command's status, and the owned workspace
  is killed whatever the outcome (which also ends a timed-out command's descendants). It is
  task-free and does not use `zor serve`. Unknown or expired evidence and a truncated final
  screen are errors; an existing workspace name is refused, never borrowed.
- fux connections, request writes, reply framing and the service client/startup channel use
  local-ipc 0.2.0's `connect_until`, `write_all_until` and `FrameReader`; connect now waits
  until the caller's full deadline instead of a single 2 s poll. zor derives fux's runtime
  directory (`$XDG_RUNTIME_DIR/fux`, macOS `~/Library/Caches/fux-runtime/fux`) from the same
  shared function fux uses, so the two cannot diverge; zor's own service directory naming is
  unchanged.


## 0.3.2 - 2026-09-11

- The wrapper is the bare form again: `zor [flags] <program> [args...]` runs the program in a
  pseudoterminal (like `sudo`, `env` or `time`), `zor` alone wraps `$SHELL -l`, and
  `zor -- <program>` forces wrapping when the program is named like a zor subcommand. The
  `wrap` subcommand is removed; its `--events`, `--title`, `--no-osc` and `--debug` options
  are top-level flags. A build without the `wrap` feature reports that the wrapper is not
  compiled in instead of a generic unknown-subcommand error.

## 0.3.1 - 2026-09-11

- `wrap` is a default feature again: `cargo install zor` includes `zor wrap <command>`. fux
  builds zor for its own use with `--no-default-features --features cli`, which has no
  wrapper, no `vt100`, and no `wrap` subcommand.

## 0.3.0 - 2026-09-11

Breaking release: zor is the agent layer only; it no longer emulates terminals or wraps PTYs.

- The PTY wrapper is now the off-by-default `wrap` Cargo feature and the `zor wrap <command>`
  subcommand (`cargo install zor --features wrap`), carrying the former `--events`/`--title`/
  `--no-osc`/`--debug` flags, `ZOR_PID` nested detection and OSC 7877 emission into the
  passthrough stream. The bare `zor <command>` form is gone, and a default build has no `wrap`
  subcommand or `vt100` dependency.
- fux observation consumes `capture format:"cells"` and builds the rule view directly; the
  vt100 re-emulation of text captures is gone, and `vt100` is no longer a runtime dependency
  (it remains a dev-dependency for two tests of zor's own output, and a runtime dependency
  only under `wrap`). Progress comes from the capture; zor's OSC 9;4 parser lives in the
  wrapper's `screen` module.
- Unknown fux event kinds are ignored instead of ending the subscription; their cursor still
  counts toward continuity.
- The local-socket discipline of `zor serve` and the fux client uses the shared `local-ipc`
  crate; no wire, path, permission or limit changed. Connect and peer-check failures are now
  reported as `io::Error` text (for example `No such file or directory (os error 2)`) instead
  of `nix` errno text.
- Requires fux 0.8.0 (cells capture).

## 0.2.0 - 2026-09-09

- Consume fux's unversioned local protocols: the four-byte `FUX\n` preface, pane creation
  through a scoped horizontal split, and incarnation, stream and input-sequence evidence in
  watch and retained-final consumers. Older fux servers are not supported.
- zor now lives in the fux repository as `crates/zor` (one workspace, CI and gate with fux);
  the standalone repository is archived.

## 0.1.2 - 2026-09-04

- Propagate nested PTY resizes reliably.
- Reap the complete nested process group when the wrapper terminates.

## 0.1.1 - 2026-09-03

- Fix Linux process-child parsing on stable Rust by making the PID type explicit.

## 0.1.0 - 2026-09-03

- Define and test the OSC 7877 parser and formatter.
- Add ordered PTY passthrough with transparent nested-wrapper execution.
- Add terminal screen observation and streaming ECMA-48 boundary tracking.
- Add rule schema, region evaluation, process identification, and hysteresis primitives.
- Add title, OSC, and JSON Lines event encoders.
