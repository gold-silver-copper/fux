//! `wait`, and the wait an action's wait words make: a command line that
//! answers once, in each of its panes, one of its conditions is met, its
//! program exited first, the pane closed, or its time is up. Each condition
//! is something that happened after the wait began:
//!
//! - `Text`: the program wrote the text, exactly, in what it wrote since,
//!   escape sequences left out (`Text`), so the echo of keys typed counts.
//! - `Change`: the program wrote anything.
//! - `Quiet`: the program wrote nothing for so long, counted from the
//!   wait's start or the last it wrote.
//! - `Exit`: the program exited, or had.
//! - `Bell`: the program rang the bell or sent a desktop notification.
//! - `Done`: a shell command ended, as its OSC 133 `D` mark says.
//!
//! After every round the server asks each wait again (`Waiting::answer`),
//! which reads what each pane's program wrote since (`Pane::fresh`) and its
//! journal since (`crate::log`); and again when its time comes
//! (`Waiting::due`).
use crate::after;
use crate::code::Code;
use crate::command::{AnyRef, Until, Wait};
use crate::id::PaneId;
use crate::json::Json;
use crate::log::{Event, Seq};
use crate::pane::Pane;
use crate::process::Ended;
use crate::protocol::{End, Reply};
use crate::session::Session;
use std::time::{Duration, Instant};

/// A wait under way.
pub struct Waiting {
    /// Each pane it watches, in order, and how far it came there.
    watches: Vec<(PaneId, Look)>,
    wait: Wait,
    deadline: Instant,
    /// How long it waits at most: its own timeout, else the server's.
    pub(crate) limit: Duration,
}

/// A pane of a wait: still looked at, or its answer, which no look
/// changes.
enum Look {
    Looking(Box<Watch>),
    Told(Reply),
}

/// One pane a wait watches.
struct Watch {
    /// The program's start it watches (`Pane::starts`).
    starts: u64,
    /// The journal's last entry as it began: what came after is new.
    from: Seq,
    /// How many bytes its program had written as it last read (`Pane::wrote`).
    read: u64,
    /// When its program last wrote since it began, or when it began.
    last: Instant,
    /// Its program wrote something since it began.
    changed: bool,
    /// What its program wrote since it began, as `Text` reads it.
    text: Text,
}

/// What a wait found in a pane.
enum Found {
    /// A text, and the line it was written on, as written so far.
    Text(String, String),
    Change,
    Quiet,
    Exit(Ended),
    /// A bell, or a notification's text.
    Bell(Option<String>),
    /// A shell command's end, and its status if the shell said.
    Done(Option<i32>),
}

/// How a wait in a pane still there ended, if it did.
enum Told {
    Found(Found),
    /// Its program exited, which it did not wait for.
    Exited(Ended),
    /// Its program was started again (`respawn`).
    Replaced,
    TimedOut,
}

impl Waiting {
    /// A wait in `panes` from `now`, for `limit` at most unless it says.
    pub fn new(panes: &[&Pane], wait: Wait, now: Instant, limit: Duration) -> Waiting {
        let limit = wait.timeout.unwrap_or(limit);
        let watches = (panes.iter())
            .map(|pane| {
                let watch = Watch {
                    starts: pane.starts,
                    from: pane.record.last(),
                    read: pane.wrote,
                    last: now,
                    changed: false,
                    text: Text::default(),
                };
                (pane.id, Look::Looking(Box::new(watch)))
            })
            .collect();
        Waiting {
            watches,
            deadline: after(now, limit),
            limit,
            wait,
        }
    }

    /// When to ask it again at the latest, whatever happens before: when a
    /// pane would have been quiet long enough, else when its time is up.
    pub fn due(&self) -> Instant {
        let quiet = (self.wait.until.iter()).find_map(|u| match u {
            Until::Quiet(quiet) => Some(*quiet),
            Until::Text(_) | Until::Change | Until::Exit | Until::Bell | Until::Done => None,
        });
        (self.watches.iter())
            .filter_map(|(_, look)| match look {
                Look::Looking(watch) => quiet.map(|quiet| after(watch.last, quiet)),
                Look::Told(_) => None,
            })
            .fold(self.deadline, Instant::min)
    }

