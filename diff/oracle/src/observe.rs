//! What is read of both sides after every step, and how two readings are
//! told apart:
//!
//! - the screen's state: size, cursor, pending wrap, every mode, margins,
//!   the pen, the open link, history length, storage, the mark, the
//!   resize report and the options;
//! - every retained row, history and screen: identity, version, soft wrap,
//!   prompt mark, length, spilled text, and each cell's text, halves,
//!   attributes (through both `attributes` and each getter) and link; and
//!   whether `Screen::cell`, `Screen::link`, `Screen::row_wrapped`,
//!   `Screen::starts_prompt` and `Row::cell` say the same;
//! - three marks (as made, the step before, a few steps before): changed,
//!   full refresh, and the dirty rows each way;
//! - rows looked up by identity, one of them perhaps gone;
//! - copies of the screen and the top of history (`Window::text`);
//! - probes: copies of each parser given sequences that show what no
//!   accessor does, read after each: every query's reply (DECRQM for every
//!   mode, DSR, DA1/2/3, XTVERSION, DECXCPR, DECRQSS, DECRQCRA, kitty's
//!   flags, the size reports, colour queries); the saved cursor (DECRC)
//!   and the character sets (G0 and G1 drawn); the tab stops; the cluster
//!   the next character would join and the character REP repeats; the
//!   kitty keyboard stack, popped, and the alternate screen's.
use crate::Side;
use crate::case::{Count, Difference, same, same_list};
use crate::model::{Glance, Heard, Row, Selection};
use std::fmt::Write;

/// The row buffers a comparison reads into, kept from step to step.
#[derive(Default)]
pub struct Readers {
    a: Row,
    b: Row,
}

/// What differs between two rows, first: a field, or the first cell.
fn row_difference(a: &Row, b: &Row) -> (String, String, String) {
    let fields: [(&str, String, String); 9] = [
        ("identity", a.id.to_string(), b.id.to_string()),
        ("version", a.version.to_string(), b.version.to_string()),
        ("soft wrap", a.wrapped.to_string(), b.wrapped.to_string()),
        ("prompt mark", a.prompt.to_string(), b.prompt.to_string()),
        (
            "length",
            format!("{} (empty {})", a.len, a.is_empty),
            format!("{} (empty {})", b.len, b.is_empty),
        ),
        (
            "spilled text",
            a.text_len.to_string(),
            b.text_len.to_string(),
        ),
        (
            "has links",
            a.has_links.to_string(),
            b.has_links.to_string(),
        ),
        (
            "accessors that disagree",
            format!("{:?}", a.disagreeing),
            format!("{:?}", b.disagreeing),
        ),
        ("links", format!("{:?}", a.links), format!("{:?}", b.links)),
    ];
    for (what, x, y) in fields {
        if x != y {
            return (what.into(), x, y);
        }
    }
    let columns = a.cells.len().max(b.cells.len());
    for col in 0..columns {
        let (x, y) = (a.describe_cell(col), b.describe_cell(col));
        if x != y {
            return (
                format!("cell {col}"),
                format!("{x}\n        row {}", a.line()),
                format!("{y}\n        row {}", b.line()),
            );
        }
    }
    ("cells".into(), a.line(), b.line())
}

/// Every retained row, bottom up.
fn rows<A: Side, B: Side>(
    step: Option<usize>,
    a: &A,
    b: &B,
    readers: &mut Readers,
    count: &mut Count,
) -> Result<(), Difference> {
    let retained = a.retained().max(b.retained());
    for offset in 0..=retained {
        let (ha, hb) = (a.row(offset, &mut readers.a), b.row(offset, &mut readers.b));
        if ha != hb || readers.a != readers.b {
            let (what, base, work) = if ha && hb {
                row_difference(&readers.a, &readers.b)
            } else {
                (
                    "whether it is retained".into(),
                    ha.to_string(),
                    hb.to_string(),
                )
            };
            return Err(Difference {
                step,
                what: format!("row {offset} from the bottom: {what}"),
                base,
                work,
            });
        }
        if ha {
            count.rows = count.rows.saturating_add(1);
            count.cells = count
                .cells
                .saturating_add(u64::try_from(readers.a.cells.len()).unwrap_or(u64::MAX));
        }
    }
    Ok(())
}

