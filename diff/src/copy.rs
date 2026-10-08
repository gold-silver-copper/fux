//! Copy mode, in the baseline and the current fux: searches over a pane's
//! history find the same matches either way; selections copy the same text
//! or the same error; and every error copy mode can reach in a session --
//! no pane to copy from, the history dropping the rows it held, a selection
//! too large, a client or pane that is gone -- says the same.
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
    ($name:ident, $fux:ident, $vt:ident, $ids:ident, |$row:ident| $id:expr) => {
        mod $name {
            use $fux::config::Config;
            use $fux::copy::{self, Seek, Select};
            use $fux::session::{Ctx, Session};
            use $fux::$ids::ClientId;
            use $vt::Parser;

            pub fn terminal(
                rows: u16,
                cols: u16,
                history: usize,
                output: &str,
            ) -> Result<Parser, String> {
                let mut parser = Parser::new(rows, cols, history).map_err(|e| format!("{e:?}"))?;
                parser
                    .process(output.as_bytes())
                    .map_err(|e| format!("{e:?}"))?;
                Ok(parser)
            }

            pub fn retained(p: &Parser) -> usize {
                copy::retained(p.screen())
            }

            /// A search, and the selection from where the search started to
            /// where it ended, of each kind.
            pub fn search(p: &Parser, query: &str, from: (usize, u16), forward: bool) -> String {
                let seek = if forward {
                    Seek::Forward
                } else {
                    Seek::Backward
                };
                let found = copy::find(p.screen(), query, from, seek);
                let end = found.unwrap_or(from);
                let (start, end) = (from.min(end), from.max(end));
                let texts: Vec<String> = [Select::Char, Select::Line, Select::Block]
                    .into_iter()
                    .map(|kind| {
                        format!(
                            "{:?}",
                            copy::text(p.screen(), kind, start, end).map_err(|e| e.to_string())
                        )
                    })
                    .collect();
                let row = copy::row_at(p.screen(), from.0).map(|$row| $id);
                format!(
                    "{found:?} {texts:?} {row:?} {:?}",
                    row.and_then(|id| copy::index_of(p.screen(), id))
                )
            }

            fn session(
                history: usize,
                rows: u16,
                cols: u16,
            ) -> Result<(Session, ClientId), String> {
                let mut config = Config::default();
                config.history_lines = history;
                let mut s = Session::new(config, "/nonexistent/fux.sock".into(), false);
                s.start().map_err(|e| e.to_string())?;
                let c = s.attach(rows, cols, None).map_err(|e| e.to_string())?;
                Ok((s, c))
            }

            fn notice(s: &Session, c: ClientId) -> String {
                format!("{:?}", s.views.get(&c).and_then(|v| v.notice.clone()))
            }

            fn run(s: &mut Session, c: ClientId, line: &str) -> String {
                let argv: Vec<String> = line.split(' ').map(str::to_owned).collect();
                format!("{:?}", s.run(&argv, &Ctx::client(c)))
            }

            /// Every copy-mode error a session can reach, `lines` lines of
            /// output into it; what each step gave back.
            pub fn errors(lines: usize) -> Result<Vec<String>, String> {
                let mut out = Vec::new();
                // No pane to copy from: a tab emptied by moving its pane out.
                let (mut s, c) = session(100, 24, 80)?;
                out.push(run(&mut s, c, "move-pane --to new-tab"));
                out.push(run(&mut s, c, "select-tab --previous"));
                out.push(run(&mut s, c, "copy-mode"));
                s.input(c, b"\x02c");
                out.push(notice(&s, c));
                // The history drops the rows copy mode holds.
                let (mut s, c) = session(3, 6, 20)?;
                out.push(run(&mut s, c, "copy-mode"));
                s.input(c, b"kkkk");
                let text: String = std::iter::repeat_n("line\r\n", lines).collect();
                if let Ok(p) = $fux::command::parse_pane("%1") {
                    s.output(p, text.as_bytes());
                }
                s.settle();
                out.push(notice(&s, c));
                // A selection past the most cells one copy takes.
                let (mut s, c) = session(3000, 30, 300)?;
                let row: String = std::iter::repeat_n('x', 299)
                    .chain("\r\n".chars())
                    .collect();
                let text: String = std::iter::repeat_n(row.as_str(), lines).collect();
                if let Ok(p) = $fux::command::parse_pane("%1") {
                    s.output(p, text.as_bytes());
                }
                out.push(run(&mut s, c, "copy-mode"));
                s.input(c, b"tszy");
                out.push(notice(&s, c));
                out.push(format!("{} buffers", s.buffers.len()));
                // No such client, and no such pane, straight to copy mode.
                let (mut s, c) = session(10, 24, 80)?;
                s.detach(c);
                out.push(format!(
                    "{:?}",
                    copy::enter(&mut s, c).map_err(|e| e.to_string())
                ));
                let (mut s, c) = session(10, 24, 80)?;
                s.panes.clear();
                out.push(format!(
                    "{:?}",
                    copy::enter(&mut s, c).map_err(|e| e.to_string())
                ));
                Ok(out)
            }
        }
    };
}

// Rows give their identity through an accessor, in the baseline as now.
stack!(base, baseline, baseline_vt, command, |row| row.id());
stack!(cur, fux, fux_vt, id, |row| row.id());

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
            let from = (
                r.below(retained.saturating_add(2)),
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
    let mut scenarios = 0u64;
    let mut seen = String::new();
    for lines in [0usize, 1, 5, 900, 1000] {
        let errors = base::errors(lines)?;
        seen.push_str(&errors.join("\n"));
        same(
            &format!("copy-mode errors, with {lines} lines of output"),
            errors,
            cur::errors(lines)?,
        )?;
        bump(&mut scenarios);
    }
    // Each is still reached, or the comparison says less than it seems to.
    for error in [
        "no pane to copy from",
        "the history dropped the rows it held",
        "the selection is larger than",
        "no such client",
        "no such pane",
    ] {
        if !seen.contains(error) {
            return Err(format!(
                "no scenario reaches the copy-mode error {error:?} any more"
            ));
        }
    }
    Ok(format!(
        "{searches} searches ({found} found) with their selections, {scenarios} runs of every copy-mode error"
    ))
}
