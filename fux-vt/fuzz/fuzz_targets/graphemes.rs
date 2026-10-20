//! Printed text against the model of how it is segmented into cells
//! (`tests/corpus/graphemes_target.rs` says what is checked).
#![no_main]
use libfuzzer_sys::fuzz_target;
#[path = "../../tests/corpus/graphemes.rs"]
mod graphemes;
#[path = "../../tests/corpus/graphemes_target.rs"]
mod graphemes_target;
#[path = "../../tests/corpus/models.rs"]
mod models;

fuzz_target!(|input: graphemes_target::Input| graphemes_target::run(&input));