/// Everything, after a step.
pub fn compare<A: Side, B: Side>(
    step: Option<usize>,
    a: &mut A,
    b: &mut B,
    readers: &mut Readers,
    count: &mut Count,
) -> Result<(), Difference> {
    let (sa, sb) = (a.state(), b.state());
    same(step, "the screen's state", &sa, &sb)?;
    rows(step, a, b, readers, count)?;
    same_list(step, "the marks", &a.marks(), &b.marks())?;
    // Rows looked up by identity: the bottom, the top of the screen, the
    // cursor's row, the oldest and one between. Each lookup walks the
    // rows, so not every row is.
    let (rows, _) = sa.size;
    let retained = a.retained();
    let cursor_row = usize::from(rows.saturating_sub(sa.cursor.0.saturating_add(1)));
    let offsets = [
        0,
        usize::from(rows.saturating_sub(1)),
        cursor_row,
        retained.saturating_sub(1),
        retained / 2,
    ];
    same_list(
        step,
        "rows looked up by identity",
        &a.lookups(&offsets),
        &b.lookups(&offsets),
    )?;
    // The screen and the top of history, copied whole.
    let (rows, cols) = sa.size;
    let last = (rows.saturating_sub(1), cols.saturating_sub(1));
    for offset in [0, sa.history_len] {
        let selection = Selection {
            offset,
            rows,
            cols,
            from: (0, 0),
            to: last,
            max_cells: usize::MAX,
            max_bytes: usize::MAX,
        };
        same(
            step,
            &format!("a copy of the window {offset} rows up"),
            &a.copy(&selection, false),
            &b.copy(&selection, false),
        )?;
    }
    let (pa, pb) = (probes(a), probes(b));
    match pa.iter().zip(&pb).find(|(x, y)| x != y) {
        // Each reading starts with its probe's name.
        Some((x, y)) => {
            let name = x.split_once(": ").map_or(x.as_str(), |(name, _)| name);
            same(step, &format!("the probe {name:?}"), x, y)
        }
        None => same(step, "how many probes", &pa.len(), &pb.len()),
    }
}

/// Every query fux-vt answers, or might: DECRQM for each DEC private mode
/// and ANSI mode it knows and some it does not, DSR, DECXCPR, DA1, DA2,
/// DA3, XTVERSION, kitty's flag query, the size queries, DECRQSS for the
/// pen, cursor shape and margins and for what it does not know,
/// DECRQCRA for the whole screen and a corner, colour queries and the
/// colour-scheme query.
const QUERIES: &[&str] = &[
    "\x1b[?1$p",
    "\x1b[?2$p",
    "\x1b[?3$p",
    "\x1b[?4$p",
    "\x1b[?5$p",
    "\x1b[?6$p",
    "\x1b[?7$p",
    "\x1b[?8$p",
    "\x1b[?9$p",
    "\x1b[?12$p",
    "\x1b[?25$p",
    "\x1b[?45$p",
    "\x1b[?47$p",
    "\x1b[?66$p",
    "\x1b[?67$p",
    "\x1b[?69$p",
    "\x1b[?1000$p",
    "\x1b[?1001$p",
    "\x1b[?1002$p",
    "\x1b[?1003$p",
    "\x1b[?1004$p",
    "\x1b[?1005$p",
    "\x1b[?1006$p",
    "\x1b[?1007$p",
    "\x1b[?1015$p",
    "\x1b[?1016$p",
    "\x1b[?1036$p",
    "\x1b[?1047$p",
    "\x1b[?1048$p",
    "\x1b[?1049$p",
    "\x1b[?2004$p",
    "\x1b[?2026$p",
    "\x1b[?2027$p",
    "\x1b[?2031$p",
    "\x1b[?2048$p",
    "\x1b[?9999$p",
    "\x1b[?$p",
    "\x1b[2$p",
    "\x1b[4$p",
    "\x1b[12$p",
    "\x1b[20$p",
    "\x1b[$p",
    "\x1b[5n",
    "\x1b[6n",
    "\x1b[?6n",
    "\x1b[?15n",
    "\x1b[?996n",
    "\x1b[c",
    "\x1b[0c",
    "\x1b[1c",
    "\x1b[>c",
    "\x1b[>0c",
    "\x1b[=c",
    "\x1b[>q",
    "\x1b[>0q",
    "\x1b[?u",
    "\x1b[18t",
    "\x1b[14t",
    "\x1b[19t",
    "\x1b[21t",
    "\x1bP$qm\x1b\\",
    "\x1bP$q q\x1b\\",
    "\x1bP$qr\x1b\\",
    "\x1bP$q\"p\x1b\\",
    "\x1bP$qs\x1b\\",
    "\x1bP$qlonger\x1b\\",
    "\x1b[1;1;1;1;9999;9999*y",
    "\x1b[7;1;2;3;4;5*y",
    "\x1b]10;?;?\x07",
    "\x1b]11;?\x1b\\",
    "\x1b]12;?\x07",
    "\x1b]4;1;?\x07",
];

