//! `fux wait`, and send-keys' waits: a command line from the CLI that answers
//! once one of its panes does what one of its conditions asks, its program
//! exits first, its pane goes, or its time is up. The server holds its
//! connection meanwhile and asks it again whenever a pane may have changed,
//! or its time comes (`Waiting::due`).
//!
//! Text is looked for in every line written since the wait began, history
//! included, not only on the screen: a match that a flood scrolls away at
//! once is still found. History rows do not change, so each is read once.
use crate::command::{CaptureView, Until, Wait};
use crate::id::PaneId;
use crate::json::Json;
use crate::pane::Pane;
use crate::process::Ended;
use crate::session::{Outcome, Session};
use std::time::Instant;

/// The exit statuses a wait ends with, besides 0 (what it waited for came)
/// and the 1 and 2 of any command.
pub const EXITED: u8 = 3;
pub const GONE: u8 = 4;
/// Keys sent changed nothing on the screen, and the wait was for it to
/// settle (send-keys --wait-idle): they may not have reached the program.
pub const UNCHANGED: u8 = 5;
/// As `timeout(1)`.
pub const TIMED_OUT: u8 = 124;

/// A wait under way.
pub struct Waiting {
    watches: Vec<Watch>,
    wait: Wait,
    /// When it began: a pane is quiet from then at the soonest.
    began: Instant,
    deadline: Instant,
    /// Printed before what the wait says: the pane `run` started.
    pub lead: String,
}

/// One pane a wait watches, and what it found as it began.
struct Watch {
    pane: PaneId,
    /// The screen's top row as the wait began: lines from it on count.
    anchor: Option<fux_vt::RowId>,
    /// The last history row read, and how many times each text was found
    /// in the history rows read: they do not change.
    read: Option<fux_vt::RowId>,
    in_history: Vec<(usize, Option<String>)>,
    /// How many times each text was there as the wait began, for a wait
    /// that counts only what comes after.
    before: Vec<usize>,
    revision: u64,
    finished: u64,
    alerts: u64,
}

/// What a wait found.
enum Found {
    Text { text: String, line: String },
    Gone(String),
    Idle { changed: bool },
    Change(u64),
    Exit(Ended),
    Done(Option<i32>),
    Bell(Option<String>),
}

impl Waiting {
    pub fn new(panes: &[&Pane], wait: Wait, now: Instant) -> Waiting {
        let texts = texts(&wait);
        let watches = panes
            .iter()
            .map(|pane| {
                let screen = pane.screen();
                let anchor = screen.window().row(0).map(|row| row.id());
                let lines = joined(
                    screen
                        .window()
                        .row(0)
                        .into_iter()
                        .flat_map(|top| std::iter::successors(Some(top), fux_vt::Row::below)),
                );
                let before = texts.iter().map(|t| count(&lines, t)).collect();
                Watch {
                    pane: pane.id,
                    anchor,
                    read: None,
                    in_history: vec![(0, None); texts.len()],
                    before,
                    revision: pane.revision,
                    finished: screen.commands().finished,
                    alerts: pane.alerts.count(),
                }
            })
            .collect();
        Waiting {
            watches,
            deadline: crate::after(now, wait.timeout),
            wait,
            began: now,
            lead: String::new(),
        }
    }

    /// When to ask it again at the latest, whatever happens before: when a
    /// pane will have been quiet long enough, or its time is up. A pane
    /// quiet already, its input still going in, is asked again as that
    /// input goes, which wakes the server itself.
    pub fn due(&self, session: &Session) -> Instant {
        let now = Instant::now();
        let idle = self.wait.until.iter().find_map(|u| match u {
            Until::Idle(idle) => Some(*idle),
            Until::Text(_)
            | Until::Gone(_)
            | Until::Change(_)
            | Until::Exit
            | Until::Done
            | Until::Bell => None,
        });
        let quiet = idle.and_then(|idle| {
            self.watches
                .iter()
                .filter_map(|w| session.panes.get(&w.pane))
                .map(|pane| crate::after(pane.changed_at.max(self.began), idle))
                .filter(|at| *at > now)
                .min()
        });
        quiet.map_or(self.deadline, |at| at.min(self.deadline))
    }