    /// Its answer, if it has one at `now`: each pane answers on its own, as
    /// a lone pane's wait would, and the wait once every one has: a line
    /// from each, in order, with the first failing status.
    pub fn answer(&mut self, session: &Session, now: Instant) -> Option<Reply> {
        let late = now >= self.deadline;
        for (id, look) in &mut self.watches {
            let Look::Looking(watch) = look else {
                continue;
            };
            let Some(pane) = session.panes.get(id) else {
                *look = Look::Told(closed(*id));
                continue;
            };
            let told = match watch.look(pane, &self.wait, now) {
                Some(told) => told,
                None if late => Told::TimedOut,
                None => continue,
            };
            *look = Look::Told(self.wait.told(pane, told, self.limit));
        }
        let answers: Vec<&Reply> = (self.watches.iter())
            .map(|(_, look)| match look {
                Look::Looking(_) => None,
                Look::Told(reply) => Some(reply),
            })
            .collect::<Option<_>>()?;
        if let [one] = answers.as_slice() {
            return Some((*one).clone());
        }
        // The first failure, in order, names the whole, with every pane's
        // why.
        let why: String = (answers.iter())
            .filter_map(|a| match &a.end {
                End::Failed { message, .. } => Some(format!("{}\n", message.trim_end())),
                End::Done => None,
            })
            .collect();
        let end = match answers.iter().find_map(|a| a.end.code()) {
            None => End::Done,
            Some(code) => End::Failed { code, message: why },
        };
        Some(Reply {
            end,
            text: answers.iter().map(|a| a.text.as_str()).collect(),
            json: answers.iter().map(|a| a.json.as_str()).collect(),
            ids: answers.iter().flat_map(|a| a.ids.iter().copied()).collect(),
            ..Reply::default()
        })
    }
}

impl Watch {
    /// Reads what `pane`'s program wrote since the last look, and what the
    /// wait has come to there at `now`, if anything.
    fn look(&mut self, pane: &Pane, wait: &Wait, now: Instant) -> Option<Told> {
        if pane.starts != self.starts {
            return Some(Told::Replaced);
        }
        // `fresh` holds the last bytes of what it wrote: those it has not
        // read are new.
        let len = u64::try_from(pane.fresh.len()).unwrap_or(u64::MAX);
        let skip = self.read.saturating_sub(pane.wrote.saturating_sub(len));
        let new = (usize::try_from(skip).ok())
            .and_then(|skip| pane.fresh.get(skip..))
            .unwrap_or_default();
        self.read = pane.wrote;
        if !new.is_empty() {
            self.changed = true;
            self.last = pane.wrote_at;
            self.text.read(new);
        }
        let journal = || pane.record.after(self.from);
        let found = wait.until.iter().find_map(|until| match until {
            Until::Text(text) => (self.text.find(text))
                .map(|line| Found::Text(text.clone(), String::from_utf8_lossy(line).into_owned())),
            Until::Change => self.changed.then_some(Found::Change),
            Until::Quiet(quiet) => (now >= after(self.last, *quiet)).then_some(Found::Quiet),
            Until::Exit => pane.process.exit_status().map(Found::Exit),
            Until::Bell => journal().find_map(|e| match &e.event {
                Event::Bell => Some(Found::Bell(None)),
                Event::Notification(text) => Some(Found::Bell(Some(text.clone()))),
                Event::Title(_) | Event::Done(_) | Event::Resize(_) => None,
            }),
            Until::Done => journal().find_map(|e| match e.event {
                Event::Done(status) => Some(Found::Done(status)),
                Event::Bell | Event::Notification(_) | Event::Title(_) | Event::Resize(_) => None,
            }),
        });
        match (found, pane.process.exit_status()) {
            (Some(found), _) => Some(Told::Found(found)),
            (None, Some(ended)) => Some(Told::Exited(ended)),
            (None, None) => None,
        }
    }
}

