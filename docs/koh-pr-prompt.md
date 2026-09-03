# Prompt: koh 0.10.0 — MIT relicense and a clap-free library surface for fux

Paste the section below into a Claude Code session opened in the koh repo. Audited against koh
0.9.1 at 6c84ffe on 3 Sep 2026.

---

Open a PR against `main` titled **"0.10.0: relicense to MIT, add `cli` feature and clap-free config
types"**. Work on a branch named `fux-surface`. This release exists so that a downstream crate,
`fux` (MIT), can depend on koh as a library, construct a serve/connect session without going
through clap, and point the server's pty at an arbitrary argv instead of a shell path. Read
`src/lib.rs`, `src/main.rs`, `src/server/cli.rs`, `src/client/cli.rs`, `src/keycmd.rs`,
`src/pty.rs`, `src/server/session.rs`, `Cargo.toml`, `CHANGELOG.md`, `examples/chaos.rs`,
`tests/e2e_pty_binary.rs` and `.github/workflows/ci.yml` before changing anything.

Constraints that hold throughout: `unsafe_code = "forbid"`, `dead_code = "deny"`, the clippy
panic-prevention denies in `Cargo.toml`, and the layering guard in CI (`predict.rs` imports nothing
from `crate::`; `server`/`client` never `use crate::wire`). Nothing here should need to touch
`predict`, `ssp`, `wire` or `transport_iroh`.

## 1. Relicense to MIT

`git shortlog -sne` shows a single author, so no consent round is needed.

- `Cargo.toml`: `license = "MIT"`.
- Replace the contents of `COPYING` with the MIT license text, copyright the current year and the
  name in `authors`. Keep the file name `COPYING`; the `exclude` comment in `Cargo.toml` says it
  ships, and that stays true.
- `README.md` "License" section (line 88): replace `GPL-3.0-or-later.` with MIT and one sentence
  saying releases before 0.10.0 remain available under GPL-3.0-or-later.
- `CHANGELOG.md` header: it says the library API is internal and unstable. Amend it to say the
  config types and entry points in section 3 are covered by semver from 0.10.0.
- `grep -rni "gpl" --exclude-dir=target .` and fix every remaining mention. `deny.toml` is
  advisories-only and has no license policy, so it needs no change; confirm rather than assume.

## 2. Add a `cli` feature that owns clap

Goal: `cargo clippy --lib --no-default-features --features backend-termina` compiles the library
with no clap in the dependency tree. `cargo install koh` is unchanged.

- `Cargo.toml`: make `clap` optional. Add `cli = ["dep:clap"]` and add `cli` to `default`. Give the
  `[[bin]]` entry `required-features = ["cli"]`.
- Two more targets need clap or the binary and must be gated the same way, or they break every
  `--no-default-features --all-targets` build:
  - `examples/chaos.rs` uses `clap::{Parser, Subcommand}`. Add an `[[example]] name = "chaos"`
    entry with `required-features = ["cli"]`.
  - `tests/e2e_pty_binary.rs` runs the `koh` binary through `CARGO_BIN_EXE_koh`, which only exists
    when the bin is built. Add a `[[test]] name = "e2e_pty_binary"` entry with
    `required-features = ["cli"]`.
- clap is used in the library only in `src/keycmd.rs`, `src/server/cli.rs`, `src/client/cli.rs`.
  Gate every clap-derived struct and every `use clap::…` behind `#[cfg(feature = "cli")]`. No
  clap `env` attributes are in use, so nothing environment-related moves.
- Any helper reachable only from a clap struct must be gated too, or `dead_code = "deny"` fails
  with the feature off. Check with the feature off, not just on.

## 3. Plain config types as the stable surface

For each entry point, introduce a config struct with **public fields**, no clap derive, `Debug` and
`Clone`, plus `Default` where every field has a sensible default. Move the real logic to take the
config. Keep the existing `*Args` structs gated on `cli`, implement `From<ServeArgs> for
ServeConfig` (and the others), and have the `cli`-gated old signatures delegate. Do not change the
`koh` binary's flags, help text or defaults. Field names below are the current `*Args` fields;
keep them, with the one substitution noted.

- `server::ServeConfig` for `serve(ServeConfig) -> anyhow::Result<()>`. Fields: `key_file:
  Option<PathBuf>`, `allow: Vec<String>`, `command: Vec<String>` (replaces `shell:
  Option<String>`), `scrollback: u64`, `session_ttl_secs: u64`, `relay_url: Option<String>`,
  `local: bool`, `max_connections: u32`, `max_sessions: u32`. `Default` uses the clap defaults
  for the numeric knobs and an empty `allow`; keep the non-empty `allow` check at the `serve`
  entry so a library caller gets the same error the CLI does.
