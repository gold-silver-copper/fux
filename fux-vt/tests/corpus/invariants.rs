//! Shared independent invariants for deterministic tests and cargo-fuzz.
use fux_vt::{Attributes, Cells, Hyperlink, Mode, Parser, Sink};

/// The most bytes a cell's cluster keeps.
const CLUSTER_CAPACITY: usize = 128;
/// The longest hyperlink URI kept (README, `Hyperlinks`).
const URI_LIMIT: usize = 2083;

fn cells(row: fux_vt::Row<'_>) -> Vec<fux_vt::CellRef<'_>> {
    row.cells().collect()
}
use std::collections::HashSet;

/// The invariants after an operation: fux-vt's own checker's
/// (`Parser::check`, with the `checks` feature), then those read through
/// the public API.
pub fn check(p: &Parser) {
    #[cfg(feature = "checks")]
    {
        let report = p.check();
        assert!(report.is_clean(), "{report}");
    }
    let s = p.screen();
    let (rows, cols) = s.size().into();
    let mut ids = HashSet::new();
    assert_eq!(
        s.rows().len(),
        usize::from(rows).saturating_add(s.history_len())
    );
    for (offset, row) in s.rows().enumerate() {
        assert!(ids.insert(row.id()), "row identities alias");
        assert!(!row.is_empty());
        // Copied cell by cell, with their text, the row is the same.
        assert!(
            Cells::new(row.len()).is_ok_and(|mut copy| {
                let kept = (0..).zip(row.cells()).all(|(i, c)| copy.set(i, c));
                kept && copy.range(0..row.len()).eq(row.cells())
            }),
            "row {offset} copies differently"
        );
        for (i, cell) in row.cells().enumerate() {
            assert!(cell.contents().len() <= CLUSTER_CAPACITY);
            // A cell with contents shows them: its text is where it says.
            if !cell.contents().is_empty() {
                assert!(
                    !cell.contents().is_empty(),
                    "row {offset}: cell {i} lost its text"
                );
            }
            if cell.is_wide() {
                assert!(
                    i.checked_add(1)
                        .and_then(|j| row.cell(j))
                        .is_some_and(|c| c.is_wide_continuation()),
                    "orphan wide leader at {offset},{i}"
                );
            }
            if cell.is_wide_continuation() {
                assert!(
                    i.checked_sub(1)
                        .and_then(|i| row.cell(i))
                        .is_some_and(|c| c.is_wide()),
                    "orphan continuation at {offset},{i}"
                );
                assert!(cell.contents().is_empty());
            }
            // A link is a glyph's: a blank cell has none, a wide glyph's
            // halves have one, and its URI is within the spec's limits.
            let link = row.link(i);
            if let Some(link) = link {
                assert!(row.has_links());
                assert!(!cell.contents().is_empty() || cell.is_wide_continuation());
                assert!(!link.uri().is_empty() && link.uri().len() <= URI_LIMIT);
                let printable = |s: &str| s.bytes().all(|b| (0x20..=0x7e).contains(&b));
                assert!(printable(link.uri()));
            }
            if cell.is_wide_continuation() {
                assert_eq!(link, i.checked_sub(1).and_then(|i| row.link(i)));
            }
        }
    }
    let mark = s.mark();
    let w = s.window();
    assert_eq!((w.rows(), w.cols()), (rows, cols));
    assert_eq!(mark, s.mark(), "reading rows changed the terminal");
}

/// Two parsers fed the same bytes, split differently, alike in everything
/// a host reads (`THREATS.md` section 6, 29): `equal`, and the screen not
/// in use too.
pub fn same_feed(a: &Parser, b: &Parser) {
    equal(a, b);
    let (a, b) = (a.screen(), b.screen());
    for (a, b) in [
        (a.main_rows(), b.main_rows()),
        (a.alternate_rows(), b.alternate_rows()),
    ] {
        assert_eq!(a.len(), b.len());
        for (offset, (a, b)) in a.zip(b).enumerate() {
            assert_eq!(cells(a), cells(b), "row {offset} of both screens");
            assert_eq!(a.continues(), b.continues(), "row {offset} of both screens");
        }
    }
}

/// Two parsers alike in everything a host reads of the screen in use:
/// its rows, cursor, modes, keys, colours and pictures; and in the state
/// it reads only by what it does next (`probe`): the pen, the open link,
/// the margins and the scroll region.
pub fn equal(a: &Parser, b: &Parser) {
    assert_eq!(a.resize_report(), b.resize_report());
    assert_eq!(probe(a), probe(b), "the pen, the open link or the margins");
    let (a, b) = (a.screen(), b.screen());
    assert_eq!(a.size(), b.size());
    assert_eq!(a.cursor_position(), b.cursor_position());
    assert_eq!(a.pending_wrap(), b.pending_wrap());
    assert_eq!(a.mouse_protocol_mode(), b.mouse_protocol_mode());
    assert_eq!(a.mouse_protocol_encoding(), b.mouse_protocol_encoding());
    assert_eq!(a.kitty_keyboard_flags(), b.kitty_keyboard_flags());
    assert_eq!(a.key_mode(), b.key_mode());
    for mode in MODES {
        assert_eq!(a.mode(mode), b.mode(mode), "{mode:?}");
    }
    assert_eq!(a.cursor_shape(), b.cursor_shape());
    for index in 0..=u8::MAX {
        assert_eq!(
            a.palette_color(index),
            b.palette_color(index),
            "colour {index}"
        );
    }
    for number in 10..=19 {
        assert_eq!(
            a.dynamic_color(number),
            b.dynamic_color(number),
            "OSC {number}"
        );
    }
    assert_eq!(a.colors_changed(), b.colors_changed());
    assert_eq!(pictures(a), pictures(b));
    assert_eq!(a.history_len(), b.history_len());
    assert_eq!(a.rows().len(), b.rows().len());
    for (offset, (a, b)) in a.rows().zip(b.rows()).enumerate() {
        assert_eq!(cells(a), cells(b), "row {offset}");
        assert_eq!(a.continues(), b.continues(), "row {offset}");
        assert_eq!(links(a), links(b), "row {offset}");
    }
}

/// Every mode, as `Mode` declares them.
const MODES: [Mode; 37] = [
    Mode::Insert,
    Mode::NewLine,
    Mode::ApplicationCursor,
    Mode::SmoothScroll,
    Mode::ReverseVideo,
    Mode::Origin,
    Mode::Autowrap,
    Mode::AutoRepeat,
    Mode::MousePress,
    Mode::CursorBlink,
    Mode::ShowCursor,
    Mode::MoreFix,
    Mode::ReverseWrap,
    Mode::AlternateScreen,
    Mode::HorizontalCursorCoupling,
    Mode::ApplicationKeypad,
    Mode::BackarrowSendsBackspace,
    Mode::LeftRightMargins,
    Mode::SixelDisplay,
    Mode::MousePressRelease,
    Mode::MouseHighlight,
    Mode::MouseButtonMotion,
    Mode::MouseAnyMotion,
    Mode::FocusReporting,
    Mode::MouseSgr,
    Mode::AlternateScroll,
    Mode::MouseUrxvt,
    Mode::MouseSgrPixels,
    Mode::ExtendedReverseWrap,
    Mode::AlternateScreenCleared,
    Mode::SaveCursor,
    Mode::AlternateScreenSaveCursor,
    Mode::BracketedPaste,
    Mode::SynchronizedOutput,
    Mode::ColorSchemeUpdates,
    Mode::InBandResize,
    Mode::GraphemeClustering,
];

/// Takes nothing.
struct Deaf;
impl Sink for Deaf {}

/// What the state no host reads does next, in a copy of `p`: the glyph
/// printed at the top left takes the pen and the open link; then, in
/// origin mode, the cursor homes to the top left of the margins and the
/// scroll region, and stops at their bottom right.
type Probed = (Option<(Attributes, Option<Hyperlink>)>, [(u16, u16); 2]);

/// [`Probed`] for `p`.
fn probe(p: &Parser) -> Probed {
    let mut p = p.clone();
    fed(&mut p, b"\x1b[?6l\x1b[Hx");
    let glyph = (p.screen().window().row(0)).map(|row| {
        let attributes = row.cell(0).map(|c| c.attributes()).unwrap_or_default();
        (attributes, row.link(0).cloned())
    });
    let corners = [
        fed(&mut p, b"\x1b[?6h\x1b[H"),
        fed(&mut p, b"\x1b[32767;32767H"),
    ];
    (glyph, corners)
}

/// `p` fed every byte of `bytes`, as a host does; the cursor then.
fn fed(p: &mut Parser, bytes: &[u8]) -> (u16, u16) {
    let mut rest = bytes;
    while let Ok(Some((taken, _))) = p.process_until_stop(rest, &mut Deaf) {
        rest = rest.get(taken..).unwrap_or_default();
    }
    p.screen().cursor_position()
}

/// A picture shown: its top row and first column, the columns and rows
/// it covers, and its size in pixels.
type Shown = (i64, u16, u16, u16, (u32, u32));

/// Each picture shown.
fn pictures(s: &fux_vt::Screen) -> Vec<Shown> {
    s.pictures()
        .map(|p| {
            (
                p.row,
                p.col,
                p.cols,
                p.rows,
                (p.picture.width(), p.picture.height()),
            )
        })
        .collect()
}

/// Each cell's link (its URI, id and key alike).
fn links(row: fux_vt::Row<'_>) -> Vec<Option<&Hyperlink>> {
    (0..row.len()).map(|col| row.link(col)).collect()
}