impl Wait {
    /// The answer of a wait that ended so in `pane`.
    fn told(&self, pane: &Pane, told: Told, limit: Duration) -> Reply {
        let id = pane.id;
        let failed = |code, why: &str, more: &str| End::Failed {
            code,
            message: format!("{id}: {why} while waiting for {}{more}", self.describe()),
        };
        let exited = |ended| match ended {
            Ended::Exited(code) => vec![("exited", Json::int(code))],
            Ended::Signalled(signal) => vec![("signal", Json::int(signal))],
        };
        let met = |what: &'static str, fields: Vec<(&'static str, Json<'static>)>| {
            std::iter::once(("met", Json::str(what)))
                .chain(fields)
                .collect()
        };
        let (said, fields, body, end) = match told {
            Told::Found(Found::Text(text, line)) => {
                let fields = vec![
                    ("text", Json::str(text.clone())),
                    ("line", Json::str(line.clone())),
                ];
                let said = format!("text {text:?}");
                (said, met("text", fields), format!("{line}\n"), End::Done)
            }
            Told::Found(Found::Change) => (
                "changed".into(),
                met("change", vec![]),
                String::new(),
                End::Done,
            ),
            Told::Found(Found::Quiet) => (
                "quiet".into(),
                met("quiet", vec![]),
                String::new(),
                End::Done,
            ),
            Told::Found(Found::Exit(ended)) => {
                let said = crate::process::told(ended);
                (said, met("exit", exited(ended)), String::new(), End::Done)
            }
            Told::Found(Found::Bell(notification)) => {
                let said = (notification.as_ref())
                    .map_or_else(|| "bell".to_owned(), |n| format!("notification {n:?}"));
                let field = notification.map_or(Json::Null, Json::str);
                let fields = met("bell", vec![("notification", field)]);
                (said, fields, String::new(), End::Done)
            }
            Told::Found(Found::Done(status)) => {
                let said = status.map_or_else(|| "done".to_owned(), |s| format!("done {s}"));
                let fields = met("done", vec![("done", status.map_or(Json::Null, Json::int))]);
                (said, fields, String::new(), End::Done)
            }
            Told::Exited(ended) => {
                let said = crate::process::told(ended);
                let end = failed(Code::Exited, &format!("its program {said}"), "");
                (said, exited(ended), String::new(), end)
            }
            Told::Replaced => {
                let end = failed(Code::Exited, "its program was started again (respawn)", "");
                ("respawned".into(), Vec::new(), String::new(), end)
            }
            Told::TimedOut => {
                let why = format!("timed out after {}s", limit.as_secs_f64());
                // `wait-done` where nothing ever marked a command.
                let unmarked = self.until.contains(&Until::Done) && !pane.marked();
                let more = unmarked.then(|| format!("; {}", crate::pane::UNMARKED));
                let end = failed(Code::Timeout, &why, &more.unwrap_or_default());
                ("timeout".into(), Vec::new(), String::new(), end)
            }
        };
        said_so(id, said, fields, body, end)
    }

    /// Its conditions, as an error says what did not come.
    fn describe(&self) -> String {
        (self.until.iter())
            .map(|until| match until {
                Until::Text(text) => format!("{text:?}"),
                Until::Change => "output".to_owned(),
                Until::Quiet(quiet) => format!("{}s with no output", quiet.as_secs_f64()),
                Until::Exit => "the program to exit".to_owned(),
                Until::Bell => "a bell".to_owned(),
                Until::Done => "a shell command to end".to_owned(),
            })
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

/// The answer of a wait whose pane `id` closed.
fn closed(id: PaneId) -> Reply {
    let end = End::Failed {
        code: Code::Closed,
        message: format!("{id}: the pane closed"),
    };
    said_so(id, "closed".into(), Vec::new(), String::new(), end)
}

/// A pane's answer to a wait, said two ways: as text, a line naming the
/// pane and what happened (`%3 done 0`), then `body`; as JSON, the pane,
/// `fields`, the status and the code of one that is not 0.
fn said_so(
    id: PaneId,
    said: String,
    fields: Vec<(&'static str, Json<'static>)>,
    body: String,
    end: End,
) -> Reply {
    let pane = ("pane", Json::str(id.to_string()));
    let mut fields: Vec<_> = std::iter::once(pane).chain(fields).collect();
    fields.push(("status", Json::int(end.status())));
    fields.extend(end.code().map(|code| ("code", Json::str(code.name()))));
    Reply {
        end,
        text: format!("{id} {said}\n{body}"),
        json: format!("{}\n", Json::Object(fields).render()),
        ids: vec![AnyRef::Pane(id)],
        ..Reply::default()
    }
}

/// What a program wrote, as `wait-text` reads it: its bytes with escape
/// sequences (CSI, OSC, DCS and the other strings, and the rest) and the
/// other control characters left out; a carriage return or a line feed
/// ends a line; tabs kept. Read as it comes, a piece at a time.
#[derive(Default)]
struct Text {
    /// Where the bytes read last left off.
    state: Strip,
    /// What it kept since the wait began: at most `KEEP` bytes, the oldest
    /// let go.
    kept: Vec<u8>,
    /// How far `kept` has been searched.
    searched: usize,
}

/// Where `Text` is in what it reads.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Strip {
    #[default]
    Text,
    /// After ESC, and any intermediate bytes.
    Escape,
    /// In a CSI, until its final byte.
    Csi,
    /// In an OSC, DCS, SOS, PM or APC, until BEL or ST.
    String,
    /// After an ESC in one of those: ST if `\` follows.
    StringEscape,
}