    /// Its answer, if it has one at `now`: what it waited for came in one of
    /// its panes, a pane's program exited first, a pane went, or its time is
    /// up.
    pub fn answer(&mut self, session: &Session, now: Instant) -> Option<Outcome> {
        let outcome = self.look(session, now)?;
        Some(Outcome {
            stdout: format!("{}{}", self.lead, outcome.stdout),
            ..outcome
        })
    }

    fn look(&mut self, session: &Session, now: Instant) -> Option<Outcome> {
        let texts = texts(&self.wait);
        let several = self.watches.len() > 1;
        for watch in &mut self.watches {
            let Some(pane) = session.panes.get(&watch.pane) else {
                let id = watch.pane;
                return Some(self.wait.told(id, None, GONE, Told::Gone, several));
            };
            if let Some(found) = watch.look(pane, &self.wait, &texts, self.began, now) {
                let status = match &found {
                    Found::Idle { changed: false } if self.wait.after => UNCHANGED,
                    Found::Text { .. }
                    | Found::Gone(_)
                    | Found::Idle { .. }
                    | Found::Change(_)
                    | Found::Exit(_)
                    | Found::Done(_)
                    | Found::Bell(_) => 0,
                };
                let id = watch.pane;
                return Some(
                    self.wait
                        .told(id, Some(pane), status, Told::Found(found), several),
                );
            }
            // A program that exited does nothing more: the wait ends, saying so.
            if let Some(ended) = pane.process.exit_status() {
                let id = watch.pane;
                return Some(
                    self.wait
                        .told(id, Some(pane), EXITED, Told::Exited(ended), several),
                );
            }
        }
        if now >= self.deadline {
            let first = self.watches.first().map(|w| w.pane);
            let pane = first.and_then(|id| session.panes.get(&id));
            return first.map(|id| self.wait.told(id, pane, TIMED_OUT, Told::TimedOut, several));
        }
        None
    }
}

/// How a wait ended, to be told.
enum Told {
    Found(Found),
    Exited(Ended),
    Gone,
    TimedOut,
}

impl Wait {
    /// The outcome of a wait that ended so in pane `id`.
    fn told(
        &self,
        id: PaneId,
        pane: Option<&Pane>,
        status: u8,
        told: Told,
        several: bool,
    ) -> Outcome {
        let prefix = if several {
            format!("{id} ")
        } else {
            String::new()
        };
        let screen = pane.filter(|_| self.print).map(text);
        let (said, problem, json) = match told {
            Told::Found(found) => {
                let (said, json) = found.said();
                let problem = matches!(found, Found::Idle { changed: false })
                    .then(|| format!("{id}: the screen did not change after the keys"));
                (said, problem, json)
            }
            Told::Exited(ended) => {
                let what = self
                    .until
                    .iter()
                    .map(Until::describe)
                    .collect::<Vec<_>>()
                    .join(" or ");
                let message = format!("{id}: {} before {what}", crate::process::told(ended));
                (String::new(), Some(message), ended_json("exited", ended))
            }
            Told::Gone => (
                String::new(),
                Some(format!("{id}: the pane closed")),
                vec![("met", Json::str("gone"))],
            ),
            Told::TimedOut => {
                let seconds = self.timeout.as_secs_f64();
                let message = format!("{id}: timed out after {seconds}s");
                (
                    String::new(),
                    Some(message),
                    vec![("met", Json::str("timeout"))],
                )
            }
        };
        let stdout = if self.json {
            let mut fields = vec![("pane", Json::str(id.to_string()))];
            fields.extend(json);
            fields.push(("status", Json::Number(i64::from(status))));
            if let Some(screen) = &screen {
                fields.push(("screen", Json::str(screen.clone())));
            }
            format!("{}\n", Json::Object(fields).render())
        } else {
            let said = if said.is_empty() {
                said
            } else {
                format!("{prefix}{said}\n")
            };
            format!("{said}{}", screen.unwrap_or_default())
        };
        Outcome {
            status,
            stdout,
            stderr: problem.filter(|_| !self.json).unwrap_or_default(),
        }
    }
}

