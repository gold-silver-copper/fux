//! The screen as text, for the tests that read it so.
use fux_vt::Parser;

/// Each row of the screen, its blank tail trimmed.
pub fn lines(parser: &Parser) -> Vec<String> {
    let screen = parser.screen();
    let (rows, cols) = screen.size().into();
    (0..rows)
        .map(|y| {
            (0..cols)
                .filter_map(|x| screen.cell(y, x))
                .filter(|c| !c.is_wide_continuation())
                .map(|c| if c.has_contents() { c.contents() } else { " " })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}
