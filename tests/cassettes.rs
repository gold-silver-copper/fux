//! Replays `tests/cassettes/` (`tests/cassettes/README.md`):
//! `cargo test --test cassettes -- [--update] [--keep] [--dir PATH] [NAME…]`.
// They drive the fux client, Unix-only till WINDOWS.md's PR 8.
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