impl Until {
    /// The condition as an error says what did not come.
    fn describe(&self) -> String {
        match self {
            Until::Text(text) => format!("showing {text:?}"),
            Until::Gone(text) => format!("{text:?} went"),
            Until::Idle(idle) => format!("settling for {}ms", idle.as_millis()),
            Until::Change(_) => "a change".into(),
            Until::Exit => "exiting".into(),
            Until::Done => "a command ended".into(),
            Until::Bell => "a bell".into(),
        }
    }
}

impl Found {
    /// What the wait prints, and its fields as JSON.
    fn said(&self) -> (String, Vec<(&'static str, Json<'static>)>) {
        match self {
            Found::Text { text, line } => (
                line.clone(),
                vec![
                    ("met", Json::str("text")),
                    ("text", Json::str(text.clone())),
                    ("line", Json::str(line.clone())),
                ],
            ),
            Found::Gone(text) => (
                String::new(),
                vec![
                    ("met", Json::str("gone")),
                    ("text", Json::str(text.clone())),
                ],
            ),
            Found::Idle { changed } => (
                String::new(),
                vec![
                    ("met", Json::str("idle")),
                    ("changed", Json::Bool(*changed)),
                ],
            ),
            Found::Change(revision) => (
                String::new(),
                vec![
                    ("met", Json::str("change")),
                    (
                        "revision",
                        Json::Number(i64::try_from(*revision).unwrap_or(i64::MAX)),
                    ),
                ],
            ),
            Found::Exit(ended) => (crate::process::told(*ended), ended_json("exit", *ended)),
            Found::Done(status) => (
                status.map_or_else(|| "done".to_owned(), |s| format!("done {s}")),
                vec![
                    ("met", Json::str("done")),
                    (
                        "exit",
                        status.map_or(Json::Null, |s| Json::Number(i64::from(s))),
                    ),
                ],
            ),
            Found::Bell(notification) => (
                notification
                    .as_ref()
                    .map_or_else(|| "bell".to_owned(), |n| format!("notification: {n}")),
                vec![
                    ("met", Json::str("bell")),
                    (
                        "notification",
                        notification.clone().map_or(Json::Null, Json::str),
                    ),
                ],
            ),
        }
    }
}

/// An end as JSON: what it met, and its status or signal.
fn ended_json(met: &'static str, ended: Ended) -> Vec<(&'static str, Json<'static>)> {
    match ended {
        Ended::Exited(code) => vec![
            ("met", Json::str(met)),
            ("exit", Json::Number(i64::from(code))),
        ],
        Ended::Signalled(signal) => vec![
            ("met", Json::str(met)),
            ("signal", Json::Number(i64::from(signal))),
        ],
    }
}

