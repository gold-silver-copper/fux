//! Copy mode, in the baseline and the current fux: searches over a pane's
//! history find the same matches either way; selections copy the same text
//! or the same error. Its errors in a session are no longer compared: the
//! baseline reaches them by keys its server reads and command lines it
//! parses, and the current fux by its clients' actions and fuxth.
use crate::rng::Rng;
use crate::{Outcome, bump, same, times};

const LINES: &[&str] = &[
    "hello world",
    "Hello World again",
    "界界 wide 界",
    "e\u{301}cole café",
    "",
    "   indented",
    "a-b_c.d/e",
    "\x1b[1mbold\x1b[0m text",
    "the quick brown fox jumps over the lazy dog",
    "FIND find Find",
];

const QUERIES: &[&str] = &[
    "o", "hello", "Hello", "world", "界", "wide", "é", "e\u{301}", "find", "Find", "FIND", "x", "",
    " ", "dog", "a-b",
];

macro_rules! stack {
    (
        $name:ident, $fux:ident, $vt:ident, $seek_in:ident,
        |$rows:ident, $cols:ident| [$($size:tt)*],
        |$s:ident| $retained:expr,
        |$at:ident, $index:ident| $row:expr,
        |$found:ident, $from:ident, $query:ident, $seek:ident| $find:expr,
        |$copied:ident, $kind:ident, $start:ident, $end:ident| $text:expr
    ) => {
        mod $name {
            use $fux::$seek_in::Seek;
            use $fux::copy::{self, Select};
            use $vt::Parser;

            pub fn terminal(
                rows: u16,
                cols: u16,
                history: usize,
                output: &str,
            ) -> Result<Parser, String> {
                let ($rows, $cols) = (rows, cols);
                let mut parser = Parser::new($($size)*, history).map_err(|e| format!("{e:?}"))?;
                parser
                    .process(output.as_bytes())
                    .map_err(|e| format!("{e:?}"))?;
                Ok(parser)
            }

            pub fn retained(p: &Parser) -> usize {
                let $s = p.screen();
                $retained
            }

            /// The identity and place of the row at `index`, counted from
            /// the oldest.
            fn row(p: &Parser, index: usize) -> Option<($vt::RowId, Option<usize>)> {
                let ($at, $index) = (p.screen(), index);
                $row
            }

            /// A search, and the selection from where the search started to
            /// where it ended, of each kind.
            pub fn search(p: &Parser, query: &str, from: (usize, u16), forward: bool) -> String {
                let seek = if forward {
                    Seek::Forward
                } else {
                    Seek::Backward
                };
                let found: Option<(usize, u16)> = {
                    let ($found, $from, $query, $seek) = (p.screen(), from, query, seek);
                    $find
                };
                let end = found.unwrap_or(from);
                let (start, end) = (from.min(end), from.max(end));
                let texts: Vec<String> = [Select::Char, Select::Line, Select::Block]
                    .into_iter()
                    .map(|kind| {
                        let text: Result<String, String> = {
                            let ($copied, $kind, $start, $end) = (p.screen(), kind, start, end);
                            $text
                        };
                        format!("{text:?}")
                    })
                    .collect();
                let (id, index) = row(p, from.0).unzip();
                format!("{found:?} {texts:?} {id:?} {:?}", index.flatten())
            }
        }
    };
}

// The baseline counts rows from the oldest in copy mode's own functions;
// now they are fux-vt's rows, which know their place. Its `Seek` was copy
// mode's own; now it is an action's, sent by the client.
stack! {
    base, baseline, baseline_vt, copy,
    |r, c| [r, c],
    |s| copy::retained(s),
    |s, index| copy::row_at(s, index).map(|row| (row.id(), copy::index_of(s, row.id()))),
    |s, from, query, seek| copy::find(s, query, from, seek),
    |s, kind, start, end| copy::text(s, kind, start, end).map_err(|e| e.to_string())
}
stack! {
    cur, fux, fux_vt, act,
    |r, c| [crate::size(r, c)],
    |s| s.rows().len(),
    |s, index| s.rows().nth(index).map(|row| (row.id(), Some(row.index()))),
    |s, from, query, seek| s
        .rows()
        .nth(from.0)
        .and_then(|row| copy::find(query, (row, from.1), seek))
        .map(|(row, col)| (row.index(), col)),
    |s, kind, start, end| {
        let at = |(index, col): (usize, u16)| Some((s.rows().nth(index)?, col));
        match at(start).zip(at(end)) {
            Some((start, end)) => copy::text(s, kind, start, end).map_err(|e| e.to_string()),
            None => Err("no such row".to_owned()),
        }
    }
}

pub fn run(r: &mut Rng, scale: usize) -> Outcome {
    let (mut searches, mut found) = (0u64, 0u64);
    for case in 0..times(1_000, scale) {
        let rows = u16::try_from(r.below(10).saturating_add(1)).unwrap_or(1);
        let cols = u16::try_from(r.below(40).saturating_add(1)).unwrap_or(1);
        let history = r.below(30);
        let output: String = (0..r.below(40))
            .map(|_| format!("{}\r\n", r.pick(LINES).copied().unwrap_or_default()))
            .collect();
        let a = base::terminal(rows, cols, history, &output)?;
        let b = cur::terminal(rows, cols, history, &output)?;
        let retained = base::retained(&a);
        same(
            &format!("terminal {case}: rows retained"),
            retained,
            cur::retained(&b),
        )?;
        for _ in 0..20 {
            let query = r.pick(QUERIES).copied().unwrap_or("o");
            // A row retained: copy mode's positions are no other.
            let from = (
                r.below(retained),
                u16::try_from(r.below(usize::from(cols).saturating_add(2))).unwrap_or(0),
            );
            let forward = r.chance(50);
            let (sa, sb) = (
                base::search(&a, query, from, forward),
                cur::search(&b, query, from, forward),
            );
            if !sa.starts_with("None") {
                bump(&mut found);
            }
            same(
                &format!(
                    "terminal {case} ({rows}x{cols}, history {history}, output {output:?}): {query:?} from {from:?} forward {forward}"
                ),
                sa,
                sb,
            )?;
            bump(&mut searches);
        }
    }
    Ok(format!(
        "{searches} searches ({found} found) with their selections"
    ))
}
