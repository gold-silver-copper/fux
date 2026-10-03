//! In-band resize (mode 2048): a program that sets it is told its size in
//! band, at once and after every resize, and only once its terminal has the
//! new size (`references/modern/mode_2048_in_band_resize.md`).
mod support;
use support::*;

/// Each report the program printed, as (rows, cols) reported and (rows,
/// cols) its terminal had when it read the report.
fn reports(text: &str) -> Vec<((u16, u16), (u16, u16))> {
    text.lines()
        .filter_map(|line| {
            let (report, size) = line.trim_end().split_once('|')?;
            let mut fields = report.split(';');
            let rows = fields.next()?.parse().ok()?;
            let cols = fields.next()?.parse().ok()?;
            let (srows, scols) = size.split_once(' ')?;
            Some(((rows, cols), (srows.parse().ok()?, scols.parse().ok()?)))
        })
        .collect()
}

#[test]
fn a_program_is_told_its_size_in_band_once_it_has_it() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 60)?;
    client.wait_for("$")?;
    // Reads each report up to its final `t`, and prints what it said beside
    // the terminal's size as the program sees it then.
    server.type_line(
        "%1",
        "bash -c 'stty -icanon -echo; printf \"\\033[?2048h\"; \
         while IFS= read -r -d t r; do printf \"%s|%s\\n\" \"${r#*48;}\" \"$(stty size)\"; done'",
    )?;
    client.wait("the first report", |t| reports(t).len() == 1)?;
    client.resize(14, 70)?;
    client.wait("the report after the resize", |t| reports(t).len() == 2)?;
    let seen = reports(&client.text());
    for (reported, size) in &seen {
        assert_eq!(reported, size, "reported before the terminal had the size");
    }
    let (first, second) = (seen.first().ok_or("one")?.0, seen.get(1).ok_or("two")?.0);
    assert_eq!(
        (
            second.0.saturating_sub(first.0),
            second.1.saturating_sub(first.1)
        ),
        (4, 10),
        "{seen:?}"
    );
    Ok(())
}
