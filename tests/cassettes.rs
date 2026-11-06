//! Replays `tests/cassettes/` (`tests/cassettes/README.md`):
//! `cargo test --test cassettes -- [--update] [--keep] [--dir PATH] [NAME…]`.
// They script `/bin/sh` over a Unix socket (WINDOWS.md, "What PR 8 found").
#[cfg(unix)]
mod runner {
    include!("cassettes/runner.rs");
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    runner::main()
}

#[cfg(windows)]
fn main() {}
