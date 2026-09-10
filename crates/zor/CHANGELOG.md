# Changelog

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
