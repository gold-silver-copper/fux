//! The `graphemes` target's frozen seeds (`seeds/graphemes`), the inputs it
//! found the engine and the model to disagree on (FA-SB-2): a full cell,
//! then a character of no width it cannot take, a control, then a modifier,
//! which begins a cluster of its own after the control (UAX #29, GB5), not
//! one more character of the full cell's. Each file is read as the target
//! reads it (`arbitrary_take_rest`, as libfuzzer-sys decodes) and checked as
//! the target checks, on every `cargo test` of this package.
use arbitrary::{Arbitrary, Unstructured};
#[path = "../../tests/corpus/graphemes.rs"]
mod graphemes;
#[path = "../../tests/corpus/graphemes_target.rs"]
mod graphemes_target;
#[path = "../../tests/corpus/models.rs"]
mod models;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn the_graphemes_seeds_replay() -> Result {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/graphemes");
    let mut replayed = 0usize;
    for entry in std::fs::read_dir(dir)? {
        let bytes = std::fs::read(entry?.path())?;
        let input = graphemes_target::Input::arbitrary_take_rest(Unstructured::new(&bytes))?;
        graphemes_target::run(&input);
        replayed = replayed.saturating_add(1);
    }
    assert!(replayed >= 4, "{replayed} seeds");
    Ok(())
}
