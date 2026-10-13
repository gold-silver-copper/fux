#![no_main]
//! Input: fuxth source, any bytes (read as UTF-8, invalid bytes replaced).
//! - Splitting never panics.
//! - Every token's line, and an error's, is a line of the source, and
//!   tokens come in the order of their lines.
//! - Each token, quoted as a saved session quotes it, reads back as itself.
use fuxth::{Place, token};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let lines = u32::try_from(text.split('\n').count()).unwrap_or(u32::MAX);
    let line = |place: &Place| match place {
        Place::Line { line, .. } => *line,
        Place::Argument(_) => 0,
    };
    match token::source("fuzz", &text) {
        Err(error) => {
            let at = error.place.as_ref().map_or(0, line);
            assert!(at >= 1 && at <= lines, "{error} of {lines} lines");
        }
        Ok(tokens) => {
            let mut last = 1;
            for t in &tokens {
                let at = line(&t.place);
                assert!(
                    at >= last && at <= lines,
                    "{t:?} after line {last}, of {lines}"
                );
                last = at;
            }
            let quoted: Vec<String> = (tokens.iter())
                .map(|t| format!("'{}'", t.text.replace('\'', "'\\''")))
                .collect();
            let again = token::source("again", &quoted.join(" "));
            let again = again.unwrap_or_else(|e| panic!("{quoted:?}: {e}"));
            let texts = |ts: &[fuxth::Token]| ts.iter().map(|t| t.text.clone()).collect::<Vec<_>>();
            assert_eq!(texts(&again), texts(&tokens));
        }
    }
});
