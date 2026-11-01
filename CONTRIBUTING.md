# Contributing to fux

- **A PR that changes an interface updates the recipes it touches** in
  [docs/cookbook.md](docs/cookbook.md) (`fux help cookbook`), in the same
  PR. `tests/cookbook.rs` runs every recipe on a private server, so a
  stale one fails the tests.
