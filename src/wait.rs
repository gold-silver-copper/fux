//! `wait`, and the wait an action's wait words make: a command line that
//! answers once, in each of its panes, one of its conditions is met, its
//! program exited first, the pane closed, or its time is up. Each condition
//! is something that happened after the wait began:
//!
//! - `Text`: the program wrote the text, exactly, in what it wrote since,
//!   escape sequences left out (`crate::written`), so the echo of keys
//!   typed counts.
//! - `Change`: the program wrote anything.
//! - `Quiet`: the program wrote nothing for so long, counted from the
//!   wait's start or the last it wrote.
//! - `Exit`: the program exited, or had.
//! - `Bell`: the program rang the bell or sent a desktop notification.
//! - `Done`: a shell command ended, as its OSC 133 `D` mark says.
//!
//! A wait begins once fux has read all its panes' programs have written
//! (`Session::catch_up`): what comes after is new. After every round the
//! server asks each wait again (`Waiting::answer`), which reads the text
//! its panes' programs wrote since (`Pane::written`), their counts of bells
//! and commands ended, and their journals (`crate::log`); and again when
//! its time comes (`Waiting::due`). Of several conditions met, the one
//! that happened first in what the program wrote answers.
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

/// One pane a wait watches, and where its program's output stood as the
/// wait began.
struct Watch {
    /// The program's start it watches (`Pane::starts`).
    starts: u64,
    /// The journal's last entry: what came after is new.
    from: Seq,
    /// How many bytes its program had written.
    bytes: u64,
    /// How many bells and notifications it had rung (`Alerts::rung`).
    rung: u64,
    /// How many shell commands had ended there.
    finished: u64,
    /// How much of its text the wait has read (`Written::text_len`).
    read: u64,
    /// When its program last wrote since it began, or when it began.
    last: Instant,
    /// Where in its bytes it first wrote something since, once it has.
    changed: Option<u64>,
    /// What it wrote since, if a text is waited for.
    text: Option<Text>,
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
                let texts = wait.until.iter().any(|u| matches!(u, Until::Text(_)));
                let watch = Watch {
                    starts: pane.starts,
                    from: pane.record.last(),
                    bytes: pane.written.bytes,
                    rung: pane.alerts.rung,
                    finished: pane.screen().commands().finished,
                    read: pane.written.text_len,
                    last: now,
                    changed: None,
                    text: texts.then(Text::default),
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
        let quiet = (self.wait.until.iter())
            .filter_map(|u| match u {
                Until::Quiet(quiet) => Some(*quiet),
                Until::Text(_) | Until::Change | Until::Exit | Until::Bell | Until::Done => None,
            })
            .min();
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
    /// wait has come to there at `now`, if anything: of the conditions
    /// met, the one met first in what it wrote (a text before a bell or a
    /// command's end that the same piece of output ends with), then its
    /// exit, then quiet.
    fn look(&mut self, pane: &Pane, wait: &Wait, now: Instant) -> Option<Told> {
        if pane.starts != self.starts {
            return Some(Told::Replaced);
        }
        let written = &pane.written;
        // The text taken since the server last let the waits read it, and
        // where each piece of it ended: what the wait has not read is new.
        let start = written
            .text_len
            .saturating_sub(u64::try_from(written.text.len()).unwrap_or(0));
        let seen = self.bytes;
        for &(end, at) in written.ends.iter().filter(|(_, at)| *at > seen) {
            let piece = |n: u64| usize::try_from(n.saturating_sub(start)).unwrap_or(usize::MAX);
            let new = written
                .text
                .get(piece(self.read)..piece(end))
                .unwrap_or_default();
            if let Some(text) = &mut self.text {
                text.push(new, at);
            }
            self.read = end;
        }
        if written.bytes > self.bytes {
            let first = written
                .ends
                .iter()
                .map(|(_, at)| *at)
                .find(|at| *at > self.bytes);
            self.changed = self.changed.or(first).or(Some(written.bytes));
            self.last = self.last.max(pane.wrote_at);
            self.bytes = written.bytes;
        }
        let texts: Vec<&String> = (wait.until.iter())
            .filter_map(|u| match u {
                Until::Text(text) => Some(text),
                Until::Change | Until::Quiet(_) | Until::Exit | Until::Bell | Until::Done => None,
            })
            .collect();
        let text = self.text.as_mut().and_then(|text| text.find(&texts));
        let journal = || pane.record.after(self.from);
        // That a bell rang, or a command ended, is counted, and so never
        // missed; what it said is the journal's, while it keeps it.
        let bell = (pane.alerts.rung > self.rung).then(|| {
            let entry = journal().find_map(|e| match &e.event {
                Event::Bell => Some((e.at, None)),
                Event::Notification(text) => Some((e.at, Some(text.clone()))),
                Event::Title(_) | Event::Done(_) | Event::Resize(_) => None,
            });
            entry.unwrap_or((written.bytes, None))
        });
        let commands = pane.screen().commands();
        let done = (commands.finished > self.finished).then(|| {
            let entry = journal().find_map(|e| match e.event {
                Event::Done(status) => Some((e.at, status)),
                Event::Bell | Event::Notification(_) | Event::Title(_) | Event::Resize(_) => None,
            });
            // With none kept, the one command that ended says its status.
            let one = commands.finished == self.finished.saturating_add(1);
            entry.unwrap_or((written.bytes, commands.last_status.filter(|_| one)))
        });
        let met = (wait.until.iter())
            .filter_map(|until| match until {
                Until::Text(_) => text
                    .clone()
                    .map(|(text, line, at)| (at, 0, Found::Text(text, line))),
                Until::Change => self.changed.map(|at| (at, 1, Found::Change)),
                Until::Bell => bell.clone().map(|(at, said)| (at, 2, Found::Bell(said))),
                Until::Done => done.map(|(at, status)| (at, 3, Found::Done(status))),
                Until::Exit => pane
                    .process
                    .exit_status()
                    .map(|ended| (u64::MAX, 4, Found::Exit(ended))),
                Until::Quiet(quiet) => {
                    (now >= after(self.last, *quiet)).then_some((u64::MAX, 5, Found::Quiet))
                }
            })
            .min_by_key(|(at, rank, _)| (*at, *rank));
        match (met, pane.process.exit_status()) {
            (Some((.., found)), _) => Some(Told::Found(found)),
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

/// The text a program wrote since a wait began (`crate::written`), as
/// `wait-text` searches it: at most `KEEP` bytes, the oldest let go, but
/// for what a text may go on from.
#[derive(Default)]
struct Text {
    kept: Vec<u8>,
    /// How far `kept` has been searched.
    searched: usize,
    /// Where each piece pushed ends in `kept`, and in the program's bytes.
    ends: Vec<(usize, u64)>,
}

/// The most `Text` keeps. A text waited for is at most a quarter of it
/// (`MAX_TEXT`), so what it keeps after searching holds any text begun.
const KEEP: usize = 64 << 10;

/// The longest text a wait looks for.
pub const MAX_TEXT: usize = KEEP / 4;

impl Text {
    /// `text`, a piece of what was written, ending at byte `at` of it.
    fn push(&mut self, text: &[u8], at: u64) {
        self.kept.extend_from_slice(text);
        self.ends.push((self.kept.len(), at));
    }

    /// The first of `texts` written since the last search, by where it
    /// starts: the text, the line it was written on, as written so far, and
    /// where the piece it ends in ended in the program's bytes.
    fn find(&mut self, texts: &[&String]) -> Option<(String, String, u64)> {
        let longest = texts.iter().map(|t| t.len()).max().unwrap_or(0);
        let from = self.searched.saturating_sub(longest.saturating_sub(1));
        let rest = self.kept.get(from..).unwrap_or_default();
        let at = |needle: &[u8]| {
            let last = (rest.len().checked_sub(needle.len())).filter(|_| !needle.is_empty())?;
            (0..=last).find(|at| rest.get(*at..).is_some_and(|r| r.starts_with(needle)))
        };
        let found = (texts.iter())
            .filter_map(|t| Some((at(t.as_bytes())?, *t)))
            .min_by_key(|(at, _)| *at);
        let Some((at, text)) = found else {
            self.searched = self.kept.len();
            if self.kept.len() > KEEP {
                let drop = self.kept.len().saturating_sub(KEEP / 2);
                self.kept = self.kept.get(drop..).unwrap_or_default().to_vec();
                self.ends.retain(|(end, _)| *end > drop);
                self.ends
                    .iter_mut()
                    .for_each(|(end, _)| *end = end.saturating_sub(drop));
                self.searched = self.kept.len();
            }
            return None;
        };
        let at = from.saturating_add(at);
        let end = at.saturating_add(text.len());
        self.searched = end;
        let byte = (self.ends.iter())
            .find(|(piece, _)| *piece >= end)
            .map(|(_, byte)| *byte)?;
        let (before, after) = self.kept.split_at_checked(end)?;
        let start = (before.get(..at)?.iter().rposition(|b| *b == b'\n'))
            .map_or(0, |n| n.saturating_add(1));
        let stop = (after.iter().position(|b| *b == b'\n'))
            .map_or(self.kept.len(), |n| end.saturating_add(n));
        let line = String::from_utf8_lossy(self.kept.get(start..stop)?).into_owned();
        Some((text.clone(), line, byte))
    }
}

#[cfg(test)]
mod tests {
    use super::Text;

    /// Of several `wait-quiet`s, the shortest says when to look again.
    #[test]
    fn the_shortest_quiet_is_looked_at_first() -> Result<(), String> {
        use crate::command::{Until, Wait};
        use std::time::{Duration, Instant};
        let session = crate::session::testing::started(crate::config::Config::default())?;
        let pane = session.panes.values().next().ok_or("a pane")?;
        let secs = Duration::from_secs;
        let wait = Wait {
            until: vec![Until::Quiet(secs(20)), Until::Quiet(secs(1))],
            timeout: Some(secs(30)),
        };
        let now = Instant::now();
        let waiting = super::Waiting::new(&[pane], wait, now, secs(30));
        assert!(waiting.due() <= crate::after(now, secs(1)));
        Ok(())
    }

    /// The line the first of `texts` written was on, searching `text`.
    fn found(text: &mut Text, texts: &[&str]) -> Option<String> {
        let texts: Vec<String> = texts.iter().map(|t| (*t).to_owned()).collect();
        let texts: Vec<&String> = texts.iter().collect();
        text.find(&texts).map(|(_, line, _)| line)
    }

    /// A text is found once, on its line as written so far, a piece at a
    /// time: a later search looks past it.
    #[test]
    fn a_text_is_found_on_its_line_once() {
        let mut text = Text::default();
        text.push(b"listen", 6);
        assert_eq!(found(&mut text, &["listening on"]), None);
        text.push(b"ing on :80\nnext", 20);
        assert_eq!(
            found(&mut text, &["listening on"]).as_deref(),
            Some("listening on :80")
        );
        assert_eq!(found(&mut text, &["listening on"]), None);
    }

    /// Of several texts, the first written is found, whichever came first
    /// among the texts asked for, with where the piece it ends in ended.
    #[test]
    fn of_several_texts_the_first_written_is_found() {
        let mut text = Text::default();
        text.push(b"one B\n", 7);
        let texts = ["A".to_owned(), "B".to_owned()];
        let found = text.find(&texts.iter().collect::<Vec<_>>());
        assert_eq!(found, Some(("B".to_owned(), "one B".to_owned(), 7)));
        text.push(b"then A and B\n", 30);
        let texts = ["B".to_owned(), "A".to_owned()];
        let found = text.find(&texts.iter().collect::<Vec<_>>());
        assert_eq!(found.map(|f| f.0).as_deref(), Some("A"));
    }

    /// A long stream is searched in parts: a text at its longest, written
    /// across where one part ends, is found all the same.
    #[test]
    fn a_text_across_what_is_let_go_is_found() {
        let long: String = std::iter::repeat_n('a', super::MAX_TEXT - 1)
            .chain(['b'])
            .collect();
        let mut text = Text::default();
        text.push(&vec![b'x'; super::KEEP], 1);
        assert_eq!(found(&mut text, &[&long]), None);
        text.push(&vec![b'a'; super::MAX_TEXT - 1], 2);
        assert_eq!(found(&mut text, &[&long]), None);
        assert!(text.kept.len() <= super::KEEP);
        text.push(b"b\n", 3);
        assert!(found(&mut text, &[&long]).is_some());
    }
}
