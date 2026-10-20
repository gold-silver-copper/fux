//! Finding a row by its identity (`Screen::row_index`, `row_by_id`) after
//! random scrolls of every kind: line feeds into history, regions scrolled
//! up and down (RI, IL, DL, SU, SD, DECSTBM), erasing and resizing, which
//! make rows above older ones (`threats.md` B40). Every row kept is found
//! where it is, and a row let go is not found. With the `checks` feature,
//! the checker holds after each step too, invariant 20 among its checks.

use fux_vt::{Parser, RowId, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A small deterministic generator (splitmix64), so a failure names its case.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }
    /// From 1 to `n`.
    fn upto(&mut self, n: u64) -> u64 {
        self.next().checked_rem(n).unwrap_or(0).saturating_add(1)
    }
}

/// One random step: mostly lines and line feeds, with every kind of
/// scroll, margins, erasing and now and then a resize.
fn step(r: &mut Rng, p: &mut Parser) -> Result {
    let rows = u64::from(p.screen().size().rows());
    let bytes = match r.upto(20) {
        1..=6 => format!("line {}\r\n", r.next() % 1000),
        7 => format!("\x1b[{};{}r", r.upto(rows), r.upto(rows)),
        8 => "\x1b[r".to_owned(),
        9 => format!("\x1b[{}H\x1bM", r.upto(rows)),
        10 => format!("\x1b[{}H\x1b[{}L", r.upto(rows), r.upto(3)),
        11 => format!("\x1b[{}H\x1b[{}M", r.upto(rows), r.upto(3)),
        12 => format!("\x1b[{}S", r.upto(3)),
        13 => format!("\x1b[{}T", r.upto(3)),
        14 => format!("\x1b[{}H\n\n", r.upto(rows)),
        15 => "\x1b[2J".to_owned(),
        16 => "\x1b[?1049h\x1bM\x1b[L\x1b[?1049l".to_owned(),
        17 => {
            let size = Size::new(u16::try_from(r.upto(10).saturating_add(2))?, 12)?;
            p.resize(size)?;
            return Ok(());
        }
        _ => format!("\x1b[{}Hx\x1b[{}D", r.upto(rows), r.upto(5)),
    };
    p.process(bytes.as_bytes())?;
    Ok(())
}

#[test]
fn every_row_kept_is_found_where_it_is() -> Result {
    let mut r = Rng(0x0b40_0000_0000_0001);
    let mut found = 0usize;
    for case in 0..200 {
        let mut p = Parser::new(Size::new(6, 12)?, 4 << 10)?;
        let mut seen: Vec<RowId> = Vec::new();
        for at in 0..200 {
            step(&mut r, &mut p)?;
            let screen = p.screen();
            let ids: Vec<RowId> = screen.rows().map(|row| row.id()).collect();
            for (index, &id) in ids.iter().enumerate() {
                assert_eq!(screen.row_index(id), Some(index), "case {case}, step {at}");
                assert_eq!(screen.row_by_id(id).map(|row| row.id()), Some(id));
                found = found.saturating_add(1);
            }
            for id in seen.iter().filter(|id| !ids.contains(id)) {
                assert_eq!(screen.row_index(*id), None, "case {case}, step {at}");
            }
            seen = ids;
            #[cfg(feature = "checks")]
            {
                let report = p.check();
                assert!(report.is_clean(), "case {case}, step {at}: {report}");
            }
        }
    }
    assert!(found > 100_000, "{found} rows found");
    Ok(())
}
