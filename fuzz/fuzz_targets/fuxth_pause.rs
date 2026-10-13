#![no_main]
//! Input: a byte picking a budget of steps (1 to 256), then fuxth source,
//! as `fuxth_run` takes it.
//! - Run in one go, and run that many steps at a time (resumed after each
//!   pause, and after each `ask` suspends), the program ends the same: the
//!   same values left, or the same error at the same place.
#[path = "fuxth_host.rs"]
mod host;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&budget, source)) = data.split_first() else {
        return;
    };
    let text = String::from_utf8_lossy(source);
    let (Ok(whole), Ok(paused)) = (host::compile(&text), host::compile(&text)) else {
        return;
    };
    let whole = host::run(whole, host::STEPS);
    let paused = host::run(paused, u64::from(budget).saturating_add(1));
    // Past the steps allowed, the two stop at different places.
    if let (Some(whole), Some(paused)) = (whole, paused) {
        assert_eq!(
            whole,
            paused,
            "budget {}",
            u64::from(budget).saturating_add(1)
        );
    }
});
