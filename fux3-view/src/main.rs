//! fux3-view: shows a fux3 session's panes as fux does, through their files.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("fux3-view runs on Linux and macOS only");

fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
