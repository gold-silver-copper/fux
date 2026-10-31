//! Replays `tests/cassettes/` (`tests/cassettes/README.md`):
//! `cargo test --test cassettes -- [--update] [--keep] [--dir PATH] [NAME…]`.
mod runner {
    include!("cassettes/runner.rs");
}

fn main() -> std::process::ExitCode {
    runner::main()
}
