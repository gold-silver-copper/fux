# Prompt: koh 0.10.0 — MIT relicense and a clap-free library surface for fux

Paste the section below into a Claude Code session opened in the koh repo.

---

Open a PR against `main` titled **"0.10.0: relicense to MIT, add `cli` feature and clap-free config
types"**. Work on a branch named `fux-surface`. This release exists so that a downstream crate,
`fux` (MIT), can depend on koh as a library, construct a serve/connect session without going
through clap, and point the server's pty at an arbitrary argv instead of a shell path. Read
`src/lib.rs`, `src/main.rs`, `src/server/cli.rs`, `src/client/cli.rs`, `src/keycmd.rs`,
`src/pty.rs`, `Cargo.toml`, `CHANGELOG.md` and `.github/workflows/ci.yml` before changing anything.

## 1. Relicense to MIT

`git shortlog -sne` shows a single author, so no consent round is needed.

- `Cargo.toml`: `license = "MIT"`.
- Replace `COPYING` with the MIT license text, copyright the current year and the name in
  `authors`. Keep the file name `COPYING` so the `exclude` list and any references stay valid.
- `README.md` "License" section: replace `GPL-3.0-or-later.` with MIT and one sentence saying
  releases before 0.10.0 remain available under GPL-3.0-or-later.
- `grep -rni "gpl" .` and fix every remaining mention, including `deny.toml` license allow-lists if
  they name koh's own license, and `SECURITY.md` or docs if they cite it.

## 2. Add a `cli` feature that owns clap

Goal: `cargo build --no-default-features --features backend-termina` compiles the library with no
clap in the dependency tree. `cargo install koh` is unchanged.

- `Cargo.toml`: make `clap` optional. Add `cli = ["dep:clap"]` and add `cli` to `default`. Give the
  `[[bin]]` entry `required-features = ["cli"]`.
- clap is used only in `src/main.rs`, `src/keycmd.rs`, `src/server/cli.rs`, `src/client/cli.rs`.
  Gate every clap-derived struct and every `use clap::…` behind `#[cfg(feature = "cli")]`.
- `dead_code = "deny"` is on. Any helper reachable only from a clap struct must be gated too. Check
  with the feature off, not just on.

## 3. Plain config types as the stable surface

For each entry point, introduce a config struct with **public fields**, no clap derive, `Debug`,
`Clone`, and a `Default` where every field has a sensible default. Move the real logic to take the
config. Keep the existing `*Args` structs gated on `cli`, implement `From<ServeArgs> for
ServeConfig` (and the others), and have the `cli`-gated old signatures delegate. Do not break the
`koh` binary's CLI flags, help text or defaults.

- `server::ServeConfig` for `serve(ServeConfig)`. Fields mirror `ServeArgs`, with one change:
  replace `shell: Option<String>` with `command: Vec<String>`. Empty means the login shell, as
  today. `command[0]` is the program, the rest are arguments. Thread this through
  `server::session::spawn_session` and `pty::Pty::spawn`, which currently takes `shell:
  Option<&str>` and passes it whole to `CommandBuilder::new`. Add the arguments with
  `CommandBuilder::args`. Keep `TERM` handling and `scrub_koh_env` exactly as they are.
- `client::ConnectConfig` for `connect(ConnectConfig) -> Result<Option<u32>>`.
- `client::IdConfig` for `run_id(IdConfig)`.
- `keycmd::KeyConfig` with a `KeyOp` enum (`Passwd`, `Info`) for `keycmd::run(KeyConfig)`. The
  passphrase prompt and the QR renderer stay in core, not behind `cli`.
- The old `ServeArgs.shell` flag must keep working: `From<ServeArgs>` maps `Some(s)` to
  `vec![s]`. Optionally also accept `--shell` more than once so a command line can be passed
  without a wrapper script; if you do, document it in `--help` and the changelog.

## 4. Documentation

- `src/lib.rs` "Public API stability": the supported surface is now the four config types, the
  four functions, and the `ssp` core. State that the `*Args` clap types are `cli`-only adapters and
  not part of the stable surface. Mention the `cli` feature and that library users should set
  `default-features = false` and pick a backend feature.
- `README.md`: add a short "As a library" subsection showing `Cargo.toml` usage with
  `default-features = false, features = ["backend-termina"]` and a five-line `serve(ServeConfig
  { allow: …, command: vec!["zellij".into(), "attach".into(), "-c".into(), "main".into()], ..
  Default::default() })` example.
- `CHANGELOG.md`: a `0.10.0` entry under the existing format. Sections: Changed (license, `cli`
  feature default, config types), Added (`command` argv, any `--shell` repetition), and a note that
  the wire protocol, key format and CLI flags are unchanged.
- Bump `version` to `0.10.0` in `Cargo.toml` and refresh `Cargo.lock`.

## 5. CI and tests

- `ci.yml` already runs clippy for `backend-crossterm` and `backend-qwertty` with
  `--no-default-features`. Add a job that builds and clippies the library with
  `--no-default-features --features backend-termina` so a clap leak fails CI, and confirm the
  existing default-feature jobs still build the binary.
- Add a unit test in `pty.rs` that `Pty::spawn` with a multi-element command runs the program
  with its arguments, for example `["sh", "-c", "exit 7"]` reaping exit code 7. Follow the style of
  the existing spawn tests.
- Add a unit test that `From<ServeArgs>` maps `--shell` to a one-element `command`.
- Run, and paste the results into the PR description:

  ```sh
  cargo fmt --check
  cargo clippy --all-targets --locked -- -D warnings
  cargo clippy --lib --locked --no-default-features --features backend-termina -- -D warnings
  cargo test --locked
  cargo build --release --locked
  cargo package --locked --allow-dirty   # confirms the .crate still builds and COPYING ships
  ```

## 6. PR description

Explain the why in two paragraphs: fux depends on koh as a library and needs a clap-free, argv-
capable server entry point; the relicense makes that dependency legal for an MIT crate. Then a
checklist of the six sections above with what was done. Note explicitly: no wire, key-format or CLI
behaviour change, and that `cargo install koh` produces the same binary tree as before plus the
`cli` feature name.

Do not publish to crates.io. Do not tag. Stop after the PR is open and report its URL.
