# Changelog

## 0.3.0 - 2026-09-14

- `read_exact_until`: full read of a fixed-size buffer under one absolute deadline (added after
  0.2.0 was published, which is why 0.2.0 tarballs could not build fux).
- `absolute_path`: the shared "absolute or relative to the working directory" resolution that
  `runtime_directory_from` uses, so callers stop duplicating it.
- `Connecting` is crate-private; `connect_until` is the public entry point.
- `BoundSocket::path` removed (no caller outside the crate's tests).

## 0.2.0 - 2026-09-11

- `connect_until`: deadline-bounded connect over `Connecting` (poll loop to an absolute
  deadline, `EINTR` retried, `TimedOut` past the deadline).
- `write_all_until`: full write of a byte slice under one absolute deadline, restoring the
  descriptor's status flags.
- `FrameReader` and `FrameError`: newline-delimited frames with a maximum payload size and a
  per-call absolute deadline, retaining partial data across polls; `new` reads in 8 KiB chunks,
  `bytewise` reads one byte at a time so a reader may be dropped between frames.
- `runtime_directory` / `runtime_directory_from`: `$XDG_RUNTIME_DIR/<name>`, else on macOS
  `$HOME/Library/Caches/<name>-runtime/<name>`.
- `nix` now enables `poll`.

## 0.1.0 - 2026-09-11

- First release: same-user local Unix-socket discipline shared by fux and zor (private 0700
  directory checks, 0600 socket with inode-scoped cleanup, peer-uid verification, random
  token, nonblocking connect initiation).