impl Watch {
    /// What the pane shows of what the wait asks, if anything.
    fn look(
        &mut self,
        pane: &Pane,
        wait: &Wait,
        texts: &[String],
        began: Instant,
        now: Instant,
    ) -> Option<Found> {
        let screen = pane.screen();
        // The screen's lines, and any text found in history lines written
        // since the wait began.
        let lines = joined(
            screen
                .window()
                .row(0)
                .into_iter()
                .flat_map(|top| std::iter::successors(Some(top), fux_vt::Row::below)),
        );
        if !texts.is_empty() {
            self.read_history(screen, texts);
        }
        for until in &wait.until {
            let found = match until {
                Until::Text(text) => {
                    let i = texts.iter().position(|t| t == text).unwrap_or(0);
                    let (scrolled, scrolled_line) =
                        self.in_history.get(i).cloned().unwrap_or_default();
                    let seen = count(&lines, text).saturating_add(scrolled);
                    let base = if wait.after {
                        self.before.get(i).copied().unwrap_or(0)
                    } else {
                        0
                    };
                    (seen > base).then(|| Found::Text {
                        text: text.clone(),
                        line: lines
                            .iter()
                            .rev()
                            .find(|l| l.contains(text.as_str()))
                            .cloned()
                            .or(scrolled_line)
                            .unwrap_or_else(|| text.clone()),
                    })
                }
                Until::Gone(text) => (!lines.iter().any(|l| l.contains(text.as_str())))
                    .then(|| Found::Gone(text.clone())),
                Until::Idle(idle) => {
                    let quiet = crate::after(pane.changed_at.max(began), *idle);
                    (pane.input_written() && now >= quiet).then_some(Found::Idle {
                        changed: pane.revision != self.revision,
                    })
                }
                Until::Change(since) => {
                    let past = since.unwrap_or(self.revision);
                    (pane.revision > past).then_some(Found::Change(pane.revision))
                }
                Until::Exit => pane.process.exit_status().map(Found::Exit),
                Until::Done => {
                    let commands = screen.commands();
                    (commands.finished > self.finished).then_some(Found::Done(commands.last_status))
                }
                Until::Bell => (pane.alerts.count() > self.alerts).then(|| {
                    Found::Bell(
                        pane.alerts
                            .last
                            .clone()
                            .filter(|_| pane.alerts.notifications > 0),
                    )
                }),
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// Counts the texts in history rows written since the wait began and
    /// not read yet: from the anchor, or the last row read, to the screen.
    fn read_history(&mut self, screen: &fux_vt::Screen, texts: &[String]) {
        let history = screen.history_len();
        let from = match (self.read, self.anchor) {
            (Some(read), _) => screen.row_index(read).map(|i| i.saturating_add(1)),
            (None, Some(anchor)) => screen.row_index(anchor),
            (None, None) => Some(history),
        };
        // An anchor gone past the history's limit: what is kept is read.
        let from = from.unwrap_or(0);
        if from >= history {
            return;
        }
        let rows: Vec<fux_vt::Row<'_>> = screen
            .rows()
            .skip(from)
            .take(history.saturating_sub(from))
            .collect();
        if let Some(last) = rows.last() {
            self.read = Some(last.id());
        }
        let lines = joined(rows.into_iter());
        for ((seen, line), text) in self.in_history.iter_mut().zip(texts) {
            *seen = seen.saturating_add(count(&lines, text));
            if line.is_none() {
                *line = lines.iter().find(|l| l.contains(text.as_str())).cloned();
            }
        }
    }
}

/// The texts a wait looks for, `--text` conditions', in order.
fn texts(wait: &Wait) -> Vec<String> {
    wait.until
        .iter()
        .filter_map(|u| match u {
            Until::Text(text) => Some(text.clone()),
            Until::Gone(_)
            | Until::Idle(_)
            | Until::Change(_)
            | Until::Exit
            | Until::Done
            | Until::Bell => None,
        })
        .collect()
}

/// Rows as lines: soft-wrapped rows joined into the lines they wrap.
fn joined<'a>(rows: impl Iterator<Item = fux_vt::Row<'a>>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut wraps = false;
    for row in rows {
        let text = crate::capture::row_text(row);
        match lines.last_mut() {
            Some(last) if wraps => last.push_str(&text),
            _ => lines.push(text),
        }
        wraps = row.wrapped();
    }
    lines
}

/// How many times `text` is in `lines`.
fn count(lines: &[String], text: &str) -> usize {
    if text.is_empty() {
        return 1;
    }
    lines.iter().map(|l| l.matches(text).count()).sum()
}

/// A pane's screen as a wait prints it: trimmed, soft-wrapped rows joined.
fn text(pane: &Pane) -> String {
    let view = CaptureView {
        join: true,
        ..CaptureView::default()
    };
    crate::capture::pane(pane, None, view, false, false)
}