- `command` semantics: empty means the login shell, exactly as today. `command[0]` is the program
  and the rest are arguments. Thread it through `server::session::spawn_session` into
  `pty::Pty::spawn`, which currently takes `shell: Option<&str>` and passes it whole to
  `CommandBuilder::new`. Change that parameter to a slice and add the tail with
  `CommandBuilder::args`. Keep `TERM`, `default_shell` and `scrub_koh_env` exactly as they are.
- `client::ConnectConfig` for `connect(ConnectConfig) -> anyhow::Result<Option<u32>>`. Fields:
  `server: String`, `key_file: Option<PathBuf>`, `direct: Option<SocketAddr>`, `relay_url:
  Option<String>`, `clipboard: bool`. No `Default`, since `server` is required.
- `client::IdConfig` for `run_id(IdConfig)`. Field: `key_file: Option<PathBuf>`.
- `keycmd::KeyConfig` for `keycmd::run(KeyConfig)`. Fields: `op: KeyOp` (`enum KeyOp { Passwd,
  Info }`), `key_file: Option<PathBuf>`. The passphrase prompt (`rpassword`) and the QR renderer
  (`qrcode`) stay in core, not behind `cli`.
- `From<ServeArgs>` maps `shell: Some(s)` to `command: vec![s]`. Optionally also let `--shell`
  repeat (`Vec<String>` with `num_args = 1`) so a command line can be hosted without a wrapper
  script; if you do, document it in `--help` and the changelog.

## 4. Documentation

- `src/lib.rs` "Public API stability": the supported surface is now the four config types, the
  four functions, and the `ssp` core. State that the `*Args` clap types are `cli`-only adapters and
  not part of the stable surface. Mention the `cli` feature and that library users should set
  `default-features = false` and pick exactly one `backend-*` feature. `cargo doc` runs in CI with
  broken intra-doc links denied, so any link to a `*Args` type must be gated or dropped.
- `README.md`: add a short "As a library" subsection showing `Cargo.toml` usage with
  `default-features = false, features = ["backend-termina"]` and a short `serve(ServeConfig {
  allow: …, command: vec!["zellij".into(), "attach".into(), "-c".into(), "main".into()],
  ..Default::default() })` example. Note the binary is unaffected.
- `CHANGELOG.md`: a `0.10.0` entry in the existing Keep a Changelog format. Changed: license,
  `cli` default feature, config types as the stable surface. Added: `command` argv, any `--shell`
  repetition. State that the wire protocol, `PROTOCOL_VERSION`/ALPN, the `koh-key-v1` format and
  the CLI flags are unchanged.
- Bump `version` to `0.10.0` in `Cargo.toml` and refresh `Cargo.lock`.

## 5. CI and tests

- The two existing jobs `clippy (crossterm backend)` and `clippy (qwertty backend)` run
  `--all-targets --no-default-features --features backend-…`. With the bin, example and test
  gated, they still pass without `cli`, and that is what proves the gating. Leave them as they
  are; they are now the no-clap tripwire for those backends.
- Add one job, `clippy (library, no cli)`: `cargo clippy --lib --locked --no-default-features
  --features backend-termina -- -D warnings`. This is the exact configuration fux builds.
- The MSRV job runs `cargo check --locked --all-targets` on 1.91 with defaults; it needs no
  change, but run it.
- Tests. `src/pty.rs` has a `mod tests` with `resolve_shell` and `scrub` tests but no live spawn
  test; `tests/e2e_pty_binary.rs` shows how a real pty is driven. Add:
  - In `src/pty.rs`: `Pty::spawn` with `["sh", "-c", "exit 7"]` reaps exit code 7, proving the
    argument tail reaches the child. Allocating a pty works on the Linux and macOS CI runners.
  - In `src/pty.rs`: an empty command resolves to `default_shell()`.
  - In `src/server/cli.rs` under `cfg(feature = "cli")`: `From<ServeArgs>` maps `--shell x` to
    `command == ["x"]`, and the numeric defaults match `ServeConfig::default()`.
- Run, and paste the results into the PR description:

  ```sh
  cargo fmt --all --check
  cargo clippy --all-targets --locked -- -D warnings
  cargo clippy --lib --locked --no-default-features --features backend-termina -- -D warnings
  cargo clippy --all-targets --locked --no-default-features --features backend-crossterm -- -D warnings
  cargo test --locked
  cargo doc --no-deps --locked
  cargo build --release --locked
  cargo package --locked --allow-dirty --list | grep -E 'COPYING|CHANGELOG'
  ```

## 6. PR description

Two paragraphs on the why: fux depends on koh as a library and needs a clap-free, argv-capable
server entry point; the relicense makes that dependency legal for an MIT crate. Then a checklist of
the six sections above with what was done. State explicitly: no wire, key-format or CLI behaviour
change; `cargo install koh` produces the same dependency tree as before, plus the `cli` feature
name; the three gated targets and why.

Do not publish to crates.io. Do not tag. Stop after the PR is open and report its URL.
