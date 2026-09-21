//! Fallible helpers so tests report failures as errors instead of panicking.
use std::fmt::Display;

pub type Outcome = Result<(), Box<dyn std::error::Error>>;

/// Test-only convenience for small frame decoders. Production copy callers
/// use explicit cell/byte limits and propagate errors instead.
pub trait ScreenText {
    fn contents(&self) -> String;
}
impl ScreenText for fux_vt::Screen {
    fn contents(&self) -> String {
        let (rows, cols) = self.size();
        let result =
            self.window(0, rows, cols)
                .text((0, 0), (rows - 1, cols - 1), 262_144, 1_048_576);
        assert!(
            result.is_ok(),
            "test frame exceeded its extraction bounds: {result:?}"
        );
        let mut text = result.unwrap_or_default();
        text.truncate(text.trim_end_matches('\n').len());
        text
    }
}

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
