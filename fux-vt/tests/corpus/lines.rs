//! The screen as text, for the tests that read it so.
use fux_vt::Parser;

/// Each row of the screen, its blank tail trimmed.
pub fn lines(parser: &Parser) -> Vec<String> {
    let window = parser.screen().window();
    (0..window.rows())
        .map(|y| {
            (window.row(y).iter().flat_map(|row| row.cells()))
                .filter(|c| !c.is_wide_continuation())
                .map(|c| {
                    if c.contents().is_empty() {
                        " "
                    } else {
                        c.contents()
                    }
                })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}