/// The most `Text` keeps: a line longer than this is searched in parts.
const KEEP: usize = 64 << 10;

impl Text {
    /// Reads `bytes`, written next.
    fn read(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state = match (self.state, b) {
                // CAN and SUB cancel a sequence anywhere.
                (_, 0x18 | 0x1a) => Strip::Text,
                (Strip::StringEscape, b'\\') => Strip::Text,
                (Strip::String, 0x07) => Strip::Text,
                (Strip::String | Strip::StringEscape, 0x1b) => Strip::StringEscape,
                (Strip::String | Strip::StringEscape, _) => Strip::String,
                (_, 0x1b) => Strip::Escape,
                (Strip::Escape, b'[') => Strip::Csi,
                (Strip::Escape, b']' | b'P' | b'X' | b'^' | b'_') => Strip::String,
                (Strip::Escape, 0x20..=0x2f) => Strip::Escape,
                (Strip::Escape, _) => Strip::Text,
                (Strip::Csi, 0x40..=0x7e) => Strip::Text,
                (Strip::Csi, _) => Strip::Csi,
                (Strip::Text, b'\r' | b'\n') => {
                    self.kept.push(b'\n');
                    Strip::Text
                }
                (Strip::Text, b'\t') => {
                    self.kept.push(b);
                    Strip::Text
                }
                (Strip::Text, 0..=0x1f | 0x7f) => Strip::Text,
                (Strip::Text, _) => {
                    self.kept.push(b);
                    Strip::Text
                }
            };
        }
    }

    /// The line `text` was written on, as written so far, if it was
    /// written since the last search. Past `KEEP`, what was searched is let
    /// go, but for what a text may go on from.
    fn find(&mut self, text: &str) -> Option<&[u8]> {
        let needle = text.as_bytes();
        let from = self.searched.saturating_sub(needle.len().saturating_sub(1));
        let rest = self.kept.get(from..).unwrap_or_default();
        let last = rest
            .len()
            .checked_sub(needle.len())
            .filter(|_| !needle.is_empty());
        let at = last.and_then(|last| {
            (0..=last).find(|at| rest.get(*at..).is_some_and(|r| r.starts_with(needle)))
        });
        let Some(at) = at.map(|at| from.saturating_add(at)) else {
            self.searched = self.kept.len();
            if self.kept.len() > KEEP {
                let drop = self.kept.len().saturating_sub(KEEP / 2);
                self.kept = self.kept.get(drop..).unwrap_or_default().to_vec();
                self.searched = self.kept.len();
            }
            return None;
        };
        let end = at.saturating_add(needle.len());
        self.searched = end;
        let (before, after) = self.kept.split_at_checked(end)?;
        let start = (before.get(..at)?.iter().rposition(|b| *b == b'\n'))
            .map_or(0, |n| n.saturating_add(1));
        let stop = (after.iter().position(|b| *b == b'\n'))
            .map_or(self.kept.len(), |n| end.saturating_add(n));
        self.kept.get(start..stop)
    }
}

#[cfg(test)]
mod tests {
    use super::Text;

    /// What `wait-text` reads of output: the text written, escape
    /// sequences and controls left out, across pieces however split; and
    /// the line a text was found on.
    #[test]
    fn text_is_read_without_its_escape_sequences() {
        let output = b"\x1b[1mlis\x1b]0;title\x07ten\x1bP+q\x1b\\ing\x1b[0m on :80\x07\r\nnext";
        for split in 0..output.len() {
            let mut text = Text::default();
            let (a, b) = output.split_at_checked(split).unwrap_or((output, &[]));
            text.read(a);
            let early = text.find("listening on :80").is_some();
            text.read(b);
            let found = text.find("listening on :80").map(<[u8]>::to_vec);
            assert!(
                early || found.as_deref() == Some(&b"listening on :80"[..]),
                "split {split}"
            );
        }
        let mut text = Text::default();
        text.read(b"a\x1b(Bb\tc\x18d");
        assert_eq!(text.find("ab\tcd"), Some(&b"ab\tcd"[..]));
        // A text is found once: a later search looks past it.
        assert_eq!(text.find("ab"), None);
    }

    /// A long stream is searched in parts, a text written across where one
    /// part ends found all the same.
    #[test]
    fn a_text_across_what_is_let_go_is_found() {
        let mut text = Text::default();
        text.read(&vec![b'x'; super::KEEP + 10]);
        text.read(b"READ");
        assert_eq!(text.find("READY"), None);
        text.read(b"Y now\n");
        assert!(
            text.find("READY")
                .is_some_and(|line| line.ends_with(b"READY now"))
        );
    }
}
