//! fux3: a terminal multiplexer where a pane is a directory of ordinary
//! files. One small process, the holder, keeps each pane: it holds the
//! program's terminal, publishes what it shows as files, and takes typed
//! bytes and commands from two FIFOs. Any Unix tool is a client.
//!
//! This library is the session directory ([`session`]), the files' formats
//! ([`files`]) and the holder ([`holder`], [`publish`], [`ctl`]). The viewer,
//! `fux3-view`, reads and writes the same files.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("fux3 runs on Linux and macOS only");

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod ctl;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod files;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod holder;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod publish;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod recipe;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod session;

/// What went wrong, said in one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure(pub String);

impl Failure {
    pub fn new(text: impl Into<String>) -> Failure {
        Failure(text.into().replace('\n', " "))
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `what: error` as a failure.
pub fn io(what: impl std::fmt::Display, error: impl std::fmt::Display) -> Failure {
    Failure::new(format!("{what}: {error}"))
}