/// A clone's state after a probe's sequence: the cursor, pen and kitty
/// flags, and the cursor's row.
fn glance<S: Side>(s: &S) -> Glance {
    let state = s.state();
    let (rows, _) = state.size;
    let offset = usize::from(rows.saturating_sub(state.cursor.0.saturating_add(1)));
    let mut row = Row::default();
    let row = if s.row(offset, &mut row) {
        format!("{row:?}")
    } else {
        "(none)".into()
    };
    Glance {
        cursor: state.cursor,
        pending_wrap: state.pending_wrap,
        origin_mode: state.origin_mode,
        pen: state.pen,
        kitty_keyboard_flags: state.kitty_keyboard_flags,
        row,
    }
}

/// Gives a copy `bytes`, and says what it heard and how it looks after.
fn poke<S: Side>(s: &mut S, label: &str, bytes: &[u8]) -> String {
    let mut heard: Vec<Heard> = Vec::new();
    let result = s.process(bytes, &mut heard);
    format!("{label}: {result:?} {heard:?} {:?}", glance(s))
}

/// The probes' readings: each a line, from two copies of the parser.
pub fn probes<S: Side>(s: &S) -> Vec<String> {
    let mut out = Vec::new();
    // The cluster the next character would join, the character REP
    // repeats, then every query.
    let mut copy = s.clone();
    out.push(poke(&mut copy, "a combining mark", "\u{301}".as_bytes()));
    out.push(poke(&mut copy, "REP", b"\x1b[2b"));
    let mut heard: Vec<Heard> = Vec::new();
    for query in QUERIES {
        let result = copy.process(query.as_bytes(), &mut heard);
        if result.is_err() {
            out.push(format!("{query:?}: {result:?}"));
        }
    }
    out.push(format!("queries: {heard:?}"));
    // The saved cursor and the character sets; the tab stops; the kitty
    // stack; the alternate screen's.
    let mut copy = s.clone();
    out.push(poke(&mut copy, "DECRC", b"\x1b8"));
    out.push(poke(&mut copy, "G0 and G1", b"q\x0eq\x0f"));
    let mut stops = String::new();
    let mut heard: Vec<Heard> = Vec::new();
    let _ = copy.process(b"\r", &mut heard);
    let (_, cols) = copy.state().size;
    for _ in 0..cols.min(400) {
        let _ = copy.process(b"\t", &mut heard);
        let _ = write!(stops, "{} ", copy.state().cursor.1);
    }
    let _ = copy.process(b"\x1b[3Z", &mut heard);
    let _ = write!(stops, "back {}", copy.state().cursor.1);
    out.push(format!("tab stops: {stops} {heard:?}"));
    for _ in 0..3 {
        out.push(poke(&mut copy, "kitty pop", b"\x1b[<u"));
    }
    out.push(poke(&mut copy, "the other screen", b"\x1b[?1049h\x1b[?u"));
    out
}
