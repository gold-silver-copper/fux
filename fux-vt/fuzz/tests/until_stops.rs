//! The fuzz targets' feeding as fux's pane feeds (`common::until_stops`)
//! finishes the work a piece's last byte leaves: REP's copies under IRM,
//! which stop at the budget between glyphs, are all made before it
//! returns, as one call of `process_with` makes them.
#[path = "../fuzz_targets/common.rs"]
#[expect(dead_code, reason = "the targets' other helpers")]
mod common;

use fux_vt::{Parser, Size};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn a_piece_s_last_rep_is_finished() -> Result {
    let size = Size::new(3, 511)?;
    let piece = b"\x1b[4hx\x1b[65535b";
    let mut whole = Parser::new(size, 8 << 20)?;
    whole.process(piece)?;
    let mut fed = Parser::new(size, 8 << 20)?;
    let mut calls = 0usize;
    common::until_stops(&mut fed, piece, &mut common::Record::default(), |_| {
        calls = calls.saturating_add(1);
    });
    assert!(calls > 1, "{calls} calls");
    assert!(!fed.has_work_left());
    let (a, b) = (whole.screen(), fed.screen());
    assert_eq!(a.cursor_position(), b.cursor_position());
    assert!(a.rows().zip(b.rows()).all(|(a, b)| a.cells().eq(b.cells())));
    Ok(())
}
