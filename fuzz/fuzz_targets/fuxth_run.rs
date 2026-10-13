#![no_main]
//! Input: fuxth source (UTF-8, invalid bytes replaced), compiled after the
//! standard library with two host words, `ask` and `maybe` (see `fuxth_host.rs`).
//! - Compiling and running never panic, and a run ends within its limits.
//! - Every error is at a line of the source or the standard library.
//! - A program the checker lets through never finds its host giving more
//!   or fewer values than the vocabulary says (`contract`), and never fails
//!   for too few values where its depth was known to be enough: an error
//!   from the run is one the checker could not rule out.
#[path = "fuxth_host.rs"]
mod host;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let program = match host::compile(&text) {
        Ok(program) => program,
        Err(error) => {
            assert!(host::placed(&error, &text), "{error}");
            return;
        }
    };
    if let Some(Err(error)) = host::run(program, host::STEPS) {
        assert!(host::placed(&error, &text), "{error}");
        assert_ne!(error.code, fuxth::Code::Contract, "{error}");
    }
});
