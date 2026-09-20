//! Fallible helpers so tests report failures as errors instead of panicking.
use std::fmt::Display;

pub type Outcome = Result<(), Box<dyn std::error::Error>>;

/// `need()` replaces `unwrap()`: the error names the call site, as a panic would.
pub trait Need<T> {
    fn need(self) -> Result<T, String>;
}
impl<T> Need<T> for Option<T> {
    #[track_caller]
    fn need(self) -> Result<T, String> {
        let location = std::panic::Location::caller();
        self.ok_or_else(|| format!("missing value at {location}"))
    }
}
impl<T, E: Display> Need<T> for Result<T, E> {
    #[track_caller]
    fn need(self) -> Result<T, String> {
        let location = std::panic::Location::caller();
        self.map_err(|error| format!("{error} at {location}"))
    }
}
