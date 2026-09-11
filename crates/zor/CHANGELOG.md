# Changelog

## 0.3.0 - 2026-09-11

Breaking release: zor is the agent layer only; it no longer emulates terminals or wraps PTYs.

- Removed the standalone PTY wrapper (`zor <command>`, the `--events`/`--title`/`--no-osc`/
  `--debug` wrapper flags, `ZOR_PID` nested detection, OSC 7877 emission into a passthrough
  stream). It lives on as the separate `zor-wrap` crate and binary (`zor-wrap <command>`);
  `zor` has no exec shim for it.
- fux observation consumes `capture format:"cells"` and builds the rule view directly; the
  vt100 re-emulation of text captures is gone, and `vt100` is no longer a runtime dependency
  (it remains a dev-dependency for two tests of zor's own output). Progress comes from the
  capture; zor's OSC 9;4 parser moved to `zor-wrap`.
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
